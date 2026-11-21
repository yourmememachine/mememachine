"use strict";

/* Tests for the judge-page review fixes:
 * - explicit fixture mode in createDataSource (the program id is a
 *   display/PDA binding, not a source selector),
 * - the pinned fixture clock,
 * - computeClaimPreview precedence (market-open first),
 * - predict-ui fallback claim preview / odds mirroring the program,
 * - S5 renderProgramSection linking only the program id,
 * - evidenceTriggerStates consuming the resolver's
 *   triggerDefiniteStates presence-level semantics,
 * - the in-page bundle hash recompute primitive.
 */

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const predict = require("../predict.js");
const tx = require("../predict-tx.js");
const ui = require("../predict-ui.js");
const ev = require("../evidence.js");
const judgePage = require("../build-judge-page.js");

const APP_DIR = path.join(__dirname, "..");
const BUNDLE = JSON.parse(
  fs.readFileSync(path.join(__dirname, "fixtures", "app-markets.json"), "utf8"));
const REPLAY = JSON.parse(fs.readFileSync(path.join(APP_DIR, "replay.json"), "utf8"));
const REAL_PROGRAM_ID = "DcwvEDi4yCk16wQ2HVzc3dQiBFccj8VxzboGUYdUXseZ";

/* ---------------- fix 1: explicit fixture mode ---------------- */

test("fix1: explicit fixture mode serves the board with the REAL program id bound", async () => {
  const src = predict.createDataSource({
    programId: REAL_PROGRAM_ID, fixtureBundle: BUNDLE, mode: "fixture",
  });
  assert.equal(src.kind, "fixture");
  assert.equal(src.programId, REAL_PROGRAM_ID);
  const markets = await src.listMarkets();
  assert.equal(markets.length, 8);
  const cfg = await src.getConfig();
  assert.equal(cfg.resolver, BUNDLE.config.resolver);
  assert.equal(cfg.minStake, "1000000");
});

test("fix1: without an explicit mode the legacy selection is unchanged", async () => {
  assert.equal(predict.createDataSource({ programId: null, fixtureBundle: BUNDLE }).kind, "fixture");
  const dev = predict.createDataSource({ programId: REAL_PROGRAM_ID, transport: {} });
  assert.equal(dev.kind, "devnet");
  await assert.rejects(dev.listMarkets(), (err) => err.code === "NOT_CONFIGURED" || err.code === "RPC_FAILED");
  assert.equal(predict.createDataSource({
    programId: REAL_PROGRAM_ID, fixtureBundle: BUNDLE, mode: "devnet", transport: {},
  }).kind, "devnet");
});

/* ---------------- fix 8: pinned fixture clock ---------------- */

test("fix8: the fixture source clock is pinned to the fixture epoch", async () => {
  const src = predict.createDataSource({
    programId: REAL_PROGRAM_ID, fixtureBundle: BUNDLE, mode: "fixture",
  });
  assert.equal(src.nowSec, predict.FIXTURE_NOW_SEC);
  const markets = await src.listMarkets();
  const open = markets.find((m) => m.address === BUNDLE.markets[0].address);
  // Inside the 24h term: open-live with less than a day left,
  // never a ~55-day countdown and never 1970.
  assert.equal(predict.marketPhase(open, src.nowSec), "open-live");
  const remaining = open.expiry - src.nowSec;
  assert.ok(remaining > 0 && remaining < 86400, `remaining ${remaining}`);
  assert.ok(src.nowSec > open.createdAt && src.nowSec < open.expiry);
});

test("fix8: fixture timestamps are recorded provenance, untouched by the pin", () => {
  assert.equal(BUNDLE.markets[0].created_at, 1799913600);
  assert.equal(BUNDLE.markets[0].expiry, 1800000000);
  assert.equal(REPLAY.market.created_at, 1799913600);
});

/* ---------------- fix 6: claim preview precedence ---------------- */

function claimMarket(over) {
  return Object.assign({
    address: "Mkt1111111111111111111111111111111111111111",
    flaggedPool: "3000000", survivesPool: "7000000", totalPool: "10000000",
    state: 2, outcome: 1, expiry: 1800000000, createdAt: 1799913600,
  }, over || {});
}

test("fix6: market-open is checked FIRST, before position existence and claimed", () => {
  const open = claimMarket({ state: 1, outcome: 0 });
  assert.deepEqual(predict.computeClaimPreview(open, null),
    { kind: "none", reason: "market-open" });
  assert.deepEqual(predict.computeClaimPreview(open, { flaggedAmount: "1", survivesAmount: "0", claimed: true }),
    { kind: "none", reason: "market-open" });
  assert.deepEqual(predict.computeClaimPreview(null, null),
    { kind: "none", reason: "market-open" });
});

test("fix6: after the open check, position existence precedes claimed", () => {
  const resolved = claimMarket({});
  assert.deepEqual(predict.computeClaimPreview(resolved, null),
    { kind: "none", reason: "no-position" });
  assert.deepEqual(predict.computeClaimPreview(resolved, { flaggedAmount: "1", survivesAmount: "0", claimed: true }),
    { kind: "none", reason: "already-claimed" });
});

/* ---------------- fix 7: predict-ui fallbacks mirror the program ---------------- */

function uiMarket(over) {
  return Object.assign({
    address: "Mkt1111111111111111111111111111111111111111",
    flaggedPool: "3000000", survivesPool: "7000000", totalPool: "10000000",
    state: 2, stateName: "resolved", outcome: 1, outcomeName: "flagged",
    expiry: 1800000000, createdAt: 1799913600,
  }, over || {});
}

test("fix7: fallback claim preview refunds in full when the winning pool is zero, even for a losing holder", () => {
  const m = uiMarket({ flaggedPool: "0", survivesPool: "5000000", totalPool: "5000000" });
  const p = { flaggedAmount: "0", survivesAmount: "2000000", claimed: false };
  const r = ui.fallbackClaimPreview(m, p);
  assert.equal(r.kind, "refund");
  assert.equal(r.amountRaw, "2000000");
  assert.equal(r.reason, "zero-winning-pool");
});

test("fix7: fallback claim preview pays winning-side stake via the payout branch when the winning pool is the whole pool", () => {
  const m = uiMarket({
    outcome: 2, outcomeName: "survives",
    flaggedPool: "0", survivesPool: "5000000", totalPool: "5000000",
  });
  // Hedged position: the program pays the winning-side stake
  // (2000000), NOT a refund of both sides' stake (3000000).
  const p = { flaggedAmount: "1000000", survivesAmount: "2000000", claimed: false };
  const r = ui.fallbackClaimPreview(m, p);
  assert.equal(r.kind, "payout");
  assert.equal(r.amountRaw, "2000000");
});

test("fix7: fallback odds use the canonical floored bps pair", () => {
  const o = ui.fallbackComputeOdds({ flaggedPool: "1", survivesPool: "2" });
  assert.equal(o.flaggedBps, 3333);
  assert.equal(o.survivesBps, 6666);
});

/* ---------------- fix 3: S5 links only the program id ---------------- */

test("fix3: renderProgramSection links only the program id; market and resolver are labelled fixture text", () => {
  const html = judgePage.renderProgramSection(REAL_PROGRAM_ID, REPLAY);
  const hrefs = [...html.matchAll(/href="([^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual(hrefs, [
    "https://explorer.solana.com/address/" + REAL_PROGRAM_ID + "?cluster=devnet",
  ]);
  assert.ok(html.includes(REPLAY.market.address), "replay market shown as text");
  assert.ok(html.includes(REPLAY.record.resolver), "resolver shown as text");
  assert.ok(/fixture \(replay record\)/.test(html), "fixture label present");
  assert.ok(!html.includes("address/" + REPLAY.market.address));
  assert.ok(!html.includes("address/" + REPLAY.record.resolver));
});

/* ---------------- fix 4: honesty copy ---------------- */

test("fix4: fixture label and banner describe recorded snapshots and a deployed program", () => {
  assert.ok(!/not deployed yet/i.test(predict.FIXTURE_DATA_LABEL));
  assert.ok(/deployed on devnet/i.test(predict.FIXTURE_DATA_LABEL));
  assert.ok(/recorded snapshots/i.test(predict.FIXTURE_BANNER));
  assert.ok(!/read live/i.test(predict.FIXTURE_BANNER));
  const index = fs.readFileSync(path.join(APP_DIR, "index.html"), "utf8");
  assert.ok(!/evidence read live/i.test(index));
  assert.ok(!/evidence is read live/i.test(index));
  assert.ok(/recorded snapshots/i.test(index));
});

/* ---------------- fix 10: in-page bundle hash recompute ---------------- */

test("fix10: the shipped SHA-256 recomputes the replay bundle hash in-page", () => {
  assert.equal(tx.sha256Hex(REPLAY.bundle.canonical), REPLAY.bundle.sha256);
  assert.equal(REPLAY.verify.hashOk, true);
});

/* ---------------- fix 13: single-source definiteness ---------------- */

test("fix13: evidence.js exports triggerDefiniteStates with presence-level semantics", () => {
  assert.equal(typeof ev.triggerDefiniteStates, "function");
  assert.deepEqual(
    ev.triggerDefiniteStates({ bundle: true, funders: true, history: { strikes: 2 }, sells: { checked: true }, liquidity: { state: "live" }, missingCore: [] }),
    { H1: true, H2: true, H3: true, H4: true, H5: true });
  assert.deepEqual(
    ev.triggerDefiniteStates({ bundle: null, funders: null, history: null, sells: null, liquidity: null, missingCore: ["bundle", "funders", "history"] }),
    { H1: false, H2: false, H3: false, H4: false, H5: false });
});

function evalResultWith(over) {
  const base = {
    hardTriggers: [
      { id: 1, fired: false, values: { strikes: 0 } },
      { id: 2, fired: false, values: { pct: 10, sharedFunderInWindow: false } },
      { id: 3, fired: false, values: { checked: true } },
      { id: 4, fired: false, values: { state: "live" } },
      { id: 5, fired: false, values: { wallets: [] } },
    ],
    missingCore: [],
  };
  return Object.assign(base, over || {});
}

test("fix13 edge (a): bundle present with pct:null still makes H2 definite", () => {
  const r = evalResultWith({});
  r.hardTriggers[1].values = { pct: null, sharedFunderInWindow: null };
  const states = predict.evidenceTriggerStates(r);
  assert.equal(states[1], "not-fired");
});

test("fix13 edge (b): numeric strikes with 'history' in missingCore still makes H1 definite", () => {
  const r = evalResultWith({ missingCore: ["history"] });
  r.hardTriggers[0].values = { strikes: 3 };
  const states = predict.evidenceTriggerStates(r);
  assert.equal(states[0], "not-fired");
  // And the resolver's own export agrees on the same families.
  assert.equal(ev.triggerDefiniteStates({
    bundle: true, funders: true, history: { strikes: 3 },
    sells: { checked: true }, liquidity: { state: "live" }, missingCore: ["history"],
  }).H1, true);
});

test("fix13: absent families still read unknown through predict.evidenceTriggerStates", () => {
  const r = evalResultWith({ missingCore: ["bundle", "funders"] });
  r.hardTriggers[0].values = { strikes: null };
  r.hardTriggers[2].values = { checked: false };
  r.hardTriggers[3].values = { state: "unknown" };
  assert.deepEqual(predict.evidenceTriggerStates(r),
    ["unknown", "unknown", "unknown", "unknown", "unknown"]);
});
