"use strict";

// RugCall transaction signing and submission.
//
// Zero-dependency Ed25519 signing and legacy Solana transaction
// assembly for the resolver's live mode. The signer is built from a
// 64-byte keypair file (32-byte seed followed by the 32-byte public
// key, the standard Solana keypair layout) using node:crypto: the
// seed is wrapped in the PKCS8 DER prefix for Ed25519 and imported
// as a private key. The submitter compiles a legacy (non-versioned)
// message, signs it with the resolver key, hands the raw bytes to
// the injected chain seam, and polls the signature status until it
// is confirmed or the poll budget runs out.

const fs = require("fs");
const crypto = require("crypto");

const BASE58_ALPHABET =
  "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const BASE58_INDEX = {};
for (let i = 0; i < BASE58_ALPHABET.length; i += 1) {
  BASE58_INDEX[BASE58_ALPHABET[i]] = i;
}

function base58Encode(bytes) {
  const buf = Buffer.from(bytes);
  if (buf.length === 0) return "";
  let zeros = 0;
  while (zeros < buf.length && buf[zeros] === 0) zeros += 1;
  // Base conversion via a digit array, most significant last.
  const digits = [0];
  for (let i = zeros; i < buf.length; i += 1) {
    let carry = buf[i];
    for (let j = 0; j < digits.length; j += 1) {
      carry += digits[j] * 256;
      digits[j] = carry % 58;
      carry = Math.floor(carry / 58);
    }
    while (carry > 0) {
      digits.push(carry % 58);
      carry = Math.floor(carry / 58);
    }
  }
  let out = "";
  for (let i = 0; i < zeros; i += 1) out += "1";
  if (!(digits.length === 1 && digits[0] === 0)) {
    for (let i = digits.length - 1; i >= 0; i -= 1) {
      out += BASE58_ALPHABET[digits[i]];
    }
  }
  return out;
}

function base58Decode(text) {
  if (typeof text !== "string" || text.length === 0) {
    throw new Error("base58 value must be a non-empty string");
  }
  let zeros = 0;
  while (zeros < text.length && text[zeros] === "1") zeros += 1;
  const bytes = [0];
  for (let i = zeros; i < text.length; i += 1) {
    const digit = BASE58_INDEX[text[i]];
    if (digit === undefined) {
      throw new Error(`invalid base58 character ${JSON.stringify(text[i])}`);
    }
    let carry = digit;
    for (let j = 0; j < bytes.length; j += 1) {
      carry += bytes[j] * 58;
      bytes[j] = carry % 256;
      carry = Math.floor(carry / 256);
    }
    while (carry > 0) {
      bytes.push(carry % 256);
      carry = Math.floor(carry / 256);
    }
  }
  const body =
    bytes.length === 1 && bytes[0] === 0
      ? Buffer.alloc(0)
      : Buffer.from(bytes.reverse());
  return Buffer.concat([Buffer.alloc(zeros), body]);
}

// PKCS8 DER prefix for an Ed25519 private key holding a 32-byte seed:
// SEQUENCE / version / algorithm OID 1.3.101.112 / OCTET STRING wrap.
const ED25519_PKCS8_PREFIX = Buffer.from(
  "302e020100300506032b657004220420",
  "hex"
);

function parseKeypairBytes(raw, label) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new Error(`keypair at ${label} is not valid JSON: ${err.message}`);
  }
  if (
    !Array.isArray(parsed) ||
    parsed.length !== 64 ||
    !parsed.every(
      (b) => Number.isInteger(b) && b >= 0 && b <= 255
    )
  ) {
    throw new Error(
      `keypair at ${label} is not a 64-byte JSON byte array`
    );
  }
  return Buffer.from(parsed);
}

function signerFromBytes(keypairBytes) {
  const bytes = Buffer.from(keypairBytes);
  if (bytes.length !== 64) {
    throw new Error("keypair must be exactly 64 bytes");
  }
  const seed = bytes.subarray(0, 32);
  const pubkeyBytes = Buffer.from(bytes.subarray(32, 64));
  const privateKey = crypto.createPrivateKey({
    key: Buffer.concat([ED25519_PKCS8_PREFIX, seed]),
    format: "der",
    type: "pkcs8",
  });
  // The stored second half is a claim, not a fact: derive the public
  // key from the seed and refuse a file whose halves do not belong
  // together -- signing with the seed while reporting the stored
  // pubkey would build transactions for the wrong account.
  const derivedJwk = crypto
    .createPublicKey(privateKey)
    .export({ format: "jwk" });
  const derivedPubkeyBytes = Buffer.from(derivedJwk.x, "base64url");
  if (!derivedPubkeyBytes.equals(pubkeyBytes)) {
    throw new Error(
      "keypair public key does not match the public key derived from its seed"
    );
  }
  return {
    pubkey: base58Encode(pubkeyBytes),
    pubkeyBytes,
    sign(msgBytes) {
      return crypto.sign(null, Buffer.from(msgBytes), privateKey);
    },
  };
}

function loadResolverSigner(keypairPath) {
  let raw;
  try {
    raw = fs.readFileSync(keypairPath, "utf8");
  } catch (err) {
    throw new Error(`cannot read keypair at ${keypairPath}: ${err.message}`);
  }
  return signerFromBytes(parseKeypairBytes(raw, keypairPath));
}

function encodeShortvec(value) {
  if (!Number.isInteger(value) || value < 0) {
    throw new Error(`shortvec value must be a non-negative integer, got ${value}`);
  }
  const out = [];
  let v = value;
  for (;;) {
    const byte = v % 128;
    v = Math.floor(v / 128);
    if (v === 0) {
      out.push(byte);
      break;
    }
    out.push(byte | 0x80);
  }
  return Buffer.from(out);
}

// Compile one or more instructions into a legacy message. The fee
// payer (the resolver signer) is always the first account key and
// always a writable signer, whatever the instruction metas say.
// Remaining keys keep first-appearance order inside each privilege
// class: writable signers, read-only signers, writable non-signers,
// read-only non-signers. Program ids are read-only non-signers.
function buildLegacyMessage({ feePayer, recentBlockhash, instructions }) {
  if (!feePayer) throw new Error("buildLegacyMessage requires a feePayer");
  if (!Array.isArray(instructions) || instructions.length === 0) {
    throw new Error("buildLegacyMessage requires at least one instruction");
  }
  const metas = new Map();
  const addMeta = (pubkey, signer, writable) => {
    const existing = metas.get(pubkey);
    if (existing) {
      existing.signer = existing.signer || signer;
      existing.writable = existing.writable || writable;
    } else {
      metas.set(pubkey, { pubkey, signer, writable });
    }
  };
  addMeta(feePayer, true, true);
  for (const ix of instructions) {
    for (const acct of ix.accounts || []) {
      addMeta(acct.pubkey, !!acct.signer, !!acct.writable);
    }
    addMeta(ix.programId, false, false);
  }
  const all = [...metas.values()];
  const ordered = [
    ...all.filter((m) => m.signer && m.writable),
    ...all.filter((m) => m.signer && !m.writable),
    ...all.filter((m) => !m.signer && m.writable),
    ...all.filter((m) => !m.signer && !m.writable),
  ];
  const indexOf = new Map(ordered.map((m, i) => [m.pubkey, i]));
  const numRequiredSignatures = ordered.filter((m) => m.signer).length;
  const numReadonlySignedAccounts = ordered.filter(
    (m) => m.signer && !m.writable
  ).length;
  const numReadonlyUnsignedAccounts = ordered.filter(
    (m) => !m.signer && !m.writable
  ).length;

  const parts = [
    Buffer.from([
      numRequiredSignatures,
      numReadonlySignedAccounts,
      numReadonlyUnsignedAccounts,
    ]),
    encodeShortvec(ordered.length),
  ];
  for (const meta of ordered) {
    const keyBytes = base58Decode(meta.pubkey);
    if (keyBytes.length !== 32) {
      throw new Error(`account key ${meta.pubkey} is not 32 bytes`);
    }
    parts.push(keyBytes);
  }
  const blockhashBytes = base58Decode(recentBlockhash);
  if (blockhashBytes.length !== 32) {
    throw new Error("recent blockhash is not 32 bytes");
  }
  parts.push(blockhashBytes);
  parts.push(encodeShortvec(instructions.length));
  for (const ix of instructions) {
    const data = ix.data
      ? Buffer.from(ix.data)
      : Buffer.from(ix.dataHex || "", "hex");
    const acctIndices = (ix.accounts || []).map((a) => indexOf.get(a.pubkey));
    parts.push(
      Buffer.from([indexOf.get(ix.programId)]),
      encodeShortvec(acctIndices.length),
      Buffer.from(acctIndices),
      encodeShortvec(data.length),
      data
    );
  }
  return {
    messageBytes: Buffer.concat(parts),
    accountKeys: ordered.map((m) => m.pubkey),
    numRequiredSignatures,
  };
}

function sleepMs(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function statusIsConfirmed(status) {
  if (!status || typeof status !== "object") return false;
  if (status.err) return false;
  if (status.confirmed === true) return true;
  return (
    status.confirmationStatus === "confirmed" ||
    status.confirmationStatus === "finalized"
  );
}

function createSubmitter(options) {
  const opts = options || {};
  const chain = opts.chain;
  const signer = opts.signer;
  if (!chain) throw new Error("createSubmitter requires a chain seam");
  if (!signer) throw new Error("createSubmitter requires a signer");
  const pollAttempts =
    opts.pollAttempts !== undefined ? opts.pollAttempts : 15;
  const pollIntervalMs =
    opts.pollIntervalMs !== undefined ? opts.pollIntervalMs : 400;
  const sleep = opts.sleep || sleepMs;

  async function submitInstruction({ programId, instruction, recentBlockhash }) {
    if (!programId) throw new Error("submitInstruction requires a programId");
    if (!instruction) throw new Error("submitInstruction requires an instruction");
    let blockhash = recentBlockhash;
    if (!blockhash) {
      if (typeof chain.getLatestBlockhash !== "function") {
        throw new Error("no recent blockhash supplied and chain cannot provide one");
      }
      const latest = await chain.getLatestBlockhash();
      blockhash =
        typeof latest === "string" ? latest : latest && latest.blockhash;
    }
    if (!blockhash) throw new Error("could not obtain a recent blockhash");
    const ix = {
      programId,
      accounts: instruction.accounts || [],
      dataHex: instruction.dataHex || instruction.data_hex || "",
    };
    const { messageBytes, numRequiredSignatures } = buildLegacyMessage({
      feePayer: signer.pubkey,
      recentBlockhash: blockhash,
      instructions: [ix],
    });
    // Signer slots come first in the transaction. Only the resolver
    // key is held here; any other required signer slot is left as a
    // zero signature, which the cluster would reject -- the resolver
    // instructions never need a second signer.
    const signatures = [];
    for (let i = 0; i < numRequiredSignatures; i += 1) {
      signatures.push(Buffer.alloc(64));
    }
    signatures[0] = signer.sign(messageBytes);
    const txBytes = Buffer.concat([
      encodeShortvec(signatures.length),
      ...signatures,
      messageBytes,
    ]);
    const signature = await chain.sendRawTransaction(txBytes);
    if (!signature || typeof signature !== "string") {
      throw new Error("chain did not return a signature for the transaction");
    }
    // From here on the transaction may already have landed, so the
    // signature must never be lost to a throw: any polling error or
    // on-chain failure is rethrown with the signature attached, and
    // the caller persists it as an unconfirmed submission instead of
    // recording a signature-less failure that would resubmit blind.
    if (typeof chain.getSignatureStatus === "function") {
      try {
        for (let attempt = 0; attempt < pollAttempts; attempt += 1) {
          const status = await chain.getSignatureStatus(signature);
          if (status && status.err) {
            throw new Error(
              `transaction ${signature} failed on chain: ${JSON.stringify(status.err)}`
            );
          }
          if (statusIsConfirmed(status)) {
            return { signature, confirmed: true };
          }
          if (attempt + 1 < pollAttempts) await sleep(pollIntervalMs);
        }
      } catch (err) {
        if (err && typeof err === "object" && !err.signature) {
          err.signature = signature;
        }
        throw err;
      }
      return { signature, confirmed: false };
    }
    return { signature, confirmed: false };
  }

  return { submitInstruction };
}

module.exports = {
  base58Encode,
  base58Decode,
  parseKeypairBytes,
  signerFromBytes,
  loadResolverSigner,
  encodeShortvec,
  buildLegacyMessage,
  createSubmitter,
};
