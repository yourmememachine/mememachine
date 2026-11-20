"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const predict = require("../predict.js");
const tx = require("../predict-tx.js");

/* The parimutuel vectors, pinned against the program helper in
   state.rs: payout = floor(winner_stake * total_pool / winning_pool). */
const VECTORS = [
  [100, 1000, 700, "142"],
  [600, 1000, 700, "857"],
  [50, 1000, 500, "100"],
  [2, 100, 3, "66"],
  [1, 10, 3, "3"],
  [700, 1000, 700, "1000"],
  [200, 500, 500, "200"],
  [1000000, 10000000, 3000000, "3333333"],
  [2000000, 10000000, 3000000, "6666666"],
  [0, 1000, 700, "0"],
  ["9223372036854775808", "18446744073709551615", "9223372036854775808", "18446744073709551615"],
];
for (const [stake, total, winning, expected] of VECTORS) {
  test(`payout(${stake}, ${total}, ${winning}) is ${expected}`, () => {
    assert.equal(tx.parimutuelPayout(String(stake), String(total), String(winning)), expected);
  });
}

const NULL_VECTORS = [
  [0, 1000, 0],
  [100, 1000, 0],
  ["18446744073709551615", "18446744073709551615", 1],
  [10, 5, 7],
  [800, 1000, 700],
];
for (const [stake, total, winning] of NULL_VECTORS) {
  test(`payout(${stake}, ${total}, ${winning}) has no answer`, () => {
    assert.equal(tx.parimutuelPayout(String(stake), String(total), String(winning)), null);
  });
}

function market(over) {
  return Object.assign({
    address: "Mkt1111111111111111111111111111111111111111",
    flaggedPool: "3000000", survivesPool: "7000000", totalPool: "10000000",
    state: 2, outcome: 1, expiry: 1800000000, createdAt: 1799913600,
  }, over || {});
}
function position(over) {
  return Object.assign({
    address: "Pos1111111111111111111111111111111111111111",
    flaggedAmount: "0", survivesAmount: "0", claimed: false,
  }, over || {});
}
const CONFIG = { minStake: "1000000" };
const OPEN = { state: 1, outcome: 0 };

test("odds of an empty market are null, never 50/50", () => {
  const o = predict.computeOdds(market({ flaggedPool: "0", survivesPool: "0" }));
  assert.equal(o.totalPool, "0");
  assert.equal(o.flaggedBps, null);
  assert.equal(o.survivesBps, null);
  assert.equal(o.flaggedPct, null);
  assert.equal(o.survivesPct, null);
  assert.equal(o.oneSided, false);
});

test("odds split 3M/7M as 3000/7000 bps", () => {
  const o = predict.computeOdds(market({}));
  assert.equal(o.totalPool, "10000000");
  assert.equal(o.flaggedBps, 3000);
  assert.equal(o.survivesBps, 7000);
  assert.equal(o.flaggedPct, 30);
  assert.equal(o.survivesPct, 70);
  assert.equal(o.oneSided, false);
});

test("odds floor at 1/3 to 3333/6666 bps", () => {
  const o = predict.computeOdds(market({ flaggedPool: "1", survivesPool: "2" }));
  assert.equal(o.flaggedBps, 3333);
  assert.equal(o.survivesBps, 6666);
});

test("one-sided odds are 10000/0 and flagged one-sided", () => {
  const o = predict.computeOdds(market({ flaggedPool: "50000000", survivesPool: "0" }));
  assert.equal(o.flaggedBps, 10000);
  assert.equal(o.survivesBps, 0);
  assert.equal(o.oneSided, true);
});

test("buy preview computes payouts against the post-buy pools", () => {
  const m = market(Object.assign({}, OPEN, { flaggedPool: "3000000", survivesPool: "7000000" }));
  const r = predict.previewBuy(m, null, 1, "1000000", CONFIG, 1799950000);
  assert.equal(r.kind, "ok");
  // Post-buy pools 4M/7M, total 11M: floor(1M * 11M / 4M).
  assert.equal(r.payoutOnThisBuyAlone, "2750000");
  assert.equal(r.projectedPositionPayout, "2750000");
});

test("buy preview projects the whole position, not just this buy", () => {
  const m = market(Object.assign({}, OPEN, { flaggedPool: "3000000", survivesPool: "7000000" }));
  const p = position({ flaggedAmount: "1000000" });
  const r = predict.previewBuy(m, p, 1, "1000000", CONFIG, 1799950000);
  assert.equal(r.payoutOnThisBuyAlone, "2750000");
  // Whole flagged stake after the buy is 2M: floor(2M * 11M / 4M).
  assert.equal(r.projectedPositionPayout, "5500000");
});

test("buy preview refuses an invalid side", () => {
  const m = market(OPEN);
  assert.equal(predict.previewBuy(m, null, 0, "1000000", CONFIG, 1799950000).reason, "invalid-side");
  assert.equal(predict.previewBuy(m, null, 3, "1000000", CONFIG, 1799950000).reason, "invalid-side");
});

test("buy preview refuses a stake below the minimum", () => {
  const m = market(OPEN);
  const r = predict.previewBuy(m, null, 1, "999999", CONFIG, 1799950000);
  assert.equal(r.kind, "refused");
  assert.equal(r.reason, "below-min-stake");
  assert.equal(predict.previewBuy(m, null, 1, "0", CONFIG, 1799950000).reason, "below-min-stake");
});

test("buy preview refuses a market that is not open", () => {
  assert.equal(predict.previewBuy(market({ state: 2 }), null, 1, "1000000", CONFIG, 1799950000).reason, "market-not-open");
  assert.equal(predict.previewBuy(market({ state: 3 }), null, 1, "1000000", CONFIG, 1799950000).reason, "market-not-open");
});

test("buy preview refuses at and after expiry", () => {
  const m = market(OPEN);
  assert.equal(predict.previewBuy(m, null, 1, "1000000", CONFIG, 1800000000).reason, "at-or-after-expiry");
  assert.equal(predict.previewBuy(m, null, 1, "1000000", CONFIG, 1800000001).reason, "at-or-after-expiry");
  assert.equal(predict.previewBuy(m, null, 1, "1000000", CONFIG, 1799999999).kind, "ok");
});

test("claim preview: no position is none", () => {
  assert.deepEqual(predict.computeClaimPreview(market({}), null), { kind: "none", reason: "no-position" });
});

test("claim preview: canonical winner is paid 3333333", () => {
  const r = predict.computeClaimPreview(market({}), position({ flaggedAmount: "1000000" }));
  assert.equal(r.kind, "payout");
  assert.equal(r.amountRaw, "3333333");
});

test("claim preview: loser has no claim", () => {
  const r = predict.computeClaimPreview(market({}), position({ survivesAmount: "5000000" }));
  assert.deepEqual(r, { kind: "no-claim", reason: "no-winning-stake" });
});

test("claim preview: hedged winner is paid on the winning side only", () => {
  const r = predict.computeClaimPreview(market({}), position({ flaggedAmount: "2000000", survivesAmount: "2000000" }));
  assert.equal(r.kind, "payout");
  assert.equal(r.amountRaw, "6666666");
});

test("claim preview: already claimed is none", () => {
  const r = predict.computeClaimPreview(market({}), position({ flaggedAmount: "1000000", claimed: true }));
  assert.deepEqual(r, { kind: "none", reason: "already-claimed" });
});

test("claim preview: open market is none, including past the grace window", () => {
  const open = market({ state: 1, outcome: 0 });
  assert.deepEqual(predict.computeClaimPreview(open, position({ flaggedAmount: "1000000" })), { kind: "none", reason: "market-open" });
  const stale = market({ state: 1, outcome: 0, expiry: 1000 });
  assert.equal(predict.marketPhase(stale, 1000 + 86401), "open-expired-past-grace");
  assert.deepEqual(predict.computeClaimPreview(stale, position({ flaggedAmount: "1000000" })), { kind: "none", reason: "market-open" });
});

test("claim preview: cancelled hedged position refunds the total stake", () => {
  const m = market({ state: 3, outcome: 0, flaggedPool: "4000000", survivesPool: "6000000" });
  const r = predict.computeClaimPreview(m, position({ flaggedAmount: "1500000", survivesAmount: "2500000" }));
  assert.deepEqual(r, { kind: "refund", amountRaw: "4000000", reason: "cancelled" });
});

test("claim preview: cancelled zero-stake position has no claim", () => {
  const m = market({ state: 3, outcome: 0 });
  const r = predict.computeClaimPreview(m, position({ flaggedAmount: "0", survivesAmount: "0" }));
  assert.deepEqual(r, { kind: "no-claim", reason: "zero-total-stake" });
});

test("claim preview: zero winning pool refunds a losing holder in full", () => {
  const m = market({ state: 2, outcome: 1, flaggedPool: "0", survivesPool: "5000000" });
  const r = predict.computeClaimPreview(m, position({ survivesAmount: "2000000" }));
  assert.deepEqual(r, { kind: "refund", amountRaw: "2000000", reason: "zero-winning-pool" });
});

test("claim preview: zero winning pool with zero total stake has no claim", () => {
  const m = market({ state: 2, outcome: 1, flaggedPool: "0", survivesPool: "5000000" });
  const r = predict.computeClaimPreview(m, position({ flaggedAmount: "0", survivesAmount: "0" }));
  assert.deepEqual(r, { kind: "no-claim", reason: "zero-total-stake" });
});

test("claim preview: one-sided winner is paid the exact stake through the payout branch", () => {
  const m = market({ state: 2, outcome: 2, flaggedPool: "0", survivesPool: "5000000" });
  const r = predict.computeClaimPreview(m, position({ survivesAmount: "2000000" }));
  assert.equal(r.kind, "payout");
  assert.equal(r.amountRaw, "2000000");
});

test("claim preview: resolved market with no outcome is an inconsistent state", () => {
  const m = market({ state: 2, outcome: 0 });
  const r = predict.computeClaimPreview(m, position({ flaggedAmount: "1000000" }));
  assert.deepEqual(r, { kind: "error", reason: "inconsistent-state" });
});

test("formatDusdc renders six decimal places", () => {
  assert.equal(predict.formatDusdc("0"), "0.000000");
  assert.equal(predict.formatDusdc("1"), "0.000001");
  assert.equal(predict.formatDusdc("1000000"), "1.000000");
  assert.equal(predict.formatDusdc("1234567"), "1.234567");
  assert.equal(predict.formatDusdc("18446744073709551615"), "18446744073709.551615");
});

test("parseDusdcToRaw is the strict inverse of formatDusdc", () => {
  assert.equal(predict.parseDusdcToRaw("0"), "0");
  assert.equal(predict.parseDusdcToRaw("1"), "1000000");
  assert.equal(predict.parseDusdcToRaw("1.234567"), "1234567");
  assert.equal(predict.parseDusdcToRaw("0.000001"), "1");
  assert.equal(predict.parseDusdcToRaw(predict.formatDusdc("987654321")), "987654321");
});

test("parseDusdcToRaw rejects non-canonical amounts", () => {
  for (const bad of ["", "1.1234567", "1e3", "-1", "+1", ".5", "1.", " 1", "1 ", "01", "1,000", "abc"]) {
    assert.equal(predict.parseDusdcToRaw(bad), null, JSON.stringify(bad));
  }
  assert.equal(predict.parseDusdcToRaw(5), null);
  assert.equal(predict.parseDusdcToRaw("18446744073709.551616"), null); // one raw unit above u64 max
});
