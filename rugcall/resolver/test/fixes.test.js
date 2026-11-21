"use strict";

// Regression tests for the six-seat review fixes:
//  1. ledger mode confusion (dry-run vs live finality/ownership)
//  2. signature loss on the submit throw path + bounded re-polls
//  3. signerFromBytes pubkey/seed mismatch
//  4. verifyBundle extended consistency checks
//  5. fetchErrors.funders dead channel
//  6. triggerDefiniteStates export
//  7. live preflight on-chain market account check

const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { createResolver, createLiveRunner, UNCONFIRMED_MAX_REPOLLS } = require("../resolver.js");
const submit = require("../submit.js");
const ev = require("../../app/evidence.js");

const FIXTURES = path.join(__dirname, "fixtures");
const EVIDENCE_DIR = path.join(FIXTURES, "evidence");
const NOW = 1799920800;
const EXPIRY = 1800000000;
const CREATED = 1799913600;
const BPF_LOADER = "BPFLoaderUpgradeab1e11111111111111111111111";
const ED25519_PKCS8_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");

const enc = (byte) => submit.base58Encode(Buffer.alloc(32, byte));
const PROGRAM_ID = enc(82);
const CONFIG_ID = enc(24);
const SIGNATURE = submit.base58Encode(Buffer.alloc(64, 9));

function tmpDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "rugcall-fixes-test-"));
}

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
  return { keypairPath, bytes, pubkeyBytes, signer: submit.loadResolverSigner(keypairPath) };
}

function spySigner(realSigner) {
  const spy = {
    pubkey: realSigner.pubkey,
    pubkeyBytes: realSigner.pubkeyBytes,
    signCalls: 0,
    sign(msg) { spy.signCalls += 1; return realSigner.sign(msg); },
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

function configBytes(resolverPubkeyBytes) {
  const data = Buffer.alloc(122);
  data[0] = 1;
  Buffer.alloc(32, 10).copy(data, 1);
  Buffer.from(resolverPubkeyBytes).copy(data, 33);
  Buffer.alloc(32, 11).copy(data, 65);
  return data;
}

function goodConfigInfo(pubkeyBytes) {
  return { executable: false, owner: PROGRAM_ID, lamports: 1000000, data: configBytes(pubkeyBytes) };
}

function marketAccountInfo(mutate) {
  const data = Buffer.alloc(142);
  data[0] = 2;
  Buffer.alloc(32, 25).copy(data, 1);
  Buffer.alloc(32, 23).copy(data, 33);
  data.writeBigInt64LE(BigInt(CREATED), 65);
  data.writeBigInt64LE(BigInt(EXPIRY), 73);
  data[97] = 1;
  if (mutate) mutate(data);
  return { executable: false, owner: PROGRAM_ID, lamports: 1000000, data };
}

function makeChain(o) {
  o = o || {};
  const chain = {
    calls: { getSignatureStatus: 0, sendRawTransaction: 0 },
    async getHealth() { return "ok"; },
    async getAccountInfo(pubkey) {
      if (pubkey === PROGRAM_ID) return { executable: true, owner: BPF_LOADER, lamports: 1000000, data: Buffer.alloc(0) };
      if (pubkey === CONFIG_ID) return o.configInfo !== undefined ? o.configInfo : null;
      if (pubkey === enc(21)) return o.marketInfo !== undefined ? o.marketInfo : marketAccountInfo();
      return null;
    },
    async getBalance() { return 100000000; },
    async getLatestBlockhash() { return enc(3); },
    async sendRawTransaction() { chain.calls.sendRawTransaction += 1; return SIGNATURE; },
    async getSignatureStatus() {
      chain.calls.getSignatureStatus += 1;
      if (o.onStatus) o.onStatus();
      return o.status !== undefined ? o.status : { confirmationStatus: "confirmed" };
    },
  };
  return chain;
}

// One open market with real 32-byte keys, matching marketAccountInfo.
function liveMarketsObj(signerPubkey) {
  const markets = JSON.parse(fs.readFileSync(path.join(FIXTURES, "markets.json"), "utf8"));
  const template = markets.markets.find((m) => m.evidence_file === "h2-bundle.json" && m.state === 1);
  const market = {
    ...structuredClone(template),
    address: enc(21),
    live_market: enc(22),
    creator: enc(23),
    subject_mint: enc(25),
  };
  return { ...markets, program_id: null, config: CONFIG_ID, resolver: signerPubkey, markets: [market] };
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
    programId: PROGRAM_ID,
    rpcUrl: "https://api.devnet.solana.com",
  });
  return { runner, records };
}

function liveSetup(o) {
  o = o || {};
  const dir = tmpDir();
  const kp = makeKeypair(dir, o.seedByte || 61);
  const signer = spySigner(kp.signer);
  const submitter = spySubmitter(o.submitterImpl);
  const chain = makeChain({ configInfo: goodConfigInfo(kp.pubkeyBytes), ...(o.chainOverrides || {}) });
  const ledgerPath = o.ledgerPath || path.join(dir, "ledger.json");
  const { runner, records } = makeRunner({ chain, signer, submitter, keypairPath: kp.keypairPath, ledgerPath });
  return { dir, kp, signer, submitter, chain, ledgerPath, runner, records, marketsObj: liveMarketsObj(signer.pubkey) };
}

// ---------------------------------------------------------------- fix 1

test("fix1a: dry-run decision on a shared ledger does not block a later live pass", async () => {
  const s = liveSetup();
  const marketsObj = s.marketsObj;
  const market = marketsObj.markets[0];

  // Dry-run pass first, on the same ledger file the live pass will use.
  const dry = createResolver({
    resolverPubkey: s.signer.pubkey,
    sink: () => {},
    now: NOW,
    outDir: tmpDir(),
    evidenceDir: EVIDENCE_DIR,
    ledgerPath: s.ledgerPath,
    crewStore: ev.emptyCrewStore(),
  });
  const dryRecords = dry.runPass(structuredClone(marketsObj));
  assert.equal(dryRecords.filter((r) => r.action === "WOULD_RESOLVE").length, 1);
  const dryLedger = JSON.parse(fs.readFileSync(s.ledgerPath, "utf8"));
  assert.equal(dryLedger[market.address].action, "WOULD_RESOLVE");
  assert.equal(dryLedger[market.address].mode, "dry-run");

  // Live pass: the dry-run WOULD_* entry is not final for live mode.
  const outcome = await s.runner.runLivePass(structuredClone(marketsObj));
  assert.equal(outcome.exitCode, 0, JSON.stringify(s.records));
  assert.equal(s.submitter.calls.length, 1, "live pass must still submit");
  const liveLedger = JSON.parse(fs.readFileSync(s.ledgerPath, "utf8"));
  assert.equal(liveLedger[market.address].mode, "live");
  assert.equal(liveLedger[market.address].submitted, true);
  assert.equal(liveLedger[market.address].signature, SIGNATURE);
});

test("fix1b: a dry-run pass leaves a SUBMITTED_UNCONFIRMED entry and its signature intact", () => {
  const marketsObj = liveMarketsObj("Dm95oxQp8YghEsCads1SXWkGwy4BaYLjCYv4fieG7GtH");
  const market = marketsObj.markets[0];
  const dir = tmpDir();
  const ledgerPath = path.join(dir, "ledger.json");
  const seeded = {
    [market.address]: {
      action: "SUBMITTED_UNCONFIRMED",
      mode: "live",
      pending_action: "WOULD_RESOLVE",
      outcome: 2,
      hard_trigger: 0,
      evidence_hash: "ab".repeat(32),
      bundle_path: null,
      submitted: true,
      signature: SIGNATURE,
      decided_at: NOW - 7200,
      next_retry_at: 0,
      repoll_count: 1,
    },
  };
  fs.writeFileSync(ledgerPath, `${JSON.stringify(seeded, null, 2)}\n`);

  const dry = createResolver({
    resolverPubkey: "Dm95oxQp8YghEsCads1SXWkGwy4BaYLjCYv4fieG7GtH",
    sink: () => {},
    now: EXPIRY,
    outDir: tmpDir(),
    evidenceDir: EVIDENCE_DIR,
    ledgerPath,
    crewStore: ev.emptyCrewStore(),
  });
  const records = dry.runPass(structuredClone(marketsObj));
  assert.ok(records.some((r) => r.market === market.address && r.action === "SKIP_LEDGER"));
  assert.equal(records.filter((r) => r.action.startsWith("WOULD_")).length, 0);
  const after = JSON.parse(fs.readFileSync(ledgerPath, "utf8"));
  assert.deepEqual(after, seeded, "the submitted entry must survive the dry-run pass untouched");
});

// ---------------------------------------------------------------- fix 2

test("fix2: a polling RPC error after broadcast rethrows with the signature attached", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 62);
  const chain = {
    async getLatestBlockhash() { return enc(3); },
    async sendRawTransaction() { return SIGNATURE; },
    async getSignatureStatus() { throw new Error("poll RPC boom"); },
  };
  const submitter = submit.createSubmitter({ chain, signer: kp.signer, pollAttempts: 3, pollIntervalMs: 1 });
  await assert.rejects(
    submitter.submitInstruction({
      programId: PROGRAM_ID,
      instruction: { dataHex: "05", accounts: [{ pubkey: kp.signer.pubkey, signer: true, writable: false }] },
    }),
    (err) => {
      assert.equal(err.signature, SIGNATURE, "the broadcast signature must ride the error");
      assert.match(err.message, /poll RPC boom/);
      return true;
    }
  );
});

test("fix2: an on-chain status.err after broadcast rethrows with the signature attached", async () => {
  const dir = tmpDir();
  const kp = makeKeypair(dir, 63);
  const chain = {
    async getLatestBlockhash() { return enc(3); },
    async sendRawTransaction() { return SIGNATURE; },
    async getSignatureStatus() { return { confirmationStatus: "confirmed", err: { InstructionError: [0, "Custom"] } }; },
  };
  const submitter = submit.createSubmitter({ chain, signer: kp.signer, pollAttempts: 3, pollIntervalMs: 1 });
  await assert.rejects(
    submitter.submitInstruction({
      programId: PROGRAM_ID,
      instruction: { dataHex: "05", accounts: [{ pubkey: kp.signer.pubkey, signer: true, writable: false }] },
    }),
    (err) => {
      assert.equal(err.signature, SIGNATURE);
      assert.match(err.message, /failed on chain/);
      return true;
    }
  );
});

test("fix2: live runner persists a thrown signature as SUBMITTED_UNCONFIRMED, never SUBMIT_FAILED with null", async () => {
  const thrown = Object.assign(new Error("poll RPC boom"), { signature: SIGNATURE });
  const s = liveSetup({ submitterImpl: async () => { throw thrown; } });
  const outcome = await s.runner.runLivePass(structuredClone(s.marketsObj));
  assert.equal(outcome.exitCode, 4);
  const unconfirmed = s.records.filter((r) => r.action === "SUBMITTED_UNCONFIRMED");
  assert.equal(unconfirmed.length, 1);
  assert.equal(unconfirmed[0].signature, SIGNATURE);
  assert.equal(s.records.filter((r) => r.action === "SUBMIT_FAILED").length, 0);
  const ledger = JSON.parse(fs.readFileSync(s.ledgerPath, "utf8"));
  const entry = ledger[s.marketsObj.markets[0].address];
  assert.equal(entry.action, "SUBMITTED_UNCONFIRMED");
  assert.equal(entry.signature, SIGNATURE);
  assert.equal(entry.pending_action, "WOULD_RESOLVE");
  assert.equal(entry.mode, "live");
});

test("fix2: an unconfirmed signature goes terminal at the re-poll bound and stops resubmission", async () => {
  const s = liveSetup({ chainOverrides: { status: null } });
  const market = s.marketsObj.markets[0];
  const seeded = {
    [market.address]: {
      action: "SUBMITTED_UNCONFIRMED",
      mode: "live",
      pending_action: "WOULD_RESOLVE",
      outcome: 2,
      hard_trigger: 0,
      evidence_hash: "ab".repeat(32),
      bundle_path: null,
      submitted: true,
      signature: SIGNATURE,
      decided_at: NOW - 7200,
      next_retry_at: 0,
      repoll_count: UNCONFIRMED_MAX_REPOLLS - 1,
    },
  };
  fs.writeFileSync(s.ledgerPath, `${JSON.stringify(seeded, null, 2)}\n`);
  const outcome = await s.runner.runLivePass(structuredClone(s.marketsObj));
  assert.ok(s.records.some((r) => r.action === "SUBMIT_FAILED" && r.signature === SIGNATURE),
    `records ${JSON.stringify(s.records.map((r) => r.action))}`);
  assert.equal(s.submitter.calls.length, 0, "no resubmission once terminal");
  const ledger = JSON.parse(fs.readFileSync(s.ledgerPath, "utf8"));
  assert.equal(ledger[market.address].action, "SUBMIT_FAILED_FINAL");
  assert.equal(ledger[market.address].signature, SIGNATURE);
  assert.ok(s.records.some((r) => r.market === market.address && r.action === "SKIP_LEDGER"));
  assert.equal(outcome.exitCode, 0);
});

// ---------------------------------------------------------------- fix 3

test("fix3: signerFromBytes refuses a keypair whose stored pubkey does not match its seed", () => {
  const a = makeKeypair(tmpDir(), 64);
  const b = makeKeypair(tmpDir(), 65);
  const mismatched = Buffer.concat([a.bytes.subarray(0, 32), b.bytes.subarray(32, 64)]);
  assert.throws(() => submit.signerFromBytes(mismatched), /does not match/);
  // The matching file still loads, and a tampered file on disk is refused too.
  assert.equal(submit.signerFromBytes(a.bytes).pubkey, a.signer.pubkey);
  const dir = tmpDir();
  const badPath = path.join(dir, "bad.json");
  fs.writeFileSync(badPath, JSON.stringify([...mismatched]));
  assert.throws(() => submit.loadResolverSigner(badPath), /does not match/);
});

// ---------------------------------------------------------------- fix 4

function bundleFor(name) {
  const evidence = JSON.parse(fs.readFileSync(path.join(EVIDENCE_DIR, `${name}.json`), "utf8"));
  const markets = JSON.parse(fs.readFileSync(path.join(FIXTURES, "markets.json"), "utf8"));
  const market = markets.markets.find((m) => m.evidence_file === `${name}.json`) || markets.markets[0];
  const store = ev.emptyCrewStore();
  const result = ev.evaluateEvidence(evidence, store);
  const bundle = ev.buildBundle({
    market, subjectMint: evidence.mint, resolver: market.resolver || "Dm95oxQp8YghEsCads1SXWkGwy4BaYLjCYv4fieG7GtH",
    expirySec: EXPIRY, fetchedAtSec: evidence.fetchedAtSec, evidence, crewStore: store, result,
  });
  return { bundle, result };
}

test("fix4: a well-formed bundle passes the extended checks, with complete reported not-recorded", () => {
  const { bundle } = bundleFor("h2-bundle");
  const verified = ev.verifyBundle(bundle, ev.hashBundle(bundle).hex);
  assert.equal(verified.hashOk, true);
  assert.equal(verified.verdictOk, true);
  assert.equal(verified.checks.partial, true);
  assert.equal(verified.checks.resolutionBlockers, true);
  assert.equal(verified.checks.complete, "not recorded in bundle");
  assert.equal(verified.checks.qualifyingSplit, true);
  assert.deepEqual(verified.qualifyingTriggers, [2]);
  assert.deepEqual(verified.excludedTriggers, []);
  assert.equal(verified.checksOk, true);
});

test("fix4: a bundle whose only fired trigger is after expiry is not qualifying-verified", () => {
  const { bundle } = bundleFor("late-trigger");
  const verified = ev.verifyBundle(bundle, ev.hashBundle(bundle).hex);
  assert.equal(verified.hashOk, true);
  assert.equal(verified.verdictOk, true, "the verdict itself still re-derives");
  assert.equal(verified.qualifyingOk, false);
  assert.equal(verified.checks.qualifyingSplit, false);
  assert.deepEqual(verified.qualifyingTriggers, []);
  assert.deepEqual(verified.excludedTriggers, [{ id: 3, eventTimestamp: EXPIRY + 3600 }]);
  assert.equal(verified.checksOk, false);
});

test("fix4: a completeness inconsistency in the result block is caught", () => {
  const { bundle } = bundleFor("partial-sells");
  assert.deepEqual(bundle.result.resolutionBlockers, ["sellsUnchecked"]);
  const tampered = structuredClone(bundle);
  tampered.result.resolutionBlockers = [];
  tampered.result.partial = true; // re-derived partial is false for this fixture
  const verified = ev.verifyBundle(tampered, ev.hashBundle(tampered).hex);
  assert.equal(verified.checks.resolutionBlockers, false);
  assert.equal(verified.checks.partial, false);
  assert.equal(verified.checksOk, false);
});

// ---------------------------------------------------------------- fix 5

test("fix5: a funder-step failure surfaces in fetchErrors.funders and reads PARTIAL", async () => {
  const MINT = "MintFix5Test11111111111111111111111111111111";
  const CREATOR = "CreatorFix5Test1111111111111111111111111111";
  const BUYER = "BuyerFix5Test111111111111111111111111111111";
  const T0 = CREATED;
  const rpc = async (url, method, params) => {
    const target = params && params[0];
    switch (method) {
      case "getTokenLargestAccounts":
        return { result: { value: [{ address: "HolderFix5Acct11111111111111111111111111111", amount: "600" }] } };
      case "getTokenSupply":
        return { result: { value: { amount: "1000", decimals: 6 } } };
      case "getSignaturesForAddress":
        if (target === MINT) return { result: [{ signature: "sigEarly", slot: 100, blockTime: T0 }] };
        if (target === BUYER) return { error: { code: -32429, message: "429 rate limited" } };
        return { result: [] };
      case "getTransaction":
        return { result: {
          slot: 100, blockTime: T0,
          transaction: { message: { accountKeys: [CREATOR, BUYER], instructions: [] } },
          meta: { preTokenBalances: [], innerInstructions: [],
            postTokenBalances: [{ accountIndex: 1, mint: MINT, owner: BUYER, uiTokenAmount: { amount: "500" } }] },
        } };
      case "getMultipleAccounts":
        return { result: { value: [null] } };
      case "getAccountInfo":
        return { result: { value: { data: { parsed: { info: { supply: "1000", decimals: 6, mintAuthority: null, freezeAuthority: null } } } } } };
      default:
        return { result: null };
    }
  };
  const fetched = await ev.fetchEvidence(MINT, {
    rpc, fetchJson: async () => ({ pairs: [] }), nowMs: () => (T0 + 3600) * 1000, rpcUrl: "http://fixture.invalid",
  });
  assert.equal(typeof fetched.fetchErrors.funders, "string",
    `fetchErrors ${JSON.stringify(fetched.fetchErrors)}`);
  assert.ok(fetched.fetchErrors.funders.length > 0);
  const result = ev.evaluateEvidence(fetched, ev.emptyCrewStore());
  assert.equal(result.partial, true, "the funder failure must read PARTIAL (fail-closed)");
});

// ---------------------------------------------------------------- fix 6

test("fix6: triggerDefiniteStates matches resolver semantics on the known edges", () => {
  // Bundle present with pct:null is still H2-definite (presence only).
  assert.deepEqual(
    ev.triggerDefiniteStates({ bundle: { pct: null }, funders: {}, history: null, sells: null, liquidity: null, missingCore: [] }),
    { H1: false, H2: true, H3: false, H4: false, H5: true }
  );
  // Numeric strikes make H1 definite even when missingCore lists
  // "history" (combined/funder history present, creator history missing).
  const states = ev.triggerDefiniteStates({
    bundle: {}, funders: {}, history: { strikes: 2 }, sells: { checked: true },
    liquidity: { state: "live" }, missingCore: ["history"],
  });
  assert.deepEqual(states, { H1: true, H2: true, H3: true, H4: true, H5: true });
  // The remaining branches.
  assert.equal(ev.triggerDefiniteStates({ history: { strikes: null } }).H1, false);
  assert.equal(ev.triggerDefiniteStates({ sells: { checked: false } }).H3, false);
  assert.equal(ev.triggerDefiniteStates({ liquidity: { state: "unknown" } }).H4, false);
  assert.equal(ev.triggerDefiniteStates({ funders: null, bundle: {} }).H2, false);
  assert.equal(ev.triggerDefiniteStates({ funders: null }).H5, false);
  assert.equal(ev.triggerDefiniteStates(null).H1, false);
});

// ---------------------------------------------------------------- fix 7

test("fix7: a market whose file expiry differs from the chain is refused before any signing", async () => {
  const s = liveSetup({
    chainOverrides: { marketInfo: marketAccountInfo((data) => data.writeBigInt64LE(BigInt(EXPIRY + 3600), 73)) },
  });
  const outcome = await s.runner.runLivePass(structuredClone(s.marketsObj));
  assert.equal(outcome.exitCode, 1);
  assert.equal(s.records.length, 1, `records ${JSON.stringify(s.records)}`);
  assert.equal(s.records[0].action, "REFUSAL");
  assert.equal(s.records[0].reason, "market-chain-mismatch");
  assert.match(s.records[0].live_preflight.detail, /expiry/);
  assert.equal(s.signer.signCalls, 0, "signer must not be called");
  assert.equal(s.submitter.calls.length, 0, "submitter must not be called");
});

test("fix7: a market whose file state differs from the chain is refused before any signing", async () => {
  const s = liveSetup({
    chainOverrides: { marketInfo: marketAccountInfo((data) => { data[97] = 2; }) },
  });
  const outcome = await s.runner.runLivePass(structuredClone(s.marketsObj));
  assert.equal(outcome.exitCode, 1);
  assert.equal(s.records.length, 1);
  assert.equal(s.records[0].reason, "market-chain-mismatch");
  assert.match(s.records[0].live_preflight.detail, /state/);
  assert.equal(s.signer.signCalls, 0);
  assert.equal(s.submitter.calls.length, 0);
});
