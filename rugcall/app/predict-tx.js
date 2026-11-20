"use strict";

// RugCall transaction layer (pure).
//
// This module builds RugCall instruction data, account lists, program
// derived addresses and advisory preflight results. It has no DOM, no
// wallet and no network access: callers pass addresses, state and a
// clock in, and receive plain descriptors and result objects back.
// Amounts are BigInt internally and decimal strings at the boundary;
// a JavaScript number is never accepted for a u64 value.
//
// Fixture mode is a local simulation against in-memory state. Its
// results are labelled simulated and carry no signature and no
// explorer link, because nothing is signed or submitted in that mode.

var RugCallTx = (function () {
  var TOKEN_PROGRAM_ID = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
  // ATA program id, pinned from the vendored SPL sources in the local
  // cargo registry: spl-associated-token-account-client-2.0.0/src/lib.rs
  // declares `solana_pubkey::declare_id!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")`,
  // and spl-associated-token-account 6.0.0 / 7.0.0 re-export that id.
  var ATA_PROGRAM_ID = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
  var SYSTEM_PROGRAM_ID = "11111111111111111111111111111111";

  var PROGRAM_IDS = {
    tokenProgram: TOKEN_PROGRAM_ID,
    tokenProgramId: TOKEN_PROGRAM_ID,
    systemProgram: SYSTEM_PROGRAM_ID,
    systemProgramId: SYSTEM_PROGRAM_ID,
    ataProgram: ATA_PROGRAM_ID,
    ataProgramId: ATA_PROGRAM_ID,
  };

  var SEED_CONFIG = "config";
  var SEED_MARKET = "market";
  var SEED_VAULT = "vault";
  var SEED_POSITION = "position";
  var SEED_LIVE_MARKET = "live-market";

  var STATE_OPEN = 1;
  var STATE_RESOLVED = 2;
  var STATE_CANCELLED = 3;
  var SIDE_FLAGGED = 1;
  var SIDE_SURVIVES = 2;
  var MIN_STAKE_RAW = "1000000";
  var DEFAULT_DURATION_SECS = 86400;
  var CANCEL_GRACE_SECS = 86400;
  var U64_MAX = (1n << 64n) - 1n;
  var I64_MAX = (1n << 63n) - 1n;

  var ORDINALS = {
    InitializeConfig: 0,
    CreateMarket: 1,
    BuyPosition: 2,
    ResolveMarket: 3,
    Claim: 4,
    CancelMarket: 5,
  };

  // RugCall custom error numbers, mirrored from the program enum.
  var ERRORS = {
    ConfigAlreadyInitialized: 1,
    ConfigNotInitialized: 2,
    NotUpgradeAuthority: 3,
    InvalidPda: 4,
    MintMismatch: 5,
    UnsupportedTokenProgram: 6,
    DuplicateLiveMarket: 7,
    InvalidSide: 8,
    BelowMinStake: 9,
    MarketNotOpen: 10,
    BuyAfterExpiry: 11,
    NotResolver: 12,
    AlreadyResolved: 13,
    AlreadyCancelled: 14,
    InvalidHardTrigger: 15,
    EarlySurvivesResolution: 16,
    ClaimBeforeResolve: 17,
    AlreadyClaimed: 18,
    NoClaimableStake: 19,
    CancelTooEarly: 20,
    Overflow: 21,
    NotImplemented: 22,
    InvalidEvidenceHash: 23,
  };

  function fail(code, message) {
    var err = new Error(message || code);
    err.code = code;
    return err;
  }

  function utf8Bytes(text) {
    if (typeof TextEncoder !== "undefined") {
      return new TextEncoder().encode(text);
    }
    var out = [];
    for (var i = 0; i < text.length; i++) {
      var c = text.charCodeAt(i);
      if (c < 0x80) out.push(c);
      else if (c < 0x800) {
        out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
      } else {
        out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
      }
    }
    return Uint8Array.from(out);
  }

  function concatBytes(parts) {
    var total = 0;
    var i;
    for (i = 0; i < parts.length; i++) total += parts[i].length;
    var out = new Uint8Array(total);
    var off = 0;
    for (i = 0; i < parts.length; i++) {
      out.set(parts[i], off);
      off += parts[i].length;
    }
    return out;
  }

  function bytesToHex(bytes) {
    var s = "";
    for (var i = 0; i < bytes.length; i++) {
      s += bytes[i].toString(16).padStart(2, "0");
    }
    return s;
  }

  function hexToBytes(hex) {
    if (typeof hex !== "string" || hex.length % 2 !== 0 || /[^0-9a-fA-F]/.test(hex)) {
      throw fail("InvalidHex", "expected an even-length hex string");
    }
    var out = new Uint8Array(hex.length / 2);
    for (var i = 0; i < out.length; i++) {
      out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
    }
    return out;
  }

  var BASE58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  var BASE58_INDEX = {};
  for (var b58i = 0; b58i < BASE58_ALPHABET.length; b58i++) {
    BASE58_INDEX[BASE58_ALPHABET[b58i]] = b58i;
  }

  function base58Encode(bytes) {
    if (!bytes || bytes.length === 0) return "";
    var input = bytes instanceof Uint8Array ? bytes : Uint8Array.from(bytes);
    var zeros = 0;
    while (zeros < input.length && input[zeros] === 0) zeros++;
    if (zeros === input.length) return "1".repeat(zeros);
    var digits = [0];
    for (var i = zeros; i < input.length; i++) {
      var carry = input[i];
      for (var j = 0; j < digits.length; j++) {
        carry += digits[j] << 8;
        digits[j] = carry % 58;
        carry = Math.floor(carry / 58);
      }
      while (carry > 0) {
        digits.push(carry % 58);
        carry = Math.floor(carry / 58);
      }
    }
    var out = "1".repeat(zeros);
    for (var k = digits.length - 1; k >= 0; k--) out += BASE58_ALPHABET[digits[k]];
    return out;
  }

  function base58Decode(text) {
    if (typeof text !== "string") {
      throw fail("InvalidBase58", "base58 input must be a string");
    }
    if (text.length === 0) return new Uint8Array(0);
    var zeros = 0;
    while (zeros < text.length && text[zeros] === "1") zeros++;
    if (zeros === text.length) return new Uint8Array(zeros);
    var bytes = [0];
    for (var i = zeros; i < text.length; i++) {
      var ch = text[i];
      var value = BASE58_INDEX[ch];
      if (value === undefined) {
        throw fail("InvalidBase58", "invalid base58 character " + JSON.stringify(ch));
      }
      var carry = value;
      for (var j = 0; j < bytes.length; j++) {
        carry += bytes[j] * 58;
        bytes[j] = carry & 0xff;
        carry >>= 8;
      }
      while (carry > 0) {
        bytes.push(carry & 0xff);
        carry >>= 8;
      }
    }
    var out = new Uint8Array(zeros + bytes.length);
    for (var k = 0; k < bytes.length; k++) {
      out[zeros + bytes.length - 1 - k] = bytes[k];
    }
    return out;
  }

  // SHA-256 (pure, synchronous). Program derived addresses are a
  // synchronous computation, so the asynchronous platform digest API
  // cannot be used here. Direct FIPS 180-4 implementation.
  var SHA256_K = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
  ];
  var SHA256_H0 = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ];

  function rotr32(x, n) {
    return ((x >>> n) | (x << (32 - n))) >>> 0;
  }

  function sha256Bytes(input) {
    var bytes;
    if (typeof input === "string") bytes = utf8Bytes(input);
    else if (input instanceof Uint8Array) bytes = input;
    else if (Array.isArray(input)) bytes = Uint8Array.from(input);
    else throw fail("InvalidSha256Input", "sha256 input must be bytes or a string");

    var bitLen = BigInt(bytes.length) * 8n;
    var paddedLen = Math.ceil((bytes.length + 1 + 8) / 64) * 64;
    var padded = new Uint8Array(paddedLen);
    padded.set(bytes, 0);
    padded[bytes.length] = 0x80;
    var lenView = new DataView(padded.buffer);
    lenView.setUint32(paddedLen - 4, Number(bitLen & 0xffffffffn));
    lenView.setUint32(paddedLen - 8, Number((bitLen >> 32n) & 0xffffffffn));

    var h = SHA256_H0.slice();
    var w = new Array(64);
    for (var block = 0; block < paddedLen; block += 64) {
      var t;
      for (t = 0; t < 16; t++) {
        w[t] = lenView.getUint32(block + t * 4);
      }
      for (t = 16; t < 64; t++) {
        var s0 = (rotr32(w[t - 15], 7) ^ rotr32(w[t - 15], 18) ^ (w[t - 15] >>> 3)) >>> 0;
        var s1 = (rotr32(w[t - 2], 17) ^ rotr32(w[t - 2], 19) ^ (w[t - 2] >>> 10)) >>> 0;
        w[t] = (w[t - 16] + s0 + w[t - 7] + s1) >>> 0;
      }
      var a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], hh = h[7];
      for (t = 0; t < 64; t++) {
        var bigS1 = (rotr32(e, 6) ^ rotr32(e, 11) ^ rotr32(e, 25)) >>> 0;
        var ch = ((e & f) ^ (~e & g)) >>> 0;
        var temp1 = (hh + bigS1 + ch + SHA256_K[t] + w[t]) >>> 0;
        var bigS0 = (rotr32(a, 2) ^ rotr32(a, 13) ^ rotr32(a, 22)) >>> 0;
        var maj = ((a & b) ^ (a & c) ^ (b & c)) >>> 0;
        var temp2 = (bigS0 + maj) >>> 0;
        hh = g; g = f; f = e; e = (d + temp1) >>> 0;
        d = c; c = b; b = a; a = (temp1 + temp2) >>> 0;
      }
      h[0] = (h[0] + a) >>> 0; h[1] = (h[1] + b) >>> 0;
      h[2] = (h[2] + c) >>> 0; h[3] = (h[3] + d) >>> 0;
      h[4] = (h[4] + e) >>> 0; h[5] = (h[5] + f) >>> 0;
      h[6] = (h[6] + g) >>> 0; h[7] = (h[7] + hh) >>> 0;
    }
    var out = new Uint8Array(32);
    var outView = new DataView(out.buffer);
    for (var i = 0; i < 8; i++) outView.setUint32(i * 4, h[i]);
    return out;
  }

  function sha256Hex(input) {
    return bytesToHex(sha256Bytes(input));
  }

  // Ed25519 curve membership. A hash is a usable derived address
  // exactly when it does NOT decompress to a point on the curve.
  // Decompression: the low 255 bits are y, the top bit is the sign
  // of x, and x^2 = (y^2 - 1) / (d * y^2 + 1), d = -121665/121666
  // mod p. x exists exactly when x^2 is a quadratic residue
  // (Legendre symbol 1), or x^2 is zero with a non-negative sign
  // bit; a zero x with the sign bit set is the one encoding
  // decompression rejects even though the residue test alone
  // would accept it.
  var FIELD_P = (1n << 255n) - 19n;

  function modP(x) {
    var r = x % FIELD_P;
    return r >= 0n ? r : r + FIELD_P;
  }

  function modPow(base, exp, mod) {
    var result = 1n;
    var b = ((base % mod) + mod) % mod;
    var e = exp;
    while (e > 0n) {
      if (e & 1n) result = (result * b) % mod;
      b = (b * b) % mod;
      e >>= 1n;
    }
    return result;
  }

  var FIELD_D = modP(-121665n * modPow(121666n, FIELD_P - 2n, FIELD_P));

  function isOnCurve(bytes) {
    if (!bytes || bytes.length !== 32) {
      throw fail("InvalidPointLength", "a curve encoding is exactly 32 bytes");
    }
    var sign = (bytes[31] >> 7) & 1;
    var y = 0n;
    for (var i = 31; i >= 0; i--) {
      var byte = bytes[i];
      if (i === 31) byte &= 0x7f;
      y = (y << 8n) | BigInt(byte);
    }
    if (y >= FIELD_P) return false;
    var y2 = (y * y) % FIELD_P;
    var denom = modP(FIELD_D * y2 + 1n);
    if (denom === 0n) return false;
    var x2 = (modP(y2 - 1n) * modPow(denom, FIELD_P - 2n, FIELD_P)) % FIELD_P;
    if (x2 === 0n) return sign === 0;
    var legendre = modPow(x2, (FIELD_P - 1n) / 2n, FIELD_P);
    return legendre === 1n;
  }

  function pubkeyBytes(input) {
    if (typeof input === "string") return base58Decode(input);
    if (input instanceof Uint8Array) return Uint8Array.from(input);
    if (Array.isArray(input)) return Uint8Array.from(input);
    throw fail("InvalidPubkey", "a public key must be a base58 string or 32 bytes");
  }

  function pubkeyBytes32(input, lengthCode) {
    var bytes = pubkeyBytes(input);
    if (bytes.length !== 32) {
      throw fail(lengthCode || "InvalidPubkeyLength",
        "expected exactly 32 bytes, got " + bytes.length);
    }
    return bytes;
  }

  function seedBytes(seed) {
    if (typeof seed === "string") return utf8Bytes(seed);
    if (seed instanceof Uint8Array) return seed;
    if (Array.isArray(seed)) return Uint8Array.from(seed);
    throw fail("InvalidSeed", "a seed must be a string or bytes");
  }

  function findProgramAddress(seeds, programId) {
    if (!Array.isArray(seeds)) throw fail("InvalidSeed", "seeds must be an array");
    if (seeds.length > 16) {
      throw fail("TooManySeeds", "at most 16 seeds are allowed, got " + seeds.length);
    }
    var seedList = seeds.map(function (seed) {
      var bytes = seedBytes(seed);
      if (bytes.length > 32) {
        throw fail("SeedTooLong", "a seed is at most 32 bytes, got " + bytes.length);
      }
      return bytes;
    });
    var programBytes = pubkeyBytes32(programId, "InvalidProgramIdLength");
    var marker = utf8Bytes("ProgramDerivedAddress");
    for (var bump = 255; bump >= 0; bump--) {
      var parts = seedList.concat([Uint8Array.from([bump]), programBytes, marker]);
      var hash = sha256Bytes(concatBytes(parts));
      if (!isOnCurve(hash)) {
        return { address: base58Encode(hash), bump: bump, bytes: hash };
      }
    }
    throw fail("NoValidBump", "no bump produced an off-curve address");
  }

  function i64LeBytes(value) {
    var n;
    if (typeof value === "bigint") n = value;
    else if (typeof value === "number" && Number.isSafeInteger(value)) n = BigInt(value);
    else if (typeof value === "string" && /^-?\d+$/.test(value.trim())) n = BigInt(value.trim());
    else throw fail("AmountType", "a timestamp must be an integer number, string or bigint");
    var out = new Uint8Array(8);
    new DataView(out.buffer).setBigInt64(0, n, true);
    return out;
  }

  function deriveConfig(programId) {
    return findProgramAddress([SEED_CONFIG], programId);
  }

  function deriveLiveMarket(programId, subjectMint) {
    return findProgramAddress(
      [SEED_LIVE_MARKET, pubkeyBytes32(subjectMint, "InvalidMintLength")], programId);
  }

  function deriveMarket(programId, subjectMint, createdAtSec) {
    return findProgramAddress(
      [SEED_MARKET, pubkeyBytes32(subjectMint, "InvalidMintLength"), i64LeBytes(createdAtSec)],
      programId);
  }

  function deriveVault(programId, market) {
    return findProgramAddress(
      [SEED_VAULT, pubkeyBytes32(market, "InvalidPubkeyLength")], programId);
  }

  function derivePosition(programId, market, user) {
    return findProgramAddress(
      [SEED_POSITION, pubkeyBytes32(market, "InvalidPubkeyLength"),
       pubkeyBytes32(user, "InvalidPubkeyLength")],
      programId);
  }

  function deriveAssociatedTokenAddress(user, mint, tokenProgramId, ataProgramId) {
    var tokenId = tokenProgramId || TOKEN_PROGRAM_ID;
    var ataId = ataProgramId || ATA_PROGRAM_ID;
    return findProgramAddress(
      [pubkeyBytes32(user, "InvalidPubkeyLength"),
       pubkeyBytes32(tokenId, "InvalidProgramIdLength"),
       pubkeyBytes32(mint, "InvalidMintLength")],
      ataId);
  }

  function parseU64(value, label) {
    if (typeof value === "bigint") return value;
    if (typeof value === "string") {
      var trimmed = value.trim();
      if (!/^\d+$/.test(trimmed)) {
        throw fail("AmountType", (label || "amount") + " must be a decimal string or bigint");
      }
      return BigInt(trimmed);
    }
    throw fail("AmountType",
      (label || "amount") + " must be a decimal string or bigint, never a JavaScript number");
  }

  function parseSignedBigInt(value, label) {
    if (typeof value === "bigint") return value;
    if (typeof value === "string") {
      var trimmed = value.trim();
      if (!/^-?\d+$/.test(trimmed)) {
        throw fail("AmountType", (label || "value") + " must be a decimal string or bigint");
      }
      return BigInt(trimmed);
    }
    throw fail("AmountType",
      (label || "value") + " must be a decimal string or bigint, never a JavaScript number");
  }

  // Exact mirror of the program helper: null when the winning pool
  // is zero, when the inputs are inconsistent, or when the true
  // result does not fit in u64; otherwise the floored pro-rata share
  // as a decimal string. This is the one implementation of the
  // formula in the client layers; other modules consume it from here.
  function parimutuelPayout(winnerStake, totalPool, winningPool) {
    var stake = parseSignedBigInt(winnerStake, "winnerStake");
    var total = parseSignedBigInt(totalPool, "totalPool");
    var winning = parseSignedBigInt(winningPool, "winningPool");
    if (stake < 0n || total < 0n || winning < 0n) return null;
    if (winning === 0n || stake > winning || total < winning) return null;
    var payout = (stake * total) / winning;
    if (payout > U64_MAX) return null;
    return payout.toString();
  }

  function u64LeBytes(value) {
    var out = new Uint8Array(8);
    new DataView(out.buffer).setBigUint64(0, value, true);
    return out;
  }

  function checkAmountRange(amount) {
    if (amount < 0n || amount > U64_MAX) {
      var err = new RangeError("amount is outside the u64 range");
      err.code = "AmountOutOfRange";
      throw err;
    }
    return amount;
  }

  function encodeCreateMarket(subjectMint) {
    var mintBytes = pubkeyBytes32(subjectMint, "InvalidMintLength");
    var out = new Uint8Array(33);
    out[0] = ORDINALS.CreateMarket;
    out.set(mintBytes, 1);
    return out;
  }

  function encodeBuyPosition(side, amount) {
    if (side !== SIDE_FLAGGED && side !== SIDE_SURVIVES) {
      throw fail("InvalidSide", "side must be 1 (Flagged) or 2 (Survives)");
    }
    var value = checkAmountRange(parseU64(amount, "amount"));
    var out = new Uint8Array(10);
    out[0] = ORDINALS.BuyPosition;
    out[1] = side;
    out.set(u64LeBytes(value), 2);
    return out;
  }

  function encodeResolveMarket(outcome, hardTrigger, evidenceHash) {
    if (outcome !== SIDE_FLAGGED && outcome !== SIDE_SURVIVES) {
      throw fail("InvalidSide", "outcome must be 1 (Flagged) or 2 (Survives)");
    }
    if (!Number.isInteger(hardTrigger) || hardTrigger < 0 || hardTrigger > 255) {
      throw fail("InvalidHardTrigger", "hard trigger must be a byte");
    }
    var hashBytes;
    if (typeof evidenceHash === "string") hashBytes = hexToBytes(evidenceHash);
    else if (evidenceHash instanceof Uint8Array) hashBytes = evidenceHash;
    else throw fail("InvalidEvidenceHash", "evidence hash must be 32 bytes or a hex string");
    if (hashBytes.length !== 32) {
      throw fail("InvalidEvidenceHash", "evidence hash must be exactly 32 bytes");
    }
    var out = new Uint8Array(35);
    out[0] = ORDINALS.ResolveMarket;
    out[1] = outcome;
    out[2] = hardTrigger;
    out.set(hashBytes, 3);
    return out;
  }

  function encodeClaim() {
    return Uint8Array.from([ORDINALS.Claim]);
  }

  function encodeCancelMarket() {
    return Uint8Array.from([ORDINALS.CancelMarket]);
  }

  // Account lists and instruction descriptors. The flags below are
  // the program test builders' flags, account by account. A
  // descriptor is plain data: { programId, keys, data }.
  function addressString(input) {
    if (typeof input === "string") return input;
    if (input instanceof Uint8Array) {
      if (input.length !== 32) {
        throw fail("InvalidPubkeyLength", "expected exactly 32 bytes, got " + input.length);
      }
      return base58Encode(input);
    }
    if (input && typeof input === "object") {
      if (typeof input.address === "string") return input.address;
      if (typeof input.pubkey === "string") return input.pubkey;
      if (input.bytes instanceof Uint8Array) return base58Encode(input.bytes);
    }
    throw fail("InvalidPubkey", "an address must be a base58 string or 32 bytes");
  }

  function key(pubkey, isSigner, isWritable) {
    return { pubkey: addressString(pubkey), isSigner: !!isSigner, isWritable: !!isWritable };
  }

  function programAddressPart(value, deriveFn) {
    if (value !== undefined && value !== null) return addressString(value);
    if (deriveFn) return deriveFn().address;
    throw fail("MissingAccount", "an account address was not supplied and cannot be derived");
  }

  function tokenProgramOf(opts) {
    return (opts && (opts.tokenProgramId || opts.tokenProgram ||
      (opts.programIds && (opts.programIds.tokenProgramId || opts.programIds.tokenProgram)))) ||
      TOKEN_PROGRAM_ID;
  }

  function systemProgramOf(opts) {
    return (opts && (opts.systemProgramId || opts.systemProgram ||
      (opts.programIds && (opts.programIds.systemProgramId || opts.programIds.systemProgram)))) ||
      SYSTEM_PROGRAM_ID;
  }

  function configAddressOf(opts, programId) {
    if (opts.configAddress) return addressString(opts.configAddress);
    if (typeof opts.config === "string") return opts.config;
    if (opts.config && typeof opts.config === "object" &&
        (opts.config.address || opts.config.pubkey)) {
      return addressString(opts.config);
    }
    if (programId) return deriveConfig(programId).address;
    throw fail("MissingAccount", "config address was not supplied");
  }

  function buildCreateMarketIx(opts) {
    opts = opts || {};
    var programId = addressString(opts.programId);
    var subjectMint = opts.subjectMint || opts.mint;
    var createdAt = opts.createdAtSec !== undefined ? opts.createdAtSec : opts.createdAt;
    var market = programAddressPart(opts.market, function () {
      return deriveMarket(programId, subjectMint, createdAt);
    });
    var liveMarket = programAddressPart(opts.liveMarket || opts.live_market, function () {
      return deriveLiveMarket(programId, subjectMint);
    });
    var vault = programAddressPart(opts.vault, function () {
      return deriveVault(programId, market);
    });
    return {
      programId: programId,
      keys: [
        key(opts.creator, true, true),
        key(configAddressOf(opts, programId), false, false),
        key(market, false, true),
        key(liveMarket, false, true),
        key(vault, false, true),
        key(opts.collateralMint, false, false),
        key(tokenProgramOf(opts), false, false),
        key(systemProgramOf(opts), false, false),
      ],
      data: encodeCreateMarket(subjectMint),
    };
  }

  function buildBuyPositionIx(opts) {
    opts = opts || {};
    var programId = addressString(opts.programId);
    var user = addressString(opts.user);
    var market = addressString(opts.market);
    var vault = programAddressPart(opts.vault, function () {
      return deriveVault(programId, market);
    });
    var position = programAddressPart(opts.position, function () {
      return derivePosition(programId, market, user);
    });
    return {
      programId: programId,
      keys: [
        key(user, true, true),
        key(configAddressOf(opts, programId), false, false),
        key(market, false, true),
        key(vault, false, true),
        key(opts.userToken || opts.user_token, false, true),
        key(position, false, true),
        key(tokenProgramOf(opts), false, false),
        key(systemProgramOf(opts), false, false),
      ],
      data: encodeBuyPosition(opts.side, opts.amount),
    };
  }

  function buildClaimIx(opts) {
    opts = opts || {};
    var programId = addressString(opts.programId);
    var user = addressString(opts.user);
    var market = addressString(opts.market);
    var vault = programAddressPart(opts.vault, function () {
      return deriveVault(programId, market);
    });
    var position = programAddressPart(opts.position, function () {
      return derivePosition(programId, market, user);
    });
    return {
      programId: programId,
      keys: [
        key(user, true, false),
        key(configAddressOf(opts, programId), false, false),
        key(market, false, false),
        key(vault, false, true),
        key(opts.userToken || opts.user_token, false, true),
        key(position, false, true),
        key(tokenProgramOf(opts), false, false),
      ],
      data: encodeClaim(),
    };
  }

  function buildCancelMarketIx(opts) {
    opts = opts || {};
    var programId = addressString(opts.programId);
    var market = addressString(opts.market);
    var liveMarket = opts.liveMarket || opts.live_market;
    if (!liveMarket && (opts.subjectMint || opts.mint)) {
      liveMarket = deriveLiveMarket(programId, opts.subjectMint || opts.mint).address;
    }
    if (!liveMarket) throw fail("MissingAccount", "live market address was not supplied");
    return {
      programId: programId,
      keys: [
        key(opts.caller, true, false),
        key(configAddressOf(opts, programId), false, false),
        key(market, false, true),
        key(liveMarket, false, true),
        key(opts.creator, false, true),
      ],
      data: encodeCancelMarket(),
    };
  }

  // Field access for state objects. State records in this project
  // spell u64 fields as decimal strings and use snake_case names;
  // camelCase spellings are accepted as well.
  function field(obj, names) {
    if (!obj || typeof obj !== "object") return undefined;
    for (var i = 0; i < names.length; i++) {
      if (obj[names[i]] !== undefined) return obj[names[i]];
    }
    return undefined;
  }

  function bigIntField(obj, names, fallback) {
    var v = field(obj, names);
    if (v === undefined || v === null) return fallback;
    if (typeof v === "bigint") return v;
    if (typeof v === "number" && Number.isInteger(v)) return BigInt(v);
    if (typeof v === "string" && /^-?\d+$/.test(v.trim())) return BigInt(v.trim());
    return fallback;
  }

  function stateCode(market) {
    var s = field(market, ["state"]);
    if (s === undefined || s === null) return undefined;
    if (typeof s === "number") return s;
    if (s === "Open") return STATE_OPEN;
    if (s === "Resolved") return STATE_RESOLVED;
    if (s === "Cancelled" || s === "Canceled") return STATE_CANCELLED;
    if (typeof s === "string" && /^\d+$/.test(s)) return Number(s);
    return undefined;
  }

  function walletAddressOf(wallet) {
    if (!wallet) return null;
    if (typeof wallet === "string") return wallet;
    var pk = wallet.publicKey;
    if (pk === undefined || pk === null) {
      return typeof wallet.address === "string" ? wallet.address : null;
    }
    if (typeof pk === "string") return pk;
    if (pk instanceof Uint8Array) return base58Encode(pk);
    if (typeof pk.toString === "function") {
      var text = pk.toString();
      return text && text !== "[object Object]" ? text : null;
    }
    return null;
  }

  function walletConnected(ctx) {
    if (ctx.walletConnected !== undefined) return !!ctx.walletConnected;
    if (ctx.connected !== undefined) return !!ctx.connected;
    return !!walletAddressOf(ctx.wallet || ctx.user || ctx.caller);
  }

  function pfError(code, rugCallError, message, extra) {
    var entry = { code: code, rugCallError: rugCallError, message: message };
    if (extra) {
      for (var k in extra) entry[k] = extra[k];
    }
    return entry;
  }

  function pfFail(entry) {
    return { ok: false, errors: [entry] };
  }

  // Advisory preflight. Each check mirrors the program handler's
  // validation order and stops at the first fault, naming it the way
  // the program would. Preflight is advisory only.
  function preflightBuyPosition(ctx) {
    ctx = ctx || {};
    if (!walletConnected(ctx)) {
      return pfFail(pfError("WALLET_NOT_CONNECTED", null, "no wallet is connected"));
    }
    var side = ctx.side;
    if (side !== SIDE_FLAGGED && side !== SIDE_SURVIVES) {
      return pfFail(pfError("InvalidSide", ERRORS.InvalidSide,
        "side must be 1 (Flagged) or 2 (Survives)"));
    }
    var amount;
    try {
      amount = parseU64(ctx.amount, "amount");
    } catch (err) {
      return pfFail(pfError(err.code || "AmountType", null, err.message));
    }
    var config = ctx.config || {};
    var minStakeRaw = field(config, ["minStake", "min_stake", "minStakeRaw", "min_stake_raw"]);
    if (minStakeRaw === undefined) {
      return pfFail(pfError("CONFIG_MISSING_MIN_STAKE", null,
        "the config object does not carry a min stake"));
    }
    var minStake = bigIntField(config,
      ["minStake", "min_stake", "minStakeRaw", "min_stake_raw"], 0n);
    if (amount < minStake) {
      return pfFail(pfError("BelowMinStake", ERRORS.BelowMinStake,
        "amount is below the config minimum stake"));
    }
    var market = ctx.market;
    if (!market || typeof market !== "object") {
      return pfFail(pfError("MARKET_NOT_FOUND", null, "the market does not exist"));
    }
    var marketAddress = field(market, ["address", "pubkey"]);
    if (ctx.marketAddress && marketAddress && ctx.marketAddress !== marketAddress) {
      return pfFail(pfError("InvalidPda", ERRORS.InvalidPda,
        "the market address does not match the supplied address"));
    }
    if (ctx.expectedMarketAddress && marketAddress &&
        ctx.expectedMarketAddress !== marketAddress) {
      return pfFail(pfError("InvalidPda", ERRORS.InvalidPda,
        "the market address does not match the expected address"));
    }
    var subjectMint = field(market, ["subject_mint", "subjectMint"]);
    var createdAt = field(market, ["created_at", "createdAt"]);
    if (ctx.programId && subjectMint && createdAt !== undefined && marketAddress) {
      var derived = null;
      try {
        derived = deriveMarket(ctx.programId, subjectMint, createdAt);
      } catch (err) {
        derived = null;
      }
      if (derived && derived.address !== marketAddress) {
        return pfFail(pfError("InvalidPda", ERRORS.InvalidPda,
          "the market address is not the address its stored fields derive"));
      }
    }
    var code = stateCode(market);
    if (code !== undefined && code !== STATE_OPEN) {
      return pfFail(pfError("MarketNotOpen", ERRORS.MarketNotOpen,
        "the market is not open"));
    }
    var nowSec = ctx.nowSec !== undefined ? ctx.nowSec : ctx.now;
    var expiry = bigIntField(market, ["expiry"], null);
    if (nowSec !== undefined && expiry !== null) {
      if (!(BigInt(Math.trunc(nowSec)) < expiry)) {
        return pfFail(pfError("BuyAfterExpiry", ERRORS.BuyAfterExpiry,
          "buys require now < expiry"));
      }
    }
    var userToken = ctx.userToken !== undefined ? ctx.userToken : ctx.user_token;
    if (userToken === null) {
      return pfFail(pfError("MintMismatch", ERRORS.MintMismatch,
        "no user token account was supplied"));
    }
    if (userToken && typeof userToken === "object") {
      var collateralMint = field(config, ["collateralMint", "collateral_mint"]);
      var tokenMint = field(userToken, ["mint", "collateralMint", "collateral_mint"]);
      if (collateralMint && tokenMint && String(tokenMint) !== String(collateralMint)) {
        return pfFail(pfError("MintMismatch", ERRORS.MintMismatch,
          "the user token account is not the collateral mint"));
      }
      var tokenOwner = field(userToken, ["owner", "user"]);
      var userAddress = walletAddressOf(ctx.wallet || ctx.user);
      if (tokenOwner && userAddress && String(tokenOwner) !== String(userAddress)) {
        return pfFail(pfError("MintMismatch", ERRORS.MintMismatch,
          "the user token account is not owned by the buyer"));
      }
      var balance = bigIntField(userToken, ["amount", "balance"], null);
      if (balance !== null && balance < amount) {
        return pfFail(pfError("InsufficientFunds", null,
          "the user token balance is below the amount (runtime error: InsufficientFunds)",
          { runtimeError: "InsufficientFunds" }));
      }
    }
    return { ok: true, errors: [], amount: amount.toString() };
  }

  function preflightClaim(ctx) {
    ctx = ctx || {};
    var market = ctx.market;
    if (!market || typeof market !== "object") {
      return pfFail(pfError("MARKET_NOT_FOUND", null, "the market does not exist"));
    }
    var state = stateCode(market);
    if (state === STATE_OPEN || (state !== undefined && state !== STATE_RESOLVED &&
        state !== STATE_CANCELLED)) {
      return pfFail(pfError("ClaimBeforeResolve", ERRORS.ClaimBeforeResolve,
        "the market is still open"));
    }
    var position = ctx.position;
    if (!position || typeof position !== "object") {
      // Client-side name for the missing account. On chain the same
      // situation is InvalidAccountData: the program has no
      // position-not-found variant in its error list.
      return pfFail(pfError("POSITION_NOT_FOUND", null,
        "no position exists for this user in this market " +
        "(on chain this is InvalidAccountData)",
        { onChainError: "InvalidAccountData" }));
    }
    if (position.claimed === true) {
      return pfFail(pfError("AlreadyClaimed", ERRORS.AlreadyClaimed,
        "the position was already claimed"));
    }
    var flaggedStake = bigIntField(position, ["flagged_amount", "flaggedAmount"], 0n);
    var survivesStake = bigIntField(position, ["survives_amount", "survivesAmount"], 0n);
    var totalStake = flaggedStake + survivesStake;
    if (state === STATE_CANCELLED) {
      if (totalStake === 0n) {
        return pfFail(pfError("NoClaimableStake", ERRORS.NoClaimableStake,
          "a refund claim needs a non-zero total stake"));
      }
      return { ok: true, errors: [], branch: "refund", refundReason: "cancelled",
        amount: totalStake.toString(), totalStake: totalStake.toString() };
    }
    var outcome = field(market, ["outcome"]);
    if (outcome !== SIDE_FLAGGED && outcome !== SIDE_SURVIVES) {
      return pfFail(pfError("InvalidAccountData", null,
        "a resolved market carries an outcome other than 1 or 2",
        { onChainError: "InvalidAccountData" }));
    }
    var winningPool = outcome === SIDE_FLAGGED
      ? bigIntField(market, ["flagged_pool", "flaggedPool"], 0n)
      : bigIntField(market, ["survives_pool", "survivesPool"], 0n);
    var winningStake = outcome === SIDE_FLAGGED ? flaggedStake : survivesStake;
    if (winningPool === 0n) {
      if (totalStake === 0n) {
        return pfFail(pfError("NoClaimableStake", ERRORS.NoClaimableStake,
          "a refund claim needs a non-zero total stake"));
      }
      return { ok: true, errors: [], branch: "refund", refundReason: "zero-winning-pool",
        amount: totalStake.toString(), totalStake: totalStake.toString(),
        winningPool: winningPool.toString(), winningStake: winningStake.toString() };
    }
    if (winningStake === 0n) {
      return pfFail(pfError("NoClaimableStake", ERRORS.NoClaimableStake,
        "the position holds no winning-side stake"));
    }
    var flaggedPool = bigIntField(market, ["flagged_pool", "flaggedPool"], 0n);
    var survivesPool = bigIntField(market, ["survives_pool", "survivesPool"], 0n);
    var payout = parimutuelPayout(
      winningStake.toString(), (flaggedPool + survivesPool).toString(), winningPool.toString());
    if (payout === null) {
      return pfFail(pfError("Overflow", ERRORS.Overflow,
        "the payout does not fit in u64"));
    }
    return { ok: true, errors: [], branch: "payout", amount: payout,
      totalStake: totalStake.toString(),
      winningPool: winningPool.toString(), winningStake: winningStake.toString() };
  }

  function preflightCreateMarket(ctx) {
    ctx = ctx || {};
    var config = ctx.config;
    var initialized = !!config && typeof config === "object" &&
      config.initialized !== false && config.exists !== false &&
      config.isInitialized !== false;
    if (!initialized) {
      return pfFail(pfError("ConfigNotInitialized", ERRORS.ConfigNotInitialized,
        "the config account has not been created"));
    }
    var mint = ctx.subjectMint || ctx.mint;
    var mintBytes;
    try {
      mintBytes = pubkeyBytes(mint);
    } catch (err) {
      return pfFail(pfError("INVALID_MINT", null, "the subject mint does not decode"));
    }
    if (mintBytes.length !== 32) {
      return pfFail(pfError("INVALID_MINT", null,
        "the subject mint must decode to exactly 32 bytes, got " + mintBytes.length));
    }
    var mintAddress = typeof mint === "string" ? mint : base58Encode(mintBytes);
    var markets = Array.isArray(ctx.markets) ? ctx.markets : [];
    var liveMarkets = Array.isArray(ctx.liveMarkets) ? ctx.liveMarkets : [];
    var hasLive = ctx.liveMarketExists === true || !!ctx.existingLiveMarket;
    if (!hasLive) {
      hasLive = liveMarkets.some(function (entry) {
        if (!entry || typeof entry !== "object") return false;
        var entryMint = field(entry, ["subject_mint", "subjectMint", "mint"]);
        return entryMint === mintAddress;
      });
    }
    if (!hasLive) {
      hasLive = markets.some(function (m) {
        return m && field(m, ["subject_mint", "subjectMint"]) === mintAddress &&
          stateCode(m) === STATE_OPEN;
      });
    }
    if (hasLive) {
      return pfFail(pfError("DuplicateLiveMarket", ERRORS.DuplicateLiveMarket,
        "a live market already exists for this mint"));
    }
    var nowSec = ctx.nowSec !== undefined ? ctx.nowSec : ctx.now;
    if (nowSec !== undefined) {
      var collision = false;
      if (ctx.programId) {
        try {
          var derivedNow = deriveMarket(ctx.programId, mintAddress, nowSec);
          collision = markets.some(function (m) {
            return m && field(m, ["address", "pubkey"]) === derivedNow.address;
          });
        } catch (err) {
          collision = false;
        }
      }
      if (!collision) {
        collision = markets.some(function (m) {
          return m && field(m, ["subject_mint", "subjectMint"]) === mintAddress &&
            Number(field(m, ["created_at", "createdAt"])) === Number(nowSec);
        });
      }
      if (collision) {
        return pfFail(pfError("RETRY_NEXT_SECOND", null,
          "a terminal market already sits at this second's address; retry one second later"));
      }
    }
    return { ok: true, errors: [] };
  }

  function preflightCancelMarket(ctx) {
    ctx = ctx || {};
    var market = ctx.market;
    if (!market || typeof market !== "object") {
      return pfFail(pfError("MARKET_NOT_FOUND", null, "the market does not exist"));
    }
    var state = stateCode(market);
    if (state === STATE_RESOLVED) {
      return pfFail(pfError("AlreadyResolved", ERRORS.AlreadyResolved,
        "the market is already resolved"));
    }
    if (state === STATE_CANCELLED) {
      return pfFail(pfError("AlreadyCancelled", ERRORS.AlreadyCancelled,
        "the market is already cancelled"));
    }
    var config = ctx.config || {};
    var resolver = ctx.resolver || field(config, ["resolver"]);
    var caller = ctx.callerAddress || walletAddressOf(ctx.caller || ctx.wallet);
    var isResolver = !!caller && !!resolver && String(caller) === String(resolver);
    if (isResolver) return { ok: true, errors: [], isResolver: true };
    var nowSec = ctx.nowSec !== undefined ? ctx.nowSec : ctx.now;
    var expiry = bigIntField(market, ["expiry"], null);
    if (nowSec === undefined || expiry === null) {
      return pfFail(pfError("CancelTooEarly", ERRORS.CancelTooEarly,
        "the cancel deadline cannot be established"));
    }
    var deadline = expiry + BigInt(CANCEL_GRACE_SECS);
    // A deadline past the largest representable clock value is a
    // deadline nobody can pass: the honest answer is too early,
    // never a wrapped deadline.
    if (deadline > I64_MAX || !(BigInt(Math.trunc(nowSec)) > deadline)) {
      return pfFail(pfError("CancelTooEarly", ERRORS.CancelTooEarly,
        "a non-resolver may cancel only after expiry plus the grace window"));
    }
    return { ok: true, errors: [], isResolver: false };
  }

  function normalizeConfig(raw) {
    if (!raw || typeof raw !== "object") {
      return { initialized: false, defaultDurationSecs: DEFAULT_DURATION_SECS };
    }
    var duration = field(raw, ["defaultDurationSecs", "default_duration_secs",
      "defaultDuration", "durationSecs"]);
    return {
      address: field(raw, ["address", "pubkey", "configAddress"]) || null,
      resolver: field(raw, ["resolver"]) || null,
      collateralMint: field(raw, ["collateralMint", "collateral_mint"]) || null,
      minStake: field(raw, ["minStake", "min_stake", "minStakeRaw", "min_stake_raw"]),
      defaultDurationSecs: duration !== undefined ? Number(duration) : DEFAULT_DURATION_SECS,
      initialized: raw.initialized !== false && raw.exists !== false &&
        raw.isInitialized !== false,
      raw: raw,
    };
  }

  function isFixtureSourced(data, extra) {
    var sources = [data, extra];
    return sources.some(function (s) {
      return !!s && typeof s === "object" &&
        (s.source === "fixture" || s.origin === "fixture" ||
         s.fixture === true || s.isFixture === true);
    });
  }

  function normalizeSide(side) {
    if (side === "flagged" || side === "Flagged" || side === "FLAGGED") return SIDE_FLAGGED;
    if (side === "survives" || side === "Survives" || side === "SURVIVES") return SIDE_SURVIVES;
    return side;
  }

  function createPredictClient(options) {
    var opts = options || {};
    var data = opts.data && typeof opts.data === "object" ? opts.data : {};
    var programId = opts.programId !== undefined && opts.programId !== null
      ? opts.programId
      : (data.program_id !== undefined ? data.program_id : null);
    var config = normalizeConfig(opts.config || data.config);
    var wallet = opts.wallet || null;
    var mode = opts.mode || "fixture";
    var nowFn = typeof opts.now === "function" ? opts.now
      : (typeof opts.now === "number" ? function () { return opts.now; }
         : function () { return Date.now(); });
    if (!Array.isArray(data.markets)) data.markets = [];
    if (!Array.isArray(data.positions)) data.positions = [];

    function liveMarketsList() {
      if (!Array.isArray(data.liveMarkets)) data.liveMarkets = [];
      return data.liveMarkets;
    }

    function nowSec() {
      return Math.floor(nowFn() / 1000);
    }

    function tokenRecords() {
      if (Array.isArray(data.tokens)) return data.tokens;
      if (Array.isArray(data.userTokens)) return data.userTokens;
      if (Array.isArray(data.tokenAccounts)) return data.tokenAccounts;
      return [];
    }

    function findMarket(ref) {
      if (!ref) return null;
      if (typeof ref === "string") {
        return data.markets.find(function (m) { return m && m.address === ref; }) || null;
      }
      if (typeof ref === "object") {
        if (ref.address) {
          var found = data.markets.find(function (m) { return m && m.address === ref.address; });
          if (found) return found;
        }
        return ref;
      }
      return null;
    }

    function findPosition(marketAddress, userAddress) {
      return data.positions.find(function (p) {
        return p && field(p, ["market"]) === marketAddress &&
          field(p, ["user"]) === userAddress;
      }) || null;
    }

    function findToken(ref, ownerAddress) {
      var records = tokenRecords();
      if (typeof ref === "string") {
        return records.find(function (t) { return t && t.address === ref; }) || null;
      }
      if (ref && typeof ref === "object" &&
          (ref.mint !== undefined || ref.owner !== undefined ||
           ref.amount !== undefined || ref.balance !== undefined)) {
        return ref;
      }
      if (ownerAddress) {
        return records.find(function (t) {
          return t && field(t, ["owner", "user"]) === ownerAddress;
        }) || null;
      }
      return null;
    }

    function tokenAddressFor(ref, ownerAddress) {
      if (typeof ref === "string") return ref;
      if (ref && typeof ref === "object" && ref.address) return ref.address;
      var record = findToken(ref, ownerAddress);
      if (record && record.address) return record.address;
      if (ownerAddress && config.collateralMint) {
        try {
          return deriveAssociatedTokenAddress(ownerAddress, config.collateralMint).address;
        } catch (err) {
          return null;
        }
      }
      return null;
    }

    function adjustToken(record, delta) {
      if (!record) return;
      var amountField = record.amount !== undefined ? "amount"
        : (record.balance !== undefined ? "balance" : "amount");
      var current = bigIntField(record, ["amount", "balance"], 0n);
      record[amountField] = (current + delta).toString();
    }

    function baseRefusal(code, message) {
      return {
        ok: false, code: code, mode: mode,
        signed: false, submitted: false, status: "refused",
        signature: null, explorerUrl: null,
        instruction: null, derived: null, preflight: null, stateDelta: null,
        message: message,
      };
    }

    function gate(extraSource) {
      if (!programId) {
        return baseRefusal("PROGRAM_NOT_DEPLOYED",
          "no program id is configured; nothing was built");
      }
      if (mode === "live" && isFixtureSourced(data, extraSource)) {
        return baseRefusal("MODE_MISMATCH",
          "a live transaction cannot be built from fixture-sourced state");
      }
      return null;
    }

    function preflightRefusal(preflight) {
      var first = preflight.errors[0] || {};
      return {
        ok: false, code: first.code || "PREFLIGHT_FAILED", mode: mode,
        signed: false, submitted: false, status: mode === "fixture" ? "simulated" : "refused",
        signature: null, explorerUrl: null,
        instruction: null, derived: null, preflight: preflight, stateDelta: null,
      };
    }

    function success(name, ordinal, descriptor, derived, preflight, stateDelta, extra) {
      var envelope = {
        ok: true, mode: mode,
        signed: false, submitted: false,
        status: mode === "fixture" ? "simulated" : "built",
        signature: null, explorerUrl: null,
        instruction: {
          name: name, ordinal: ordinal,
          dataHex: bytesToHex(descriptor.data), dataLen: descriptor.data.length,
          keys: descriptor.keys, programId: descriptor.programId,
        },
        descriptor: descriptor,
        derived: derived, preflight: preflight, stateDelta: stateDelta,
      };
      if (extra) {
        for (var k in extra) envelope[k] = extra[k];
      }
      return envelope;
    }

    function derivedConfig() {
      try { return deriveConfig(programId); } catch (err) { return null; }
    }

    function configKey() {
      if (config.address) return config.address;
      var d = derivedConfig();
      return d ? d.address : null;
    }

    function createMarket(subjectMintInput) {
      var refused = gate(null);
      if (refused) return refused;
      var creator = walletAddressOf(wallet);
      if (!creator) {
        return preflightRefusal({ ok: false, errors: [pfError(
          "WALLET_NOT_CONNECTED", null, "no wallet is connected")] });
      }
      var createdAtSec = nowSec();
      var preflight = preflightCreateMarket({
        config: config, subjectMint: subjectMintInput, markets: data.markets,
        liveMarkets: Array.isArray(data.liveMarkets) ? data.liveMarkets : [],
        programId: programId, nowSec: createdAtSec,
      });
      if (!preflight.ok) return preflightRefusal(preflight);
      var mintAddress = addressString(subjectMintInput);
      var marketD = deriveMarket(programId, mintAddress, createdAtSec);
      var liveD = deriveLiveMarket(programId, mintAddress);
      var vaultD = deriveVault(programId, marketD.address);
      var descriptor = buildCreateMarketIx({
        programId: programId, creator: creator, configAddress: configKey(),
        market: marketD.address, liveMarket: liveD.address, vault: vaultD.address,
        collateralMint: config.collateralMint, subjectMint: mintAddress,
      });
      var duration = config.defaultDurationSecs;
      var marketRecord = {
        address: marketD.address,
        live_market: liveD.address,
        subject_mint: mintAddress,
        creator: creator,
        created_at: createdAtSec,
        expiry: createdAtSec + duration,
        flagged_pool: "0",
        survives_pool: "0",
        state: STATE_OPEN,
        outcome: 0,
        evidence_hash: "0".repeat(64),
        hard_trigger: 0,
        resolved_at: 0,
        bump: marketD.bump,
        vault_bump: vaultD.bump,
      };
      if (mode === "fixture") {
        data.markets.push(marketRecord);
        liveMarketsList().push({
          address: liveD.address, market: marketD.address,
          subject_mint: mintAddress, bump: liveD.bump,
        });
      }
      var stateDelta = mode === "fixture" ? {
        action: "create-market", market: marketRecord,
        liveMarket: { address: liveD.address, market: marketD.address },
      } : null;
      return success("CreateMarket", ORDINALS.CreateMarket, descriptor,
        { config: derivedConfig(), market: marketD, liveMarket: liveD, vault: vaultD },
        preflight, stateDelta, { market: marketRecord });
    }

    // Accepts either an options object or the positional form
    // buyPosition(marketAddress, side, amount).
    function buyPosition(input, sideArg, amountArg) {
      if (typeof input === "string") {
        input = { market: input, side: sideArg, amount: amountArg };
      }
      input = input || {};
      var refused = gate(input.market);
      if (refused) return refused;
      var user = walletAddressOf(wallet);
      var marketRecord = findMarket(input.market || input.marketAddress);
      var tokenRecord = findToken(input.userToken || input.userTokenAddress, user);
      var tokenRef = tokenRecord || input.userToken || input.userTokenAddress;
      var side = normalizeSide(input.side);
      var preflight = preflightBuyPosition({
        wallet: wallet, side: side, amount: input.amount, config: config,
        market: marketRecord, programId: programId,
        userToken: tokenRef === undefined ? undefined : tokenRef, nowSec: nowSec(),
      });
      if (!preflight.ok) return preflightRefusal(preflight);
      var marketAddress = marketRecord.address;
      var vaultD = deriveVault(programId, marketAddress);
      var positionD = derivePosition(programId, marketAddress, user);
      var descriptor = buildBuyPositionIx({
        programId: programId, user: user, configAddress: configKey(),
        market: marketAddress, vault: vaultD.address,
        userToken: tokenAddressFor(tokenRef, user), position: positionD.address,
        side: side, amount: input.amount,
      });
      var amount = parseU64(input.amount, "amount");
      var stateDelta = null;
      if (mode === "fixture") {
        var poolField = side === SIDE_FLAGGED ? "flagged_pool" : "survives_pool";
        var posField = side === SIDE_FLAGGED ? "flagged_amount" : "survives_amount";
        marketRecord[poolField] =
          (bigIntField(marketRecord, [poolField], 0n) + amount).toString();
        var position = findPosition(marketAddress, user);
        if (!position) {
          position = {
            address: positionD.address, market: marketAddress, user: user,
            flagged_amount: "0", survives_amount: "0", claimed: false,
            bump: positionD.bump,
          };
          data.positions.push(position);
        }
        position[posField] =
          (bigIntField(position, [posField], 0n) + amount).toString();
        adjustToken(tokenRecord, -amount);
        var vaultRecord = findToken(vaultD.address, null);
        adjustToken(vaultRecord, amount);
        stateDelta = {
          action: "buy-position", market: marketAddress, side: side,
          amount: amount.toString(),
          flaggedPool: String(marketRecord.flagged_pool),
          survivesPool: String(marketRecord.survives_pool),
          position: position,
        };
      }
      return success("BuyPosition", ORDINALS.BuyPosition, descriptor,
        { config: derivedConfig(), market: { address: marketAddress },
          vault: vaultD, position: positionD },
        preflight, stateDelta);
    }

    function claim(input) {
      if (typeof input === "string") input = { market: input };
      input = input || {};
      var refused = gate(input.market);
      if (refused) return refused;
      var user = walletAddressOf(wallet);
      var marketRecord = findMarket(input.market || input.marketAddress);
      var position = marketRecord && user
        ? findPosition(marketRecord.address, user) : null;
      if (input.position && typeof input.position === "object") position = input.position;
      var preflight = preflightClaim({ market: marketRecord, position: position });
      if (!preflight.ok) return preflightRefusal(preflight);
      var marketAddress = marketRecord.address;
      var vaultD = deriveVault(programId, marketAddress);
      var positionD = derivePosition(programId, marketAddress, user);
      var tokenRecord = findToken(input.userToken || input.userTokenAddress, user);
      var descriptor = buildClaimIx({
        programId: programId, user: user, configAddress: configKey(),
        market: marketAddress, vault: vaultD.address,
        userToken: tokenAddressFor(tokenRecord || input.userToken, user),
        position: positionD.address,
      });
      var amount = BigInt(preflight.amount);
      var stateDelta = null;
      if (mode === "fixture") {
        position.claimed = true;
        adjustToken(tokenRecord, amount);
        var vaultRecord = findToken(vaultD.address, null);
        adjustToken(vaultRecord, -amount);
        stateDelta = {
          action: "claim", market: marketAddress, branch: preflight.branch,
          refundReason: preflight.refundReason || null, amount: preflight.amount,
          position: position,
        };
      }
      return success("Claim", ORDINALS.Claim, descriptor,
        { config: derivedConfig(), market: { address: marketAddress },
          vault: vaultD, position: positionD },
        preflight, stateDelta,
        { branch: preflight.branch, amount: preflight.amount });
    }

    function cancelMarket(input) {
      if (typeof input === "string") input = { market: input };
      input = input || {};
      var refused = gate(input.market);
      if (refused) return refused;
      var caller = walletAddressOf(wallet);
      var marketRecord = findMarket(input.market || input.marketAddress);
      var preflight = preflightCancelMarket({
        caller: wallet, callerAddress: caller, config: config,
        market: marketRecord, nowSec: nowSec(),
      });
      if (!preflight.ok) return preflightRefusal(preflight);
      var marketAddress = marketRecord.address;
      var subjectMint = field(marketRecord, ["subject_mint", "subjectMint"]);
      var liveD = null;
      try { liveD = subjectMint ? deriveLiveMarket(programId, subjectMint) : null; }
      catch (err) { liveD = null; }
      var liveAddress = field(marketRecord, ["live_market", "liveMarket"]) ||
        (liveD && liveD.address);
      var descriptor = buildCancelMarketIx({
        programId: programId, caller: caller, configAddress: configKey(),
        market: marketAddress, liveMarket: liveAddress,
        creator: field(marketRecord, ["creator"]),
      });
      var stateDelta = null;
      if (mode === "fixture") {
        marketRecord.state = STATE_CANCELLED;
        var locks = liveMarketsList();
        var idx = locks.findIndex(function (entry) {
          return entry && (entry.market === marketAddress || entry.address === liveAddress);
        });
        if (idx >= 0) locks.splice(idx, 1);
        stateDelta = {
          action: "cancel-market", market: marketAddress,
          state: STATE_CANCELLED, lockReleased: true,
        };
      }
      return success("CancelMarket", ORDINALS.CancelMarket, descriptor,
        { config: derivedConfig(), market: { address: marketAddress }, liveMarket: liveD },
        preflight, stateDelta);
    }

    return {
      createMarket: createMarket,
      buyPosition: buyPosition,
      claim: claim,
      cancelMarket: cancelMarket,
      getState: function () { return data; },
      config: config,
      programId: programId,
      mode: mode,
    };
  }

  return {
    TOKEN_PROGRAM_ID: TOKEN_PROGRAM_ID,
    ATA_PROGRAM_ID: ATA_PROGRAM_ID,
    SYSTEM_PROGRAM_ID: SYSTEM_PROGRAM_ID,
    PROGRAM_IDS: PROGRAM_IDS,
    SEED_CONFIG: SEED_CONFIG,
    SEED_MARKET: SEED_MARKET,
    SEED_VAULT: SEED_VAULT,
    SEED_POSITION: SEED_POSITION,
    SEED_LIVE_MARKET: SEED_LIVE_MARKET,
    STATE_OPEN: STATE_OPEN,
    STATE_RESOLVED: STATE_RESOLVED,
    STATE_CANCELLED: STATE_CANCELLED,
    SIDE_FLAGGED: SIDE_FLAGGED,
    SIDE_SURVIVES: SIDE_SURVIVES,
    MIN_STAKE_RAW: MIN_STAKE_RAW,
    DEFAULT_DURATION_SECS: DEFAULT_DURATION_SECS,
    CANCEL_GRACE_SECS: CANCEL_GRACE_SECS,
    ORDINALS: ORDINALS,
    ERRORS: ERRORS,
    base58Encode: base58Encode,
    base58Decode: base58Decode,
    sha256: sha256Bytes,
    sha256Bytes: sha256Bytes,
    sha256Hex: sha256Hex,
    isOnCurve: isOnCurve,
    findProgramAddress: findProgramAddress,
    deriveConfig: deriveConfig,
    deriveLiveMarket: deriveLiveMarket,
    deriveMarket: deriveMarket,
    deriveVault: deriveVault,
    derivePosition: derivePosition,
    deriveAssociatedTokenAddress: deriveAssociatedTokenAddress,
    parimutuelPayout: parimutuelPayout,
    encodeCreateMarket: encodeCreateMarket,
    encodeBuyPosition: encodeBuyPosition,
    encodeResolveMarket: encodeResolveMarket,
    encodeClaim: encodeClaim,
    encodeCancelMarket: encodeCancelMarket,
    buildCreateMarketIx: buildCreateMarketIx,
    buildBuyPositionIx: buildBuyPositionIx,
    buildClaimIx: buildClaimIx,
    buildCancelMarketIx: buildCancelMarketIx,
    preflightBuyPosition: preflightBuyPosition,
    preflightClaim: preflightClaim,
    preflightCreateMarket: preflightCreateMarket,
    preflightCancelMarket: preflightCancelMarket,
    createPredictClient: createPredictClient,
    bytesToHex: bytesToHex,
    hexToBytes: hexToBytes,
  };
})();

if (typeof module !== "undefined" && module.exports) {
  module.exports = RugCallTx;
}
if (typeof window !== "undefined") {
  window.RugCallTx = RugCallTx;
}
if (typeof globalThis !== "undefined") {
  globalThis.RugCallTx = RugCallTx;
}
