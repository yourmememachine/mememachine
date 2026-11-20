"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const predict = require("../predict.js");
const tx = require("../predict-tx.js");

const APP_FIXTURES = JSON.parse(
  fs.readFileSync(path.join(__dirname, "fixtures", "app-markets.json"), "utf8")
);
const PROGRAM_ID = "RugCa11Program111111111111111111111111111111";
const M5 = APP_FIXTURES.markets[4].address;

function fixtureSource() {
  return predict.createDataSource({ programId: null, fixtureBundle: APP_FIXTURES });
}

test("factory picks the fixture source when programId is null and devnet otherwise", () => {
  assert.equal(fixtureSource().kind, "fixture");
  assert.equal(fixtureSource().programId, null);
  const dev = predict.createDataSource({ programId: PROGRAM_ID, transport: {} });
  assert.equal(dev.kind, "devnet");
  assert.equal(dev.programId, PROGRAM_ID);
});

test("fixture listMarkets orders by createdAt descending with an address tiebreak", async () => {
  const markets = await fixtureSource().listMarkets();
  assert.equal(markets.length, 8);
  for (let i = 1; i < markets.length; i++) {
    const a = markets[i - 1], b = markets[i];
    assert.ok(a.createdAt > b.createdAt || (a.createdAt === b.createdAt && a.address < b.address),
      `order broken at ${i}: ${a.address} then ${b.address}`);
  }
  // The expired market was created a day earlier, so it sorts last.
  assert.equal(markets[7].address, APP_FIXTURES.markets[3].address);
  assert.equal(markets[0].createdAt, 1799913600);
});

test("fixture listMarkets returns fresh copies", async () => {
  const src = fixtureSource();
  const first = await src.listMarkets();
  first[0].flaggedPool = "999";
  first[0].subjectMint = "Mutated111111111111111111111111111111111111";
  const second = await src.listMarkets();
  assert.notEqual(second[0].flaggedPool, "999");
  assert.notEqual(second[0].subjectMint, "Mutated111111111111111111111111111111111111");
});

test("fixture getMarket hits and misses", async () => {
  const src = fixtureSource();
  const hit = await src.getMarket(M5);
  assert.equal(hit.address, M5);
  assert.equal(hit.stateName, "resolved");
  assert.equal(hit.outcomeName, "flagged");
  assert.equal(hit.hardTrigger, 2);
  assert.equal(await src.getMarket("NoSuchMarket111111111111111111111111111111"), null);
});

test("fixture getLiveMarketForMint returns the open market for a mint", async () => {
  const src = fixtureSource();
  const h2Mint = APP_FIXTURES.markets[0].subject_mint;
  const live = await src.getLiveMarketForMint(h2Mint);
  assert.equal(live.market, APP_FIXTURES.markets[0].address);
  assert.equal(live.address, APP_FIXTURES.markets[0].live_market);
  const cleanMint = APP_FIXTURES.markets[1].subject_mint;
  const cleanLive = await src.getLiveMarketForMint(cleanMint);
  assert.equal(cleanLive.market, APP_FIXTURES.markets[1].address);
  assert.equal(await src.getLiveMarketForMint("UnknownMint1111111111111111111111111111111"), null);
});

test("fixture positions join by market and by user", async () => {
  const src = fixtureSource();
  const { demo, userB, userC, userD } = APP_FIXTURES.users;
  assert.equal((await src.listPositions({ market: M5 })).length, 3);
  assert.equal((await src.listPositions({ user: demo })).length, 8);
  assert.equal((await src.listPositionsForUser(userB)).length, 7);
  assert.equal((await src.listPositionsForUser(userC)).length, 5);
  assert.equal((await src.listPositionsForUser(userD)).length, 2);
  assert.equal((await src.listPositions({})).length, 22);
  const both = await src.listPositions({ market: M5, user: userD });
  assert.equal(both.length, 1);
  assert.equal(both[0].flaggedAmount, "2000000");
  assert.equal(both[0].survivesAmount, "2000000");
});

test("fixture getConfig carries the recorded resolver and minimum stake", async () => {
  const cfg = await fixtureSource().getConfig();
  assert.equal(cfg.address, APP_FIXTURES.config.address);
  assert.equal(cfg.resolver, APP_FIXTURES.config.resolver);
  assert.equal(cfg.collateralMint, null);
  assert.equal(cfg.minStake, "1000000");
});

test("an invalid fixture bundle rejects with FIXTURE_INVALID", async () => {
  const bad = JSON.parse(JSON.stringify(APP_FIXTURES));
  bad.markets[0].state = 9;
  const src = predict.createDataSource({ programId: null, fixtureBundle: bad });
  await assert.rejects(src.listMarkets(), (err) => err.code === "FIXTURE_INVALID");
  const empty = predict.createDataSource({ programId: null, fixtureBundle: null });
  await assert.rejects(empty.listMarkets(), (err) => err.code === "FIXTURE_INVALID");
});

/* ---------------- devnet source over a fake transport ---------------- */

function pub(byte) {
  return tx.base58Encode(new Uint8Array(32).fill(byte));
}

function marketBytes(over) {
  const o = Object.assign({ subject: 4, createdAt: 1799913600n, flagged: 3000000n, survives: 7000000n, state: 1 }, over || {});
  const b = Buffer.alloc(142);
  b[0] = 2;
  b.fill(o.subject, 1, 33);
  b.fill(5, 33, 65);
  b.writeBigInt64LE(o.createdAt, 65);
  b.writeBigInt64LE(1800000000n, 73);
  b.writeBigUInt64LE(o.flagged, 81);
  b.writeBigUInt64LE(o.survives, 89);
  b[97] = o.state;
  return b;
}

function positionBytes(over) {
  const o = Object.assign({ marketByte: 6, userByte: 7, flagged: 400n, survives: 600n }, over || {});
  const b = Buffer.alloc(83);
  b[0] = 3;
  b.fill(o.marketByte, 1, 33);
  b.fill(o.userByte, 33, 65);
  b.writeBigUInt64LE(o.flagged, 65);
  b.writeBigUInt64LE(o.survives, 73);
  return b;
}

function configBytes() {
  const b = Buffer.alloc(122);
  b[0] = 1;
  b.fill(1, 1, 33);
  b.fill(2, 33, 65);
  b.fill(3, 65, 97);
  b.writeBigInt64LE(86400n, 105);
  b.writeBigUInt64LE(1000000n, 113);
  b[121] = 254;
  return b;
}

function fakeTransport() {
  const calls = [];
  // Real base58 strings derived from byte fills, so a lock's
  // stored market field decodes to exactly marketA's address.
  const marketA = pub(10);
  const marketB = pub(11);
  const liveA = pub(12);
  const byAddress = {};
  byAddress[marketA] = marketBytes({ createdAt: 1799913600n });
  byAddress[marketB] = marketBytes({ createdAt: 1799913601n, subject: 9 });
  const liveBytes = Buffer.alloc(34);
  liveBytes[0] = 4;
  liveBytes.fill(10, 1, 33);
  liveBytes[33] = 250;
  return {
    calls,
    marketA,
    marketB,
    async getProgramAccounts(programId, filter) {
      calls.push({ method: "getProgramAccounts", programId, filter });
      if (filter.dataSize === 142) {
        return [
          { pubkey: marketA, data: byAddress[marketA] },
          { pubkey: marketB, data: byAddress[marketB] },
        ];
      }
      if (filter.dataSize === 83) return [{ pubkey: "PosDevnet1111111111111111111111111111111111", data: positionBytes({}) }];
      if (filter.dataSize === 122) return [{ pubkey: "ConfigDevnet111111111111111111111111111111", data: configBytes() }];
      if (filter.dataSize === 34) return [{ pubkey: liveA, data: liveBytes }];
      return [];
    },
    async getAccountInfo(address) {
      calls.push({ method: "getAccountInfo", address });
      return byAddress[address] ? { data: byAddress[address] } : null;
    },
    async getMultipleAccounts(addresses) {
      calls.push({ method: "getMultipleAccounts", addresses });
      return addresses.map((a) => (byAddress[a] ? { data: byAddress[a] } : null));
    },
  };
}

test("devnet listMarkets decodes accounts and sorts newest first", async () => {
  const t = fakeTransport();
  const src = predict.createDataSource({ programId: PROGRAM_ID, transport: t });
  const markets = await src.listMarkets();
  assert.equal(markets.length, 2);
  assert.equal(markets[0].address, t.marketB);
  assert.equal(markets[0].createdAt, 1799913601);
  assert.equal(markets[1].flaggedPool, "3000000");
  const call = t.calls.find((c) => c.method === "getProgramAccounts" && c.filter.dataSize === 142);
  assert.equal(call.programId, PROGRAM_ID);
  assert.deepEqual(call.filter.memcmp, [{ offset: 0, bytes: tx.base58Encode(Uint8Array.of(2)) }]);
});

test("devnet listPositions passes discriminator and field memcmp filters", async () => {
  const t = fakeTransport();
  const src = predict.createDataSource({ programId: PROGRAM_ID, transport: t });
  const byMarket = await src.listPositions({ market: t.marketA });
  assert.equal(byMarket.length, 1);
  assert.equal(byMarket[0].flaggedAmount, "400");
  let call = t.calls.filter((c) => c.method === "getProgramAccounts" && c.filter.dataSize === 83).pop();
  assert.deepEqual(call.filter.memcmp, [
    { offset: 0, bytes: tx.base58Encode(Uint8Array.of(3)) },
    { offset: 1, bytes: t.marketA },
  ]);
  await src.listPositions({ user: "UserDevnet11111111111111111111111111111111" });
  call = t.calls.filter((c) => c.method === "getProgramAccounts" && c.filter.dataSize === 83).pop();
  assert.deepEqual(call.filter.memcmp, [
    { offset: 0, bytes: tx.base58Encode(Uint8Array.of(3)) },
    { offset: 33, bytes: "UserDevnet11111111111111111111111111111111" },
  ]);
});

test("devnet listPositions without a filter rejects NOT_CONFIGURED", async () => {
  const src = predict.createDataSource({ programId: PROGRAM_ID, transport: fakeTransport() });
  await assert.rejects(src.listPositions({}), (err) => err.code === "NOT_CONFIGURED");
  await assert.rejects(src.listPositions(), (err) => err.code === "NOT_CONFIGURED");
});

test("devnet getConfig decodes the scanned config account", async () => {
  const t = fakeTransport();
  const src = predict.createDataSource({ programId: PROGRAM_ID, transport: t });
  const cfg = await src.getConfig();
  assert.equal(cfg.resolver, pub(2));
  assert.equal(cfg.minStake, "1000000");
  assert.equal(cfg.defaultDurationSecs, 86400);
  const call = t.calls.find((c) => c.method === "getProgramAccounts" && c.filter.dataSize === 122);
  assert.deepEqual(call.filter.memcmp, [{ offset: 0, bytes: tx.base58Encode(Uint8Array.of(1)) }]);
});

test("devnet getLiveMarketForMint scans live locks at dataSize 34", async () => {
  const t = fakeTransport();
  const src = predict.createDataSource({ programId: PROGRAM_ID, transport: t });
  const live = await src.getLiveMarketForMint(pub(4));
  assert.equal(live.market, t.marketA);
  assert.equal(live.address, pub(12));
  assert.equal(live.bump, 250);
  // marketB is about subject byte 9 and has no live lock.
  assert.equal(await src.getLiveMarketForMint(pub(9)), null);
  const scan = t.calls.find((c) => c.method === "getProgramAccounts" && c.filter.dataSize === 34);
  assert.ok(scan, "a dataSize 34 scan happened");
  assert.deepEqual(scan.filter.memcmp, [{ offset: 0, bytes: tx.base58Encode(Uint8Array.of(4)) }]);
});

test("a failing transport rejects with RPC_FAILED, never an empty list", async () => {
  const broken = {
    async getProgramAccounts() { throw new Error("connection reset"); },
    async getAccountInfo() { throw new Error("connection reset"); },
    async getMultipleAccounts() { throw new Error("connection reset"); },
  };
  const src = predict.createDataSource({ programId: PROGRAM_ID, transport: broken });
  await assert.rejects(src.listMarkets(), (err) => err.code === "RPC_FAILED" && err.name === "PredictSourceError");
  await assert.rejects(src.getMarket("MarketDevnetA111111111111111111111111111111"), (err) => err.code === "RPC_FAILED");
});
