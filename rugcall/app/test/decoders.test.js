"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const predict = require("../predict.js");
const tx = require("../predict-tx.js");

function pub(byte) {
  return tx.base58Encode(new Uint8Array(32).fill(byte));
}

function buildMarket(over) {
  const o = Object.assign({
    subject: 4, creator: 5, createdAt: 1799913600n, expiry: 1800000000n,
    flagged: 3000000n, survives: 7000000n, state: 1, outcome: 0,
    hashByte: 7, trigger: 0, resolvedAt: 0n, bump: 253, vaultBump: 252,
  }, over || {});
  const b = Buffer.alloc(142);
  b[0] = 2;
  b.fill(o.subject, 1, 33);
  b.fill(o.creator, 33, 65);
  b.writeBigInt64LE(o.createdAt, 65);
  b.writeBigInt64LE(o.expiry, 73);
  b.writeBigUInt64LE(o.flagged, 81);
  b.writeBigUInt64LE(o.survives, 89);
  b[97] = o.state;
  b[98] = o.outcome;
  b.fill(o.hashByte, 99, 131);
  b[131] = o.trigger;
  b.writeBigInt64LE(o.resolvedAt, 132);
  b[140] = o.bump;
  b[141] = o.vaultBump;
  return b;
}

function buildPosition(over) {
  const o = Object.assign({
    market: 6, user: 7, flagged: 400n, survives: 600n, claimed: 0, bump: 251,
  }, over || {});
  const b = Buffer.alloc(83);
  b[0] = 3;
  b.fill(o.market, 1, 33);
  b.fill(o.user, 33, 65);
  b.writeBigUInt64LE(o.flagged, 65);
  b.writeBigUInt64LE(o.survives, 73);
  b[81] = o.claimed;
  b[82] = o.bump;
  return b;
}

function buildConfig(over) {
  const o = Object.assign({
    admin: 1, resolver: 2, mint: 3, feeBps: 0n, duration: 86400n, minStake: 1000000n, bump: 254,
  }, over || {});
  const b = Buffer.alloc(122);
  b[0] = 1;
  b.fill(o.admin, 1, 33);
  b.fill(o.resolver, 33, 65);
  b.fill(o.mint, 65, 97);
  b.writeBigUInt64LE(o.feeBps, 97);
  b.writeBigInt64LE(o.duration, 105);
  b.writeBigUInt64LE(o.minStake, 113);
  b[121] = o.bump;
  return b;
}

function assertCode(fn, code) {
  assert.throws(fn, (err) => {
    assert.equal(err.code, code, `expected code ${code}, got ${err && err.code} (${err && err.message})`);
    return true;
  });
}

test("market roundtrip decodes every field", () => {
  const m = predict.decodeMarket(buildMarket(), "MarketAddr1111111111111111111111111111111111");
  assert.equal(m.address, "MarketAddr1111111111111111111111111111111111");
  assert.equal(m.subjectMint, pub(4));
  assert.equal(m.creator, pub(5));
  assert.equal(m.createdAt, 1799913600);
  assert.equal(m.expiry, 1800000000);
  assert.equal(m.flaggedPool, "3000000");
  assert.equal(m.survivesPool, "7000000");
  assert.equal(m.totalPool, "10000000");
  assert.equal(m.state, 1);
  assert.equal(m.stateName, "open");
  assert.equal(m.outcome, 0);
  assert.equal(m.outcomeName, "none");
  assert.equal(m.evidenceHash, "07".repeat(32));
  assert.equal(m.evidenceHashIsZero, false);
  assert.equal(m.hardTrigger, 0);
  assert.equal(m.resolvedAt, 0);
  assert.equal(m.bump, 253);
  assert.equal(m.vaultBump, 252);
});

test("market offsets pinned: hash at 99, trigger at 131, bumps at 140 and 141", () => {
  const b = buildMarket({ hashByte: 0xab, trigger: 4, bump: 200, vaultBump: 201, state: 2, outcome: 1, resolvedAt: 1799920800n });
  const m = predict.decodeMarket(b);
  assert.equal(m.evidenceHash, "ab".repeat(32));
  assert.equal(m.hardTrigger, 4);
  assert.equal(m.bump, 200);
  assert.equal(m.vaultBump, 201);
  assert.equal(m.stateName, "resolved");
  assert.equal(m.outcomeName, "flagged");
  assert.equal(m.resolvedAt, 1799920800);
  // Changing bytes outside 99..131 must not change the hash.
  const b2 = Buffer.from(b);
  b2[140] = 0xcd;
  assert.equal(predict.decodeMarket(b2).evidenceHash, "ab".repeat(32));
  assert.equal(predict.decodeMarket(b2).bump, 0xcd);
});

test("position roundtrip decodes every field", () => {
  const p = predict.decodePosition(buildPosition({ claimed: 1 }), "PosAddr111111111111111111111111111111111111");
  assert.equal(p.address, "PosAddr111111111111111111111111111111111111");
  assert.equal(p.market, pub(6));
  assert.equal(p.user, pub(7));
  assert.equal(p.flaggedAmount, "400");
  assert.equal(p.survivesAmount, "600");
  assert.equal(p.totalStake, "1000");
  assert.equal(p.claimed, true);
  assert.equal(p.bump, 251);
  assert.equal(predict.decodePosition(buildPosition({ claimed: 0 })).claimed, false);
});

test("config roundtrip with resolver at 33 and minStake at 113", () => {
  const c = predict.decodeConfig(buildConfig(), "ConfigAddr11111111111111111111111111111111");
  assert.equal(c.address, "ConfigAddr11111111111111111111111111111111");
  assert.equal(c.admin, pub(1));
  assert.equal(c.resolver, pub(2));
  assert.equal(c.collateralMint, pub(3));
  assert.equal(c.feeBps, "0");
  assert.equal(c.defaultDurationSecs, 86400);
  assert.equal(c.minStake, "1000000");
  assert.equal(c.bump, 254);
  const b = buildConfig({ minStake: 18446744073709551615n });
  b.writeBigUInt64LE(42n, 97);
  const c2 = predict.decodeConfig(b);
  assert.equal(c2.feeBps, "42");
  assert.equal(c2.minStake, "18446744073709551615");
});

test("live market roundtrip", () => {
  const b = Buffer.alloc(34);
  b[0] = 4;
  b.fill(8, 1, 33);
  b[33] = 250;
  const lm = predict.decodeLiveMarket(b, "LiveAddr1111111111111111111111111111111111");
  assert.equal(lm.address, "LiveAddr1111111111111111111111111111111111");
  assert.equal(lm.market, pub(8));
  assert.equal(lm.bump, 250);
});

test("u64 max pool decodes exactly with no truncation", () => {
  const m = predict.decodeMarket(buildMarket({ flagged: 18446744073709551615n, survives: 42n }));
  assert.equal(m.flaggedPool, "18446744073709551615");
  assert.equal(m.survivesPool, "42");
  assert.equal(m.totalPool, "18446744073709551657");
});

test("amounts above 2^63 survive a JSON roundtrip exactly", () => {
  const big = "9223372036854775809"; // 2^63 + 1
  const p = predict.decodePosition(buildPosition({ flagged: 9223372036854775809n, survives: 1n }));
  assert.equal(p.flaggedAmount, big);
  const revived = JSON.parse(JSON.stringify(p));
  assert.equal(revived.flaggedAmount, big);
  assert.equal(revived.totalStake, "9223372036854775810");
});

test("truncated market bytes reject with BAD_LENGTH", () => {
  const b = buildMarket();
  assertCode(() => predict.decodeMarket(b.subarray(0, 141)), "BAD_LENGTH");
  assertCode(() => predict.decodePosition(buildPosition().subarray(0, 82)), "BAD_LENGTH");
  assertCode(() => predict.decodeConfig(buildConfig().subarray(0, 121)), "BAD_LENGTH");
});

test("trailing bytes reject with BAD_LENGTH", () => {
  const b = Buffer.concat([buildMarket(), Buffer.from([0])]);
  assertCode(() => predict.decodeMarket(b), "BAD_LENGTH");
  const lb = Buffer.alloc(35);
  lb[0] = 4;
  assertCode(() => predict.decodeLiveMarket(lb), "BAD_LENGTH");
});

test("wrong discriminator rejects with BAD_DISCRIMINATOR", () => {
  const b = buildMarket();
  b[0] = 3;
  assertCode(() => predict.decodeMarket(b), "BAD_DISCRIMINATOR");
  const c = buildConfig();
  c[0] = 2;
  assertCode(() => predict.decodeConfig(c), "BAD_DISCRIMINATOR");
});

test("bad market state rejects with BAD_STATE", () => {
  assertCode(() => predict.decodeMarket(buildMarket({ state: 9 })), "BAD_STATE");
  assertCode(() => predict.decodeMarket(buildMarket({ state: 0 })), "BAD_STATE");
});

test("bad outcome rejects with BAD_OUTCOME", () => {
  assertCode(() => predict.decodeMarket(buildMarket({ outcome: 7 })), "BAD_OUTCOME");
});

test("bad hard trigger rejects with BAD_TRIGGER", () => {
  assertCode(() => predict.decodeMarket(buildMarket({ trigger: 6 })), "BAD_TRIGGER");
  assert.equal(predict.decodeMarket(buildMarket({ trigger: 5 })).hardTrigger, 5);
});

test("claimed byte other than 0 or 1 rejects with BAD_BOOL", () => {
  assertCode(() => predict.decodePosition(buildPosition({ claimed: 2 })), "BAD_BOOL");
});

test("zero pubkey encodes as the all-ones address", () => {
  const m = predict.decodeMarket(buildMarket({ subject: 0, creator: 0 }));
  assert.equal(m.subjectMint, "11111111111111111111111111111111");
  assert.equal(m.creator, "11111111111111111111111111111111");
});

test("evidence hash hex is lowercase and the zero hash is flagged", () => {
  const m = predict.decodeMarket(buildMarket({ hashByte: 0xab }));
  assert.match(m.evidenceHash, /^[0-9a-f]{64}$/);
  const z = predict.decodeMarket(buildMarket({ hashByte: 0 }));
  assert.equal(z.evidenceHash, "0".repeat(64));
  assert.equal(z.evidenceHashIsZero, true);
});

test("timestamp outside the safe integer range rejects with BAD_TIMESTAMP", () => {
  assertCode(() => predict.decodeMarket(buildMarket({ createdAt: 9223372036854775807n })), "BAD_TIMESTAMP");
});
