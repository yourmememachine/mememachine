"use strict";

// Tests for rugcall/resolver/resolver.js: decision table over the
// fixture markets, bundle file output, ledger idempotence, and the
// refusal rails. Every pass runs in a fresh temp output directory.

const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { createResolver } = require("../resolver.js");
const ev = require("../../app/evidence.js");

const FIXTURES = path.join(__dirname, "fixtures");
const EVIDENCE_DIR = path.join(FIXTURES, "evidence");
const CREATED = 1799913600;
const EXPIRY = 1800000000;
const RESOLVER_PUBKEY = "Dm95oxQp8YghEsCads1SXWkGwy4BaYLjCYv4fieG7GtH";

function loadMarkets() {
  return JSON.parse(
    fs.readFileSync(path.join(FIXTURES, "markets.json"), "utf8")
  );
}

function marketByEvidence(markets, evidenceFile, state) {
  return markets.markets.find(
    (m) =>
      m.evidence_file === evidenceFile &&
      (state === undefined || m.state === state)
  );
}

function singleMarketObj(market) {
  const markets = loadMarkets();
  return { ...markets, markets: [structuredClone(market)] };
}

function tmpDir() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "rugcall-resolver-test-"));
}

function makeResolver(overrides) {
  const sinkRecords = [];
  const opts = {
    resolverPubkey: RESOLVER_PUBKEY,
    sink: (record) => sinkRecords.push(record),
    now: EXPIRY,
    outDir: tmpDir(),
    evidenceDir: EVIDENCE_DIR,
    crewStore: ev.emptyCrewStore(),
    ...(overrides || {}),
  };
  const resolver = createResolver(opts);
  return { resolver, sinkRecords, opts };
}

function forMarket(records, market) {
  return records.filter((r) => r.market === market.address);
}

function wouldRecords(records) {
  return records.filter((r) => r.action.startsWith("WOULD_"));
}

test("resolver: clean market at expiry would-resolve as Survives with a verifiable bundle", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "clean.json", 1);
  const { resolver, sinkRecords, opts } = makeResolver({ now: EXPIRY });
  const records = resolver.runPass(singleMarketObj(market));
  const mine = forMarket(records, market);
  const resolves = mine.filter((r) => r.action === "WOULD_RESOLVE");
  assert.equal(resolves.length, 1);
  assert.equal(sinkRecords.length, records.length);
  const record = resolves[0];
  assert.equal(record.outcome, 2);
  assert.equal(record.hard_trigger, 0);
  assert.match(record.evidence_hash, /^[0-9a-f]{64}$/);
  assert.notEqual(record.evidence_hash, "0".repeat(64));
  assert.equal(record.signed, false);
  assert.equal(record.submitted, false);

  const bundlePath = path.join(opts.outDir, "evidence", `${market.address}.json`);
  assert.ok(fs.existsSync(bundlePath), `bundle at ${bundlePath}`);
  const fileBytes = fs.readFileSync(bundlePath);
  const fileSha = crypto.createHash("sha256").update(fileBytes).digest("hex");
  assert.equal(fileSha, record.evidence_hash);
  assert.equal(record.bundle_sha256, record.evidence_hash);

  const ix = record.instruction;
  assert.equal(ix.name, "ResolveMarket");
  assert.equal(ix.ordinal, 3);
  assert.equal(ix.data_len, 35);
  assert.equal(ix.data_hex.length, 70);
  assert.ok(ix.data_hex.startsWith("03"));
  assert.equal(ix.data_hex.slice(2, 4), "02");
  assert.equal(ix.data_hex.slice(4, 6), "00");
  assert.deepEqual(ix.accounts, [
    { pubkey: RESOLVER_PUBKEY, signer: true, writable: false },
    { pubkey: markets.config, signer: false, writable: false },
    { pubkey: market.address, signer: false, writable: true },
    { pubkey: market.live_market, signer: false, writable: true },
    { pubkey: market.creator, signer: false, writable: true },
  ]);
});

test("resolver: h2 market resolves early as Flagged with trigger 2", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "h2-bundle.json", 1);
  const { resolver } = makeResolver({ now: CREATED + 7200 });
  const records = resolver.runPass(singleMarketObj(market));
  const resolves = forMarket(records, market).filter(
    (r) => r.action === "WOULD_RESOLVE"
  );
  assert.equal(resolves.length, 1);
  assert.equal(resolves[0].outcome, 1);
  assert.equal(resolves[0].hard_trigger, 2);
  assert.deepEqual(resolves[0].qualifying_triggers, [2]);
});

test("resolver: soft-flagged market produces no would-resolve before expiry", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "soft-flagged.json", 1);
  const { resolver } = makeResolver({ now: CREATED + 7200 });
  const records = resolver.runPass(singleMarketObj(market));
  assert.deepEqual(wouldRecords(forMarket(records, market)), []);
});

test("resolver: soft-flagged market would-resolve as Survives at expiry", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "soft-flagged.json", 1);
  const { resolver } = makeResolver({ now: EXPIRY });
  const records = resolver.runPass(singleMarketObj(market));
  const resolves = forMarket(records, market).filter(
    (r) => r.action === "WOULD_RESOLVE"
  );
  assert.equal(resolves.length, 1);
  assert.equal(resolves[0].outcome, 2);
  assert.equal(resolves[0].hard_trigger, 0);
});

test("resolver: late trigger is excluded and the market would-resolve as Survives", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "late-trigger.json", 1);
  const { resolver } = makeResolver({ now: EXPIRY + 3600 });
  const records = resolver.runPass(singleMarketObj(market));
  const resolves = forMarket(records, market).filter(
    (r) => r.action === "WOULD_RESOLVE"
  );
  assert.equal(resolves.length, 1);
  assert.equal(resolves[0].outcome, 2);
  assert.equal(resolves[0].hard_trigger, 0);
  assert.ok(
    resolves[0].excluded_triggers.some(
      (t) => t.id === 3 && t.eventTimestamp === EXPIRY + 3600
    ),
    `excluded_triggers ${JSON.stringify(resolves[0].excluded_triggers)} should contain trigger 3 at ${EXPIRY + 3600}`
  );
});

test("resolver: trigger landing exactly at expiry qualifies and flags the market", () => {
  const markets = loadMarkets();
  const template = marketByEvidence(markets, "late-trigger.json", 1);
  const evidence = JSON.parse(
    fs.readFileSync(path.join(EVIDENCE_DIR, "at-expiry-trigger.json"), "utf8")
  );
  const market = {
    ...template,
    address: "MarketAtExpiry111111111111111111111111111111",
    live_market: "LiveAtExpiry11111111111111111111111111111111",
    subject_mint: evidence.mint,
    evidence_file: "at-expiry-trigger.json",
  };
  const { resolver } = makeResolver({ now: EXPIRY + 3600 });
  const records = resolver.runPass(singleMarketObj(market));
  const resolves = forMarket(records, market).filter(
    (r) => r.action === "WOULD_RESOLVE"
  );
  assert.equal(resolves.length, 1);
  assert.equal(resolves[0].outcome, 1);
  assert.equal(resolves[0].hard_trigger, 3);
  assert.deepEqual(resolves[0].qualifying_triggers, [3]);
});

test("resolver: partial market at expiry is deferred, not resolved", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "partial-bundle.json", 1);
  const { resolver } = makeResolver({ now: EXPIRY });
  const records = resolver.runPass(singleMarketObj(market));
  const mine = forMarket(records, market);
  assert.deepEqual(
    mine.map((r) => r.action),
    ["DEFERRED_PARTIAL"]
  );
});

test("resolver: partial market at the end of the grace window is still deferred", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "partial-bundle.json", 1);
  const { resolver } = makeResolver({ now: EXPIRY + 86400 });
  const records = resolver.runPass(singleMarketObj(market));
  const mine = forMarket(records, market);
  assert.deepEqual(
    mine.map((r) => r.action),
    ["DEFERRED_PARTIAL"]
  );
});

test("resolver: partial market past the grace window would-cancel", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "partial-bundle.json", 1);
  const { resolver } = makeResolver({ now: EXPIRY + 86401 });
  const records = resolver.runPass(singleMarketObj(market));
  const cancels = forMarket(records, market).filter(
    (r) => r.action === "WOULD_CANCEL"
  );
  assert.equal(cancels.length, 1);
  assert.equal(cancels[0].instruction.name, "CancelMarket");
  assert.equal(cancels[0].instruction.data_hex, "05");
  assert.equal(cancels[0].instruction.data_len, 1);
});

test("resolver: partial market whose evidence completes inside grace resolves as Survives", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "partial-bundle.json", 1);
  const workDir = tmpDir();
  const evidenceDir = path.join(workDir, "evidence");
  fs.mkdirSync(evidenceDir, { recursive: true });
  for (const file of fs.readdirSync(EVIDENCE_DIR)) {
    fs.copyFileSync(path.join(EVIDENCE_DIR, file), path.join(evidenceDir, file));
  }
  const ledgerPath = path.join(workDir, "ledger.json");
  const outDir = path.join(workDir, "out");
  const first = makeResolver({
    now: EXPIRY,
    outDir,
    evidenceDir,
    ledgerPath,
  });
  const firstRecords = first.resolver.runPass(singleMarketObj(market));
  assert.deepEqual(
    forMarket(firstRecords, market).map((r) => r.action),
    ["DEFERRED_PARTIAL"]
  );

  const partialPath = path.join(evidenceDir, "partial-bundle.json");
  const completed = JSON.parse(fs.readFileSync(partialPath, "utf8"));
  const clean = JSON.parse(
    fs.readFileSync(path.join(EVIDENCE_DIR, "clean.json"), "utf8")
  );
  completed.deep.bundle = clean.deep.bundle;
  fs.writeFileSync(partialPath, `${JSON.stringify(completed, null, 2)}\n`);

  const second = makeResolver({
    now: EXPIRY + 3600,
    outDir,
    evidenceDir,
    ledgerPath,
  });
  const secondRecords = second.resolver.runPass(singleMarketObj(market));
  const resolves = forMarket(secondRecords, market).filter(
    (r) => r.action === "WOULD_RESOLVE"
  );
  assert.equal(resolves.length, 1);
  assert.equal(resolves[0].outcome, 2);
  assert.equal(resolves[0].hard_trigger, 0);
});

test("resolver: resolved and cancelled fixture markets are skipped as terminal", () => {
  const markets = loadMarkets();
  const { resolver } = makeResolver({ now: EXPIRY });
  const records = resolver.runPass(loadMarkets());
  for (const market of markets.markets.filter((m) => m.state === 2 || m.state === 3)) {
    const mine = forMarket(records, market);
    assert.ok(mine.length >= 1, `records for ${market.address}`);
    assert.ok(
      mine.every((r) => r.action === "SKIP_TERMINAL"),
      `terminal market actions ${JSON.stringify(mine.map((r) => r.action))}`
    );
    assert.deepEqual(wouldRecords(mine), []);
  }
});

test("resolver: a decided market is skipped from the ledger on the next pass", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "clean.json", 1);
  const workDir = tmpDir();
  const ledgerPath = path.join(workDir, "ledger.json");
  const { resolver } = makeResolver({
    now: EXPIRY,
    outDir: path.join(workDir, "out"),
    ledgerPath,
  });
  const obj = singleMarketObj(market);
  const first = resolver.runPass(obj);
  assert.equal(
    forMarket(first, market).filter((r) => r.action === "WOULD_RESOLVE").length,
    1
  );
  const second = resolver.runPass(obj);
  const mine = forMarket(second, market);
  assert.ok(mine.some((r) => r.action === "SKIP_LEDGER"));
  assert.deepEqual(wouldRecords(mine), []);
});

test("resolver: without a ledger, two passes produce identical records", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "clean.json", 1);
  const outDir = tmpDir();
  const a = makeResolver({ now: EXPIRY, outDir });
  const b = makeResolver({ now: EXPIRY, outDir });
  const obj = singleMarketObj(market);
  const first = a.resolver.runPass(obj);
  const second = b.resolver.runPass(obj);
  assert.deepEqual(second, first);
});

test("resolver: a markets file naming a different resolver is refused outright", () => {
  const markets = loadMarkets();
  const wrong = {
    ...markets,
    resolver: "WrongReso1ver1111111111111111111111111111111",
  };
  const { resolver } = makeResolver({ now: EXPIRY });
  const records = resolver.runPass(wrong);
  assert.equal(records.length, 1);
  assert.equal(records[0].action, "REFUSAL");
  assert.deepEqual(wouldRecords(records), []);
});

test("resolver: a zero evidence hash blocks the resolution", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "clean.json", 1);
  const { resolver } = makeResolver({
    now: EXPIRY,
    hashFn: () => ({ hex: "0".repeat(64), bytes: Buffer.alloc(32) }),
  });
  const records = resolver.runPass(singleMarketObj(market));
  const mine = forMarket(records, market);
  assert.ok(mine.some((r) => r.action === "BLOCKED_ZERO_HASH"));
  assert.deepEqual(
    mine.filter((r) => r.action === "WOULD_RESOLVE"),
    []
  );
});

test("resolver: a throwing hash function surfaces as a thrown error, not a record", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "clean.json", 1);
  const { resolver } = makeResolver({
    now: EXPIRY,
    hashFn: () => {
      throw new Error("hash boom");
    },
  });
  assert.throws(
    () => resolver.runPass(singleMarketObj(market)),
    /hash boom/
  );
});

test("resolver: clean market one second before expiry produces no would records", () => {
  const markets = loadMarkets();
  const market = marketByEvidence(markets, "clean.json", 1);
  const { resolver } = makeResolver({ now: EXPIRY - 1 });
  const records = resolver.runPass(singleMarketObj(market));
  const mine = forMarket(records, market);
  assert.deepEqual(wouldRecords(mine), []);
  assert.ok(mine.some((r) => r.action === "WATCHING"));
});

test("resolver: every record in a full pass is unsigned and unsubmitted", () => {
  const { resolver } = makeResolver({ now: EXPIRY });
  const records = resolver.runPass(loadMarkets());
  assert.ok(records.length > 0);
  for (const record of records) {
    assert.equal(record.signed, false, `${record.action} signed`);
    assert.equal(record.submitted, false, `${record.action} submitted`);
  }
});
