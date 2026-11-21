"use strict";

// Tests for rugcall/resolver/gen-replay.js: the deterministic replay
// artifact built from the H2 fixture market with the standard pools
// from the program's program-test vector.

const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const ev = require("../../app/evidence.js");
const { generateReplay, computeClaim, H2_MARKET_ADDRESS } = require("../gen-replay.js");

const FIXTURES = path.join(__dirname, "fixtures");
const MARKETS_PATH = path.join(FIXTURES, "markets.json");
const EVIDENCE_DIR = path.join(FIXTURES, "evidence");
const FIXED_NOW = 1799920800;

function tmpDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "rugcall-replay-test-"));
}

function genToTemp() {
  const dir = tmpDir();
  const outPath = path.join(dir, "replay.json");
  const bundleOutDir = path.join(dir, "bundles");
  const replay = generateReplay({
    marketsPath: MARKETS_PATH,
    evidenceDir: EVIDENCE_DIR,
    outPath,
    bundleOutDir,
  });
  return {
    dir,
    replay,
    replayBytes: fs.readFileSync(outPath),
    bundleBytes: fs.readFileSync(
      path.join(bundleOutDir, `${H2_MARKET_ADDRESS}.json`)
    ),
  };
}

test("replay: generator resolves H2 early as Flagged trigger 2 at the fixed clock", () => {
  const { replay } = genToTemp();
  assert.equal(replay.record.ts, FIXED_NOW);
  assert.ok(FIXED_NOW < replay.market.expiry, "resolution is before expiry");
  assert.equal(replay.record.action, "WOULD_RESOLVE");
  assert.equal(replay.record.outcome, 1);
  assert.equal(replay.record.hard_trigger, 2);
  assert.equal(replay.record.trigger_event_ts, 1799913660);
  assert.deepEqual(replay.record.qualifying_triggers, [2]);
});

test("replay: embedded bundle re-verifies and its sha256 matches the record hashes", () => {
  const { replay, bundleBytes } = genToTemp();
  const bundle = JSON.parse(replay.bundle.canonical);
  const verify = ev.verifyBundle(bundle, replay.record.evidence_hash);
  assert.equal(verify.hashOk, true);
  assert.equal(verify.verdictOk, true);
  assert.deepEqual(replay.verify, { hashOk: true, verdictOk: true });
  const canonicalSha = crypto
    .createHash("sha256")
    .update(replay.bundle.canonical, "utf8")
    .digest("hex");
  assert.equal(canonicalSha, replay.record.evidence_hash);
  assert.equal(canonicalSha, replay.record.bundle_sha256);
  assert.equal(replay.bundle.sha256, canonicalSha);
  assert.equal(bundleBytes.toString("utf8"), replay.bundle.canonical);
});

test("replay: claim amounts match the standard pools and the unit vectors", () => {
  const { replay } = genToTemp();
  const byWallet = Object.fromEntries(
    replay.claims.map((c) => [c.wallet, c])
  );
  assert.equal(byWallet.Alice.payout, "3333333");
  assert.equal(byWallet.Bob.payout, "6666666");
  assert.equal(byWallet.Carol.payout, "0");
  assert.equal(byWallet.Carol.program_error, 19);
  assert.equal(byWallet.Carol.program_error_name, "NoClaimableStake");
  assert.equal(replay.dust, "1");
  assert.equal(computeClaim(100, 1000, 700), 142n);
  assert.equal(computeClaim(600, 1000, 700), 857n);
  assert.equal(computeClaim("1000000", "10000000", "3000000"), 3333333n);
});

test("replay: regeneration is deterministic across output directories", () => {
  const a = genToTemp();
  const b = genToTemp();
  assert.notEqual(a.dir, b.dir);
  assert.ok(a.replayBytes.equals(b.replayBytes), "replay.json bytes differ");
  assert.ok(a.bundleBytes.equals(b.bundleBytes), "bundle bytes differ");
});

test("replay: stakes and pools match the standard vector and the fixture is untouched", () => {
  const { replay } = genToTemp();
  assert.equal(replay.market.address, H2_MARKET_ADDRESS);
  assert.equal(replay.market.flagged_pool, "3000000");
  assert.equal(replay.market.survives_pool, "7000000");
  assert.deepEqual(replay.buys, [
    { wallet: "Alice", side: 1, stake: "1000000" },
    { wallet: "Bob", side: 1, stake: "2000000" },
    { wallet: "Carol", side: 2, stake: "7000000" },
  ]);
  assert.deepEqual(
    replay.claims.map((c) => [c.wallet, c.side, c.stake]),
    replay.buys.map((b) => [b.wallet, b.side, b.stake])
  );
  const fixture = JSON.parse(fs.readFileSync(MARKETS_PATH, "utf8"));
  const fixtureH2 = fixture.markets.find((m) => m.address === H2_MARKET_ADDRESS);
  assert.equal(fixtureH2.flagged_pool, "0");
  assert.equal(fixtureH2.survives_pool, "0");
  assert.equal(Object.keys(replay.summaries).length, 7);
  assert.deepEqual(replay.summaries[H2_MARKET_ADDRESS].hardTriggers, [2]);
  assert.equal(replay.summaries[H2_MARKET_ADDRESS].verdict, "FLAGGED");
});

test("replay: record honesty end to end", () => {
  const { replay } = genToTemp();
  assert.equal(replay.schema, "rugcall-replay/v1");
  assert.equal(replay.record.signed, false);
  assert.equal(replay.record.submitted, false);
  assert.equal(replay.record.program_id, null);
  assert.equal(replay.config.programId, null);
  assert.equal(replay.config.cluster, "devnet");
  for (const key of [
    "evidenceJsSha256",
    "resolverJsSha256",
    "marketsSha256",
    "evidenceFixtureSha256",
  ]) {
    assert.match(replay.generatedFrom[key], /^[0-9a-f]{64}$/);
  }
});

// The resolver fixture addresses feed the shipped judge page (via
// the replay and the app bundle built from the same fixture set).
// The readable label addresses used here originally decoded to 33
// bytes and broke the app client's PDA derivation on the shipped
// page; every address slot in this file must decode to exactly 32
// bytes, and each market's evidence fixture must name the market's
// subject mint (which must itself decode to 32 bytes).
test("fixture market addresses decode to exactly 32 bytes and match their evidence mints", () => {
  const tx = require("../../app/predict-tx.js");
  const marketsObj = JSON.parse(fs.readFileSync(MARKETS_PATH, "utf8"));
  for (const m of marketsObj.markets) {
    for (const field of ["address", "live_market", "subject_mint", "creator"]) {
      assert.equal(tx.base58Decode(m[field]).length, 32, `${m.evidence_file} ${field}`);
    }
    const evidence = JSON.parse(
      fs.readFileSync(path.join(EVIDENCE_DIR, m.evidence_file), "utf8"));
    assert.equal(evidence.mint, m.subject_mint, m.evidence_file);
    assert.equal(tx.base58Decode(evidence.mint).length, 32, m.evidence_file);
  }
});
