"use strict";

// Tests for the resolver's live mode: preflight refusals against a
// stub chain (no signing is ever attempted on a refusal path), the
// submitting happy path through the real transaction builder, and
// the zero-hash rail. All keypairs are synthetic temp files built
// from a fixed seed in this file; no real keys are used.

const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { createLiveRunner } = require("../resolver.js");
const submit = require("../submit.js");
const ev = require("../../app/evidence.js");

const FIXTURES = path.join(__dirname, "fixtures");
const EVIDENCE_DIR = path.join(FIXTURES, "evidence");
const NOW = 1799920800;
const BPF_LOADER = "BPFLoaderUpgradeab1e11111111111111111111111";
const ED25519_PKCS8_PREFIX = Buffer.from(
  "302e020100300506032b657004220420",
  "hex"
);

const enc = (byte) => submit.base58Encode(Buffer.alloc(32, byte));
const PROGRAM_ID = enc(82);
const CONFIG_ID = enc(24);
const SIGNATURE = submit.base58Encode(Buffer.alloc(64, 9));

function tmpDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "rugcall-live-test-"));
}

// Build a valid synthetic keypair file: a fixed 32-byte seed plus
// the public key actually derived from it.
function makeKeypair(dir, seedByte) {
  const seed = Buffer.alloc(32, seedByte);
  const privateKey = crypto.createPrivateKey({
    key: Buffer.concat([ED25519_PKCS8_PREFIX, seed]),
    format: "der",
    type: "pkcs8",
  });
  const jwk = crypto.createPublicKey(privateKey).export({ format: "jwk" });
  const pubkeyBytes = Buffer.from(jwk.x, "base64url");
  const bytes = Buffer.concat([seed, pubkeyBytes]);
  const keypairPath = path.join(dir, "keypair.json");
  fs.writeFileSync(keypairPath, JSON.stringify([...bytes]));
  const signer = submit.loadResolverSigner(keypairPath);
  return { keypairPath, bytes, pubkeyBytes, signer };
}

function spySigner(realSigner) {
  const spy = {
    pubkey: realSigner.pubkey,
    pubkeyBytes: realSigner.pubkeyBytes,
    signCalls: 0,
    sign(msg) {
      spy.signCalls += 1;
      return realSigner.sign(msg);
    },
  };
  return spy;
}

function spySubmitter(impl) {
  const spy = {
    calls: [],
    async submitInstruction(args) {
      spy.calls.push(args);
      if (impl) return impl(args);
      return { signature: SIGNATURE, confirmed: true };
    },
  };
  return spy;
}

function configBytes(resolverPubkeyBytes, mutate) {
  const data = Buffer.alloc(122);
  data[0] = 1;
  Buffer.alloc(32, 10).copy(data, 1); // admin
  Buffer.from(resolverPubkeyBytes).copy(data, 33); // resolver
  Buffer.alloc(32, 11).copy(data, 65); // collateral mint
  if (mutate) mutate(data);
  return data;
}

// A readable on-chain Market account (142-byte layout) matching
// liveMarketsObj below: the live preflight now reads market
// accounts back from the chain, so the stub chain must serve one.
function marketAccountInfo(mutate) {
  const data = Buffer.alloc(142);
  data[0] = 2;
  Buffer.alloc(32, 25).copy(data, 1); // subject_mint = enc(25)
  Buffer.alloc(32, 23).copy(data, 33); // creator = enc(23)
  data.writeBigInt64LE(1799913600n, 65); // created_at
  data.writeBigInt64LE(1800000000n, 73); // expiry
  data[97] = 1; // state Open
  if (mutate) mutate(data);
  return { executable: false, owner: PROGRAM_ID, lamports: 1000000, data };
}

function makeChain(overrides) {
  const o = overrides || {};
  const chain = {
    calls: {
      getHealth: 0,
      getAccountInfo: [],
      getBalance: 0,
      getLatestBlockhash: 0,
      sendRawTransaction: 0,
      getSignatureStatus: 0,
    },
    sentBytes: [],
    totalCalls() {
      return (
        chain.calls.getHealth +
        chain.calls.getAccountInfo.length +
        chain.calls.getBalance +
        chain.calls.getLatestBlockhash +
        chain.calls.sendRawTransaction +
        chain.calls.getSignatureStatus
      );
    },
    async getHealth() {
      chain.calls.getHealth += 1;
      if (o.healthError) throw o.healthError;
      return "ok";
    },
    async getAccountInfo(pubkey) {
      chain.calls.getAccountInfo.push(pubkey);
      if (pubkey === PROGRAM_ID) {
        return o.programInfo !== undefined
          ? o.programInfo
          : {
              executable: true,
              owner: BPF_LOADER,
              lamports: 1000000,
              data: Buffer.alloc(0),
            };
      }
      if (pubkey === CONFIG_ID) {
        return o.configInfo !== undefined ? o.configInfo : null;
      }
      if (pubkey === enc(21)) {
        return o.marketInfo !== undefined ? o.marketInfo : marketAccountInfo();
      }
      return null;
    },
    async getBalance() {
      chain.calls.getBalance += 1;
      return o.balance !== undefined ? o.balance : 100000000;
    },
    async getLatestBlockhash() {
      chain.calls.getLatestBlockhash += 1;
      return enc(3);
    },
    async sendRawTransaction(bytes) {
      chain.calls.sendRawTransaction += 1;
      chain.sentBytes.push(Buffer.from(bytes));
      if (o.onSend) o.onSend();
      return SIGNATURE;
    },
    async getSignatureStatus() {
      chain.calls.getSignatureStatus += 1;
      if (o.onStatus) o.onStatus();
      return { confirmationStatus: "confirmed" };
    },
  };
  return chain;
}

function loadMarkets() {
  return JSON.parse(
    fs.readFileSync(path.join(FIXTURES, "markets.json"), "utf8")
  );
}

// A single H2 market whose account addresses are real 32-byte keys
// (the fixture display addresses do not decode to 32 bytes, so the
// submitting path needs these), with the test signer as resolver.
function liveMarketsObj(signerPubkey) {
  const markets = loadMarkets();
  const template = markets.markets.find((m) => m.state === 1 && m.evidence_file === "h2-bundle.json");
  const market = {
    ...structuredClone(template),
    address: enc(21),
    live_market: enc(22),
    creator: enc(23),
    // A real 32-byte mint key: the fixture display mint does not
    // decode to 32 bytes, so it cannot match an on-chain account.
    subject_mint: enc(25),
  };
  return {
    ...markets,
    program_id: null,
    config: CONFIG_ID,
    resolver: signerPubkey,
    markets: [market],
  };
}

function fixtureMarketsObj(signerPubkey) {
  const markets = loadMarkets();
  return { ...markets, config: CONFIG_ID, resolver: signerPubkey };
}

function makeRunner(o) {
  const records = [];
  const runner = createLiveRunner({
    chain: o.chain,
    signer: o.signer,
    submitter: o.submitter,
    sink: (r) => records.push(r),
    now: NOW,
    outDir: tmpDir(),
    evidenceDir: EVIDENCE_DIR,
    ledgerPath: o.ledgerPath || null,
    crewStore: ev.emptyCrewStore(),
    keypairPath: o.keypairPath,
    programId: o.programId !== undefined ? o.programId : PROGRAM_ID,
    hashFn: o.hashFn,
    minResolverLamports: o.minResolverLamports,
    rpcUrl: "https://api.devnet.solana.com",
  });
  return { runner, records };
}

function assertRefusal(outcome, records, code, spies) {
  assert.equal(outcome.exitCode, 1);
  assert.equal(records.length, 1, `records ${JSON.stringify(records)}`);
  const record = records[0];
  assert.equal(record.action, "REFUSAL");
  assert.equal(record.decision_reason, "live-preflight-failed");
  assert.equal(record.reason, code);
  assert.equal(record.live_preflight.check, code);
  assert.equal(record.live_preflight.rpc_url, "https://api.devnet.solana.com");
  assert.equal(record.signed, false);
  assert.equal(record.submitted, false);
  assert.equal(record.signature, null);
  if (spies) {
    assert.equal(spies.signer.signCalls, 0, "signer must not be called");
    assert.equal(spies.submitter.calls.length, 0, "submitter must not be called");
  }
}

function goodConfigInfo(pubkeyBytes, mutate) {
  return {
    executable: false,
    owner: PROGRAM_ID,
    lamports: 1000000,
    data: configBytes(pubkeyBytes, mutate),
  };
}

test("live: missing keypair file is refused before any chain call", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 41);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter();
  const chain = makeChain({ configInfo: goodConfigInfo(kp.pubkeyBytes) });
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: path.join(dir, "does-not-exist.json"),
  });
  const outcome = await runner.runLivePass(fixtureMarketsObj(signer.pubkey));
  assertRefusal(outcome, records, "keypair-missing", { signer, submitter });
  assert.equal(chain.totalCalls(), 0);
});

test("live: null program id is refused with zero chain calls", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 42);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter();
  const chain = makeChain({ configInfo: goodConfigInfo(kp.pubkeyBytes) });
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: kp.keypairPath,
    programId: null,
  });
  const outcome = await runner.runLivePass(fixtureMarketsObj(signer.pubkey));
  assertRefusal(outcome, records, "program-id-unset", { signer, submitter });
  assert.equal(outcome.records[0].live_preflight.program_id, null);
  assert.equal(chain.totalCalls(), 0);
});

test("live: signer that is not the config resolver is refused", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 43);
  const other = makeKeypair(tmpDir(), 44);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter();
  const chain = makeChain({ configInfo: goodConfigInfo(other.pubkeyBytes) });
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: kp.keypairPath,
  });
  const outcome = await runner.runLivePass(fixtureMarketsObj(signer.pubkey));
  assertRefusal(outcome, records, "keypair-config-mismatch", { signer, submitter });
  assert.equal(chain.calls.getBalance, 0);
});

test("live: missing program account is refused", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 45);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter();
  const chain = makeChain({
    programInfo: null,
    configInfo: goodConfigInfo(kp.pubkeyBytes),
  });
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: kp.keypairPath,
  });
  const outcome = await runner.runLivePass(fixtureMarketsObj(signer.pubkey));
  assertRefusal(outcome, records, "program-missing", { signer, submitter });
});

test("live: non-executable program account is refused", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 46);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter();
  const chain = makeChain({
    programInfo: {
      executable: false,
      owner: BPF_LOADER,
      lamports: 1000000,
      data: Buffer.alloc(0),
    },
    configInfo: goodConfigInfo(kp.pubkeyBytes),
  });
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: kp.keypairPath,
  });
  const outcome = await runner.runLivePass(fixtureMarketsObj(signer.pubkey));
  assertRefusal(outcome, records, "program-not-executable", { signer, submitter });
});

test("live: program account with the wrong owner is refused", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 47);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter();
  const chain = makeChain({
    programInfo: {
      executable: true,
      owner: "11111111111111111111111111111111",
      lamports: 1000000,
      data: Buffer.alloc(0),
    },
    configInfo: goodConfigInfo(kp.pubkeyBytes),
  });
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: kp.keypairPath,
  });
  const outcome = await runner.runLivePass(fixtureMarketsObj(signer.pubkey));
  assertRefusal(outcome, records, "program-wrong-owner", { signer, submitter });
});

test("live: unreachable RPC is refused", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 48);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter();
  const chain = makeChain({
    healthError: new Error("connection refused"),
    configInfo: goodConfigInfo(kp.pubkeyBytes),
  });
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: kp.keypairPath,
  });
  const outcome = await runner.runLivePass(fixtureMarketsObj(signer.pubkey));
  assertRefusal(outcome, records, "rpc-unreachable", { signer, submitter });
});

test("live: underfunded resolver is refused with the amounts in the record", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 49);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter();
  const chain = makeChain({
    balance: 12345,
    configInfo: goodConfigInfo(kp.pubkeyBytes),
  });
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: kp.keypairPath,
  });
  const outcome = await runner.runLivePass(fixtureMarketsObj(signer.pubkey));
  assertRefusal(outcome, records, "resolver-underfunded", { signer, submitter });
  const preflight = records[0].live_preflight;
  assert.equal(preflight.actual_lamports, 12345);
  assert.equal(preflight.required_lamports, 50000000);
  assert.match(preflight.detail, /12345/);
  assert.match(preflight.detail, /50000000/);
});

test("live: missing or malformed config account is refused", async () => {
  const cases = [
    { label: "config missing", configInfo: null, code: "config-missing" },
    {
      label: "bad length",
      configInfo: {
        executable: false,
        owner: PROGRAM_ID,
        lamports: 1000000,
        data: Buffer.alloc(100),
      },
      code: "config-malformed",
    },
    {
      label: "bad discriminator",
      configInfo: null, // filled below with mutated bytes
      mutate: (data) => {
        data[0] = 2;
      },
      code: "config-malformed",
    },
    {
      label: "wrong owner",
      configInfo: null,
      ownerOverride: "11111111111111111111111111111111",
      code: "config-malformed",
    },
  ];
  for (const c of cases) {
    const dir = tmpDir();
    const kp = makeKeypair(dir, 50);
    const signer = spySigner(kp.signer);
    const submitter = spySubmitter();
    let configInfo = c.configInfo;
    if (c.mutate) configInfo = goodConfigInfo(kp.pubkeyBytes, c.mutate);
    if (c.ownerOverride) {
      configInfo = { ...goodConfigInfo(kp.pubkeyBytes), owner: c.ownerOverride };
    }
    const chain = makeChain({ configInfo });
    const { runner, records } = makeRunner({
      chain,
      signer,
      submitter,
      keypairPath: kp.keypairPath,
    });
    const outcome = await runner.runLivePass(fixtureMarketsObj(signer.pubkey));
    assertRefusal(outcome, records, c.code, { signer, submitter });
  }
});

test("live: passing preflight submits the decided instruction exactly once", async () => {
  // Signing known-answer checks, folded into this test: a fixed
  // synthetic keypair signs a fixed message, the signature verifies
  // against the derived public key, and an independent JWK
  // construction produces the identical signature.
  const dir = tmpDir();
  const kp = makeKeypair(dir, 51);
  const fixedMessage = Buffer.from("rugcall submit known answer", "utf8");
  const signature = kp.signer.sign(fixedMessage);
  assert.equal(signature.length, 64);
  const publicKey = crypto.createPublicKey({
    key: { kty: "OKP", crv: "Ed25519", x: kp.pubkeyBytes.toString("base64url") },
    format: "jwk",
  });
  assert.equal(crypto.verify(null, fixedMessage, publicKey, signature), true);
  const independentPrivate = crypto.createPrivateKey({
    key: {
      kty: "OKP",
      crv: "Ed25519",
      x: kp.pubkeyBytes.toString("base64url"),
      d: kp.bytes.subarray(0, 32).toString("base64url"),
    },
    format: "jwk",
  });
  assert.ok(
    crypto.sign(null, fixedMessage, independentPrivate).equals(signature)
  );

  // Legacy message byte layout against a hand-computed message.
  const marketKey = enc(7);
  const configKey = enc(5);
  const built = submit.buildLegacyMessage({
    feePayer: kp.signer.pubkey,
    recentBlockhash: enc(3),
    instructions: [
      {
        programId: PROGRAM_ID,
        dataHex: "0301",
        accounts: [
          { pubkey: kp.signer.pubkey, signer: true, writable: false },
          { pubkey: marketKey, signer: false, writable: true },
          { pubkey: configKey, signer: false, writable: false },
        ],
      },
    ],
  });
  const expectedMessage = Buffer.concat([
    Buffer.from([1, 0, 2]),
    Buffer.from([4]),
    kp.pubkeyBytes,
    Buffer.alloc(32, 7),
    Buffer.alloc(32, 5),
    Buffer.alloc(32, 82),
    Buffer.alloc(32, 3),
    Buffer.from([1]),
    Buffer.from([3]),
    Buffer.from([3]),
    Buffer.from([0, 1, 2]),
    Buffer.from([2]),
    Buffer.from([0x03, 0x01]),
  ]);
  assert.ok(built.messageBytes.equals(expectedMessage));

  // The live pass itself, through the real submitter.
  const ledgerPath = path.join(dir, "ledger.json");
  let ledgerHadEntryAtStatusTime = null;
  const marketsObj = liveMarketsObj(kp.signer.pubkey);
  const marketAddress = marketsObj.markets[0].address;
  const chain = makeChain({
    configInfo: goodConfigInfo(kp.pubkeyBytes),
    onStatus: () => {
      ledgerHadEntryAtStatusTime =
        fs.existsSync(ledgerPath) &&
        !!JSON.parse(fs.readFileSync(ledgerPath, "utf8"))[marketAddress];
    },
  });
  const signer = spySigner(kp.signer);
  const realSubmitter = submit.createSubmitter({
    chain,
    signer,
    pollAttempts: 3,
    pollIntervalMs: 1,
  });
  const submitter = spySubmitter((args) =>
    realSubmitter.submitInstruction(args)
  );
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: kp.keypairPath,
    ledgerPath,
  });
  const outcome = await runner.runLivePass(marketsObj);
  assert.equal(outcome.exitCode, 0, JSON.stringify(records));
  assert.equal(submitter.calls.length, 1);
  assert.equal(signer.signCalls, 1);
  const resolves = records.filter((r) => r.action === "WOULD_RESOLVE");
  assert.equal(resolves.length, 1);
  assert.equal(submitter.calls[0].programId, PROGRAM_ID);
  assert.equal(
    submitter.calls[0].instruction.dataHex,
    resolves[0].instruction.data_hex
  );
  assert.equal(chain.calls.sendRawTransaction, 1);
  const rawTx = chain.sentBytes[0];
  const dataBytes = Buffer.from(resolves[0].instruction.data_hex, "hex");
  assert.ok(rawTx.indexOf(dataBytes) > 64, "instruction data is in the transaction");
  assert.equal(rawTx[0], 1);
  assert.equal(
    crypto.verify(null, rawTx.subarray(65), publicKey, rawTx.subarray(1, 65)),
    true
  );
  assert.equal(resolves[0].signed, true);
  assert.equal(resolves[0].submitted, true);
  assert.equal(resolves[0].signature, SIGNATURE);
  assert.equal(ledgerHadEntryAtStatusTime, false);
  const ledger = JSON.parse(fs.readFileSync(ledgerPath, "utf8"));
  assert.equal(ledger[marketAddress].action, "WOULD_RESOLVE");
  assert.equal(ledger[marketAddress].submitted, true);
  assert.equal(ledger[marketAddress].signature, SIGNATURE);
  assert.equal(ledger[marketAddress].decided_at, NOW);
});

test("live: a zero evidence hash blocks the pass before any signing", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 52);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter();
  const chain = makeChain({ configInfo: goodConfigInfo(kp.pubkeyBytes) });
  const { runner, records } = makeRunner({
    chain,
    signer,
    submitter,
    keypairPath: kp.keypairPath,
    hashFn: () => ({ hex: "0".repeat(64), bytes: Buffer.alloc(32) }),
  });
  const outcome = await runner.runLivePass(liveMarketsObj(signer.pubkey));
  assert.equal(outcome.exitCode, 2);
  assert.ok(records.some((r) => r.action === "BLOCKED_ZERO_HASH"));
  assert.equal(signer.signCalls, 0);
  assert.equal(submitter.calls.length, 0);
});
