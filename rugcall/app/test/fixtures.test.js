"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const predict = require("../predict.js");
const tx = require("../predict-tx.js");
const ev = require("../evidence.js");
const ui = require("../predict-ui.js");

const HERE = __dirname;
const APP_FIXTURES_DIR = path.join(HERE, "fixtures");
const RESOLVER_EVIDENCE_DIR = path.join(HERE, "..", "..", "resolver", "test", "fixtures", "evidence");
const ZERO_HASH = "0".repeat(64);
const U64_MAX = 18446744073709551615n;

const bundle = JSON.parse(fs.readFileSync(path.join(APP_FIXTURES_DIR, "app-markets.json"), "utf8"));
const golden = JSON.parse(fs.readFileSync(path.join(APP_FIXTURES_DIR, "pda-vectors-deployed.json"), "utf8"));
const markets = bundle.markets;
const positions = bundle.positions;
const byAddress = {};
for (const m of markets) byAddress[m.address] = m;

function evidencePathFor(m) {
  const local = path.join(APP_FIXTURES_DIR, m.evidence_file);
  if (fs.existsSync(local)) return local;
  return path.join(RESOLVER_EVIDENCE_DIR, m.evidence_file);
}
function loadEvidenceFor(m) {
  return JSON.parse(fs.readFileSync(evidencePathFor(m), "utf8"));
}

test("schema, program id and cast sizes", () => {
  assert.equal(bundle.schema, "rugcall-app-fixtures/v1");
  // The fixture bundle carries the deployed devnet program id
  // since the 2026-11-20 deploy (it was null while undeployed).
  assert.equal(bundle.program_id, "DcwvEDi4yCk16wQ2HVzc3dQiBFccj8VxzboGUYdUXseZ");
  // Fixture placeholder, pinned on purpose: this readable label is
  // NOT the derived config PDA. The real config account under the
  // deployed program is D8upBvHaUh7pX8PT3AZWFFWa78PkDJAJ5xPoumBSwANp
  // (bump 252). See "config_note" in app-markets.json.
  assert.equal(bundle.config.address, "ConfigRugCa111111111111111111111111111111111");
  assert.equal(bundle.config.resolver, "Dm95oxQp8YghEsCads1SXWkGwy4BaYLjCYv4fieG7GtH");
  assert.equal(bundle.config.collateral_mint, null);
  assert.equal(bundle.config.min_stake, "1000000");
  assert.equal(markets.length, 8);
  assert.equal(positions.length, 22);
  const userKeys = Object.keys(bundle.users).sort();
  assert.deepEqual(userKeys, ["demo", "userB", "userC", "userD"]);
  const addrs = new Set(Object.values(bundle.users));
  assert.equal(addrs.size, 4);
  for (const u of Object.values(bundle.users)) assert.match(u, /^[1-9A-HJ-NP-Za-km-z]{32,44}$/);
});

test("pool and stake amounts are canonical u64 strings", () => {
  const canon = /^(0|[1-9][0-9]*)$/;
  for (const m of markets) {
    assert.match(m.flagged_pool, canon, m.address);
    assert.match(m.survives_pool, canon, m.address);
    assert.ok(BigInt(m.flagged_pool) <= U64_MAX);
    assert.ok(BigInt(m.survives_pool) <= U64_MAX);
  }
  for (const p of positions) {
    assert.match(p.flagged_amount, canon, p.address);
    assert.match(p.survives_amount, canon, p.address);
  }
});

test("position sums equal the pools per side for every market", () => {
  for (const m of markets) {
    let f = 0n, s = 0n;
    for (const p of positions.filter((x) => x.market === m.address)) {
      f += BigInt(p.flagged_amount);
      s += BigInt(p.survives_amount);
    }
    assert.equal(f.toString(), m.flagged_pool, `${m.address} flagged`);
    assert.equal(s.toString(), m.survives_pool, `${m.address} survives`);
  }
  const crafted = positions.filter((p) => p.crafted === true);
  assert.equal(crafted.length, 1);
  assert.equal(crafted[0].flagged_amount, "0");
  assert.equal(crafted[0].survives_amount, "0");
  assert.equal(crafted[0].claimed, false);
  assert.equal(byAddress[crafted[0].market].state, 3);
});

test("every required market state is present with its cast", () => {
  assert.equal(markets.filter((m) => m.state === 1).length, 4);
  assert.equal(markets.filter((m) => m.state === 2).length, 3);
  assert.equal(markets.filter((m) => m.state === 3).length, 1);
  const oneSided = markets.find((m) => m.subject_mint === "MintSoftF1ag1111111111111111111111111111111");
  assert.equal(oneSided.flagged_pool, "50000000");
  assert.equal(oneSided.survives_pool, "0");
  const expired = markets.find((m) => m.evidence_file === "late-trigger.json");
  assert.equal(expired.created_at, 1799740800);
  assert.equal(expired.expiry, 1799827200);
  const zeroWin = markets.find((m) => m.evidence_file === "evidence-h3-zerowin.json");
  assert.equal(zeroWin.flagged_pool, "0");
  assert.equal(zeroWin.survives_pool, "5000000");
  assert.equal(zeroWin.outcome, 1);
  assert.equal(zeroWin.hard_trigger, 3);
});

test("every evidence file exists and names the market subject mint", () => {
  for (const m of markets) {
    assert.ok(m.evidence_file, m.address);
    const p = evidencePathFor(m);
    assert.ok(fs.existsSync(p), `${m.address} -> ${m.evidence_file}`);
    const evidence = JSON.parse(fs.readFileSync(p, "utf8"));
    assert.equal(evidence.mint, m.subject_mint, m.address);
  }
});

test("hash and trigger consistency across terminal markets", () => {
  for (const m of markets) {
    if (m.state === 2 && m.outcome === 1) {
      assert.notEqual(m.evidence_hash, ZERO_HASH, m.address);
      assert.ok(m.hard_trigger >= 1 && m.hard_trigger <= 5, m.address);
    }
    if (m.state === 2 && m.outcome === 2) {
      assert.notEqual(m.evidence_hash, ZERO_HASH, m.address);
      assert.equal(m.hard_trigger, 0, m.address);
    }
    if (m.state === 3) {
      assert.equal(m.evidence_hash, ZERO_HASH, m.address);
      assert.equal(m.outcome, 0, m.address);
      assert.equal(m.hard_trigger, 0, m.address);
    }
    if (m.state === 1) assert.equal(m.evidence_hash, ZERO_HASH, m.address);
  }
});

test("resolved market hashes re-verify from their evidence bundles", () => {
  for (const m of markets.filter((x) => x.state === 2)) {
    const evidence = loadEvidenceFor(m);
    const store = ev.emptyCrewStore();
    const result = ev.evaluateEvidence(evidence, store);
    const openCopy = Object.assign({}, m, { evidence_hash: ZERO_HASH });
    const built = ev.buildBundle({
      market: openCopy,
      subjectMint: m.subject_mint,
      resolver: bundle.config.resolver,
      expirySec: m.expiry,
      fetchedAtSec: evidence.fetchedAtSec,
      evidence,
      crewStore: store,
      result,
    });
    assert.equal(ev.hashBundle(built).hex, m.evidence_hash, m.address);
    assert.equal(result.primaryHardTrigger, m.hard_trigger, m.address);
  }
});

test("claim previews over the fixture cast cover every outcome kind", async () => {
  const src = predict.createDataSource({ programId: null, fixtureBundle: bundle });
  const decodedMarkets = await src.listMarkets();
  const decodedPositions = await src.listPositions({});
  const marketOf = {};
  for (const m of decodedMarkets) marketOf[m.address] = m;
  const previewFor = (marketIdx, userKey) => {
    const m = decodedMarkets.find((x) => x.address === markets[marketIdx].address);
    const p = decodedPositions.find((x) => x.market === m.address && x.user === bundle.users[userKey]);
    return predict.computeClaimPreview(m, p || null);
  };
  // Market 5 (index 4): canonical payouts.
  assert.deepEqual(previewFor(4, "demo"), { kind: "payout", amountRaw: "3333333", reason: "winning-stake" });
  assert.deepEqual(previewFor(4, "userD"), { kind: "payout", amountRaw: "6666666", reason: "winning-stake" });
  assert.deepEqual(previewFor(4, "userC"), { kind: "no-claim", reason: "no-winning-stake" });
  // Market 6 (index 5): survives payouts.
  assert.deepEqual(previewFor(5, "demo"), { kind: "payout", amountRaw: "142857142", reason: "winning-stake" });
  assert.deepEqual(previewFor(5, "userB"), { kind: "payout", amountRaw: "857142857", reason: "winning-stake" });
  assert.deepEqual(previewFor(5, "userC"), { kind: "no-claim", reason: "no-winning-stake" });
  // Market 7 (index 6): cancelled refunds and the claimed flag.
  assert.deepEqual(previewFor(6, "demo"), { kind: "refund", amountRaw: "4000000", reason: "cancelled" });
  assert.deepEqual(previewFor(6, "userB"), { kind: "none", reason: "already-claimed" });
  assert.deepEqual(previewFor(6, "userC"), { kind: "no-claim", reason: "zero-total-stake" });
  // Market 8 (index 7): zero winning pool refunds in full.
  assert.deepEqual(previewFor(7, "demo"), { kind: "refund", amountRaw: "2000000", reason: "zero-winning-pool" });
  assert.deepEqual(previewFor(7, "userB"), { kind: "refund", amountRaw: "3000000", reason: "zero-winning-pool" });
  // The whole cast exercises payout, refund, no-claim and none.
  const kinds = new Set(decodedPositions.map((p) => predict.computeClaimPreview(marketOf[p.market], p).kind));
  assert.ok(kinds.has("payout") && kinds.has("refund") && kinds.has("no-claim") && kinds.has("none"));
});

test("timestamps are coherent", () => {
  for (const m of markets) {
    assert.ok(m.created_at < m.expiry, m.address);
    if (m.state === 2) assert.ok(m.resolved_at >= m.created_at, m.address);
    if (m.state !== 2) assert.equal(m.resolved_at, 0, m.address);
  }
});

test("token metadata has an entry for every subject mint", () => {
  const mints = new Set(markets.map((m) => m.subject_mint));
  for (const mint of mints) {
    assert.ok(Object.prototype.hasOwnProperty.call(bundle.token_meta, mint), mint);
    const meta = bundle.token_meta[mint];
    if (meta !== null) {
      assert.equal(typeof meta.symbol, "string");
      assert.equal(typeof meta.name, "string");
      assert.equal(typeof meta.priceUsd, "number");
      assert.equal(typeof meta.liquidityUsd, "number");
    }
  }
});

test("the expired market walks the grace phases at injected times", async () => {
  const src = predict.createDataSource({ programId: null, fixtureBundle: bundle });
  const expired = (await src.listMarkets()).find((m) => m.address === markets[3].address);
  assert.equal(predict.marketPhase(expired, expired.expiry), "open-live");
  assert.equal(predict.marketPhase(expired, expired.expiry + 3600), "open-expired-grace");
  assert.equal(predict.marketPhase(expired, expired.expiry + 86400), "open-expired-grace");
  assert.equal(predict.marketPhase(expired, expired.expiry + 86401), "open-expired-past-grace");
});

test("portfolio join for the demo user covers all eight markets", async () => {
  const src = predict.createDataSource({ programId: null, fixtureBundle: bundle });
  const decodedMarkets = await src.listMarkets();
  const demoPositions = await src.listPositionsForUser(bundle.users.demo);
  const portfolio = predict.buildPortfolio(decodedMarkets, demoPositions, 1799950000);
  assert.equal(portfolio.length, 8);
  for (const row of portfolio) {
    assert.ok(row.market && row.position && row.claimPreview, row.position.address);
    assert.equal(row.position.user, bundle.users.demo);
  }
  const m5 = portfolio.find((r) => r.market.address === markets[4].address);
  assert.equal(m5.claimPreview.amountRaw, "3333333");
});

test("trigger states pin against the recorded evidence fixtures", () => {
  const load = (name) => JSON.parse(fs.readFileSync(path.join(RESOLVER_EVIDENCE_DIR, name + ".json"), "utf8"));
  const states = (name) => predict.evidenceTriggerStates(ev.evaluateEvidence(load(name), ev.emptyCrewStore()));
  assert.deepEqual(states("clean"), ["not-fired", "not-fired", "not-fired", "not-fired", "not-fired"]);
  assert.deepEqual(states("h2-bundle"), ["not-fired", "fired", "not-fired", "not-fired", "not-fired"]);
  assert.deepEqual(states("h3-exit"), ["not-fired", "not-fired", "fired", "not-fired", "not-fired"]);
  assert.deepEqual(states("h3-unchecked"), ["not-fired", "not-fired", "unknown", "not-fired", "not-fired"]);
  assert.deepEqual(states("partial-liquidity"), ["not-fired", "not-fired", "not-fired", "unknown", "not-fired"]);
  assert.deepEqual(predict.evidenceTriggerStates(null), ["unknown", "unknown", "unknown", "unknown", "unknown"]);
});

test("enrichment marks a soft-only flag and passes evidence and metadata through", async () => {
  const src = predict.createDataSource({ programId: null, fixtureBundle: bundle });
  const decoded = await src.listMarkets();
  const softMarket = decoded.find((m) => m.address === markets[2].address);
  const softEvidence = JSON.parse(fs.readFileSync(path.join(RESOLVER_EVIDENCE_DIR, "soft-flagged.json"), "utf8"));
  const softResult = ev.evaluateEvidence(softEvidence, ev.emptyCrewStore());
  assert.equal(softResult.displayVerdict, "FLAGGED");
  assert.equal(softResult.primaryHardTrigger, 0);
  const view = predict.enrichMarket(softMarket, { position: null, evidenceResult: softResult, dexMeta: null, nowSec: 1799950000 });
  assert.equal(view.softFlagOnly, true);
  assert.equal(view.dexMeta, null);
  assert.equal(view.evidence, softResult);
  assert.deepEqual(view.evidence.missingCore, softResult.missingCore);
  assert.equal(view.phase, "open-live");
  assert.equal(view.odds.oneSided, true);
  const cleanResult = ev.evaluateEvidence(JSON.parse(fs.readFileSync(path.join(RESOLVER_EVIDENCE_DIR, "clean.json"), "utf8")), ev.emptyCrewStore());
  const cleanView = predict.enrichMarket(softMarket, { position: null, evidenceResult: cleanResult, dexMeta: bundle.token_meta[softMarket.subjectMint], nowSec: 1799950000 });
  assert.equal(cleanView.softFlagOnly, false);
  assert.equal(cleanView.dexMeta.symbol, "SOFT");
});

/* ---------------- fixture address integrity ----------------
 * The shipped-page break this guards: the bundle's readable label
 * addresses decoded to 33 bytes, so the client's PDA derivation
 * threw on every pre-seeded market. Market, live-market and
 * position addresses are now the derived PDAs under the deployed
 * program id; mints, users and creators are labels that decode to
 * exactly 32 bytes. Snapshot markets that share one mint share
 * one live-market PDA by construction (the live-market PDA is
 * seeded by the mint alone); their market PDAs differ because
 * the snapshot records carry created_at values a second apart. */

test("every address in the fixture bundle decodes to exactly 32 bytes", () => {
  const slots = [];
  slots.push(["program_id", bundle.program_id]);
  slots.push(["config.address", bundle.config.address]);
  slots.push(["config.resolver", bundle.config.resolver]);
  if (typeof bundle.config.admin === "string") slots.push(["config.admin", bundle.config.admin]);
  if (typeof bundle.resolver === "string") slots.push(["resolver", bundle.resolver]);
  for (const [k, v] of Object.entries(bundle.users)) slots.push(["users." + k, v]);
  for (const m of markets) {
    slots.push(["market.address", m.address]);
    slots.push(["market.live_market", m.live_market]);
    slots.push(["market.subject_mint", m.subject_mint]);
    slots.push(["market.creator", m.creator]);
  }
  for (const p of positions) {
    slots.push(["position.address", p.address]);
    slots.push(["position.market", p.market]);
    slots.push(["position.user", p.user]);
  }
  for (const k of Object.keys(bundle.token_meta)) slots.push(["token_meta key", k]);
  for (const k of Object.keys(bundle.evidence_by_market || {})) slots.push(["evidence_by_market key", k]);
  for (const [label, addr] of slots) {
    assert.equal(tx.base58Decode(addr).length, 32, label + " " + addr);
  }
  assert.equal(new Set(markets.map((m) => m.address)).size, markets.length, "market addresses are distinct");
  assert.equal(new Set(positions.map((p) => p.address)).size, positions.length, "position addresses are distinct");
});

test("fixture market, live-market and position addresses are the derived PDAs", () => {
  const pid = bundle.program_id;
  for (const m of markets) {
    const dm = tx.deriveMarket(pid, m.subject_mint, m.created_at);
    assert.equal(m.address, dm.address, m.evidence_file);
    assert.equal(m.bump, dm.bump, m.evidence_file);
    assert.equal(m.live_market, tx.deriveLiveMarket(pid, m.subject_mint).address, m.evidence_file);
    assert.equal(m.vault_bump, tx.deriveVault(pid, m.address).bump, m.evidence_file);
  }
  for (const p of positions) {
    const dp = tx.derivePosition(pid, p.market, p.user);
    assert.equal(p.address, dp.address, p.market);
    assert.equal(p.bump, dp.bump, p.market);
  }
});

/* ------------- Rust golden vectors (deployed program) -------------
 * The derived-PDA test above checks the bundle against this
 * codebase's own JavaScript derivation — the implementation grading
 * itself. The golden file is the independent derivation: it is
 * generated from the Rust program's seed constructions by
 * gen-pda-vectors-deployed.sh, and the Rust suite re-derives it
 * from the program's seeds (programs/crates/rug-call/tests/
 * pda_golden_vectors.rs). These tests compare the bundle's stored
 * addresses against the golden file by pure string equality — no
 * derive* call appears here on purpose. The bundle stores no vault
 * address and no live-market bump, so those two golden fields are
 * asserted only on the Rust side. */

test("fixture bundle addresses match the Rust-generated deployed PDA golden vectors", () => {
  assert.equal(golden.schema, "rugcall-pda-vectors-deployed/v1");
  assert.equal(golden.program_id, "DcwvEDi4yCk16wQ2HVzc3dQiBFccj8VxzboGUYdUXseZ");
  assert.equal(golden.program_id, bundle.program_id);
  const byMintAndTime = new Map();
  for (const gm of golden.markets) {
    const key = gm.subject_mint + ":" + gm.created_at;
    assert.ok(!byMintAndTime.has(key), "golden market key repeats: " + key);
    byMintAndTime.set(key, gm);
  }
  assert.equal(golden.markets.length, markets.length);
  const consumedMarkets = new Set();
  for (const m of markets) {
    const key = m.subject_mint + ":" + m.created_at;
    const gm = byMintAndTime.get(key);
    assert.ok(gm, "no golden vector for market " + m.address);
    consumedMarkets.add(key);
    assert.equal(m.address, gm.market.address, m.evidence_file);
    assert.equal(m.bump, gm.market.bump, m.evidence_file);
    assert.equal(m.live_market, gm.live_market.address, m.evidence_file);
    assert.equal(m.vault_bump, gm.vault.bump, m.evidence_file);
  }
  // Every golden entry must be consumed exactly once: a bundle
  // that duplicated one market row and dropped another (same total
  // length) would otherwise look the same vector up twice and pass.
  assert.equal(consumedMarkets.size, golden.markets.length, "every golden market vector is consumed");
  const byMarketAndUser = new Map();
  for (const gp of golden.positions) {
    const key = gp.market + ":" + gp.user;
    assert.ok(!byMarketAndUser.has(key), "golden position key repeats: " + key);
    byMarketAndUser.set(key, gp);
  }
  assert.equal(golden.positions.length, positions.length);
  const consumedPositions = new Set();
  for (const p of positions) {
    const key = p.market + ":" + p.user;
    const gp = byMarketAndUser.get(key);
    assert.ok(gp, "no golden vector for position " + p.address);
    consumedPositions.add(key);
    assert.equal(p.address, gp.address, p.market);
    assert.equal(p.bump, gp.bump, p.market);
  }
  assert.equal(consumedPositions.size, golden.positions.length, "every golden position vector is consumed");
});

test("deployed golden config PDA is the known on-chain config, not the fixture placeholder", () => {
  assert.equal(golden.config.address, "D8upBvHaUh7pX8PT3AZWFFWa78PkDJAJ5xPoumBSwANp");
  assert.equal(golden.config.bump, 252);
  // The bundle's config address stays the readable fixture label
  // (see config_note in app-markets.json); it is never compared
  // against the derived config PDA.
  assert.notEqual(bundle.config.address, golden.config.address);
});

test("the shell's own fixture address literals decode to exactly 32 bytes", () => {
  const index = fs.readFileSync(path.join(HERE, "..", "index.html"), "utf8");
  const literals = index.match(/"(Fixture[1-9A-HJ-NP-Za-km-z]+)"/g) || [];
  assert.equal(literals.length, 2, "the shell constructs two fixture address literals");
  for (const lit of literals) {
    const s = lit.slice(1, -1);
    assert.equal(tx.base58Decode(s).length, 32, s);
  }
});

/* ---------------- embedded evidence snapshots ---------------- */

test("the soft-flag market carries its recorded evidence snapshot, identical to the resolver fixture", () => {
  const soft = markets.find((m) => m.evidence_file === "soft-flagged.json");
  assert.ok(soft, "the bundle seeds the soft-flag market");
  assert.ok(bundle.evidence_by_market, "the bundle embeds evidence snapshots by market");
  assert.deepEqual(Object.keys(bundle.evidence_by_market), [soft.address]);
  const resolverFixture = JSON.parse(
    fs.readFileSync(path.join(RESOLVER_EVIDENCE_DIR, "soft-flagged.json"), "utf8"));
  assert.deepEqual(bundle.evidence_by_market[soft.address], resolverFixture);
  const result = ev.evaluateEvidence(bundle.evidence_by_market[soft.address], ev.emptyCrewStore());
  assert.equal(result.displayVerdict, "FLAGGED");
  assert.equal(result.primaryHardTrigger, 0);
  assert.equal(result.partial, false);
});

test("soft-flag panel: rendered detail is consistent with the fixture market and its snapshot", async () => {
  const soft = markets.find((m) => m.evidence_file === "soft-flagged.json");
  const snapshot = bundle.evidence_by_market[soft.address];
  // Snapshot-vs-market metadata: the snapshot the panel renders for
  // this market must describe this market — same mint and creator,
  // and its check/fetch timestamps anchored on the market's own
  // creation time, inside the market's term.
  assert.equal(snapshot.mint, soft.subject_mint);
  assert.equal(snapshot.deep.creator, soft.creator);
  assert.equal(snapshot.light.checkedAt, soft.created_at);
  assert.equal(snapshot.deep.creationTime, soft.created_at);
  assert.ok(snapshot.fetchedAtSec >= soft.created_at && snapshot.fetchedAtSec <= soft.expiry);
  // Rendered detail: subject fields come from the market object,
  // the evidence panel from the snapshot's evaluation; the two must
  // agree on screen — this market's identifiers and timestamps, the
  // FLAGGED verdict, the soft-score-only caption, and no other
  // market's identifiers leaking in.
  const src = predict.createDataSource({ programId: null, fixtureBundle: bundle });
  const decoded = (await src.listMarkets()).find((m) => m.address === soft.address);
  const evaluation = ev.evaluateEvidence(snapshot, ev.emptyCrewStore());
  const html = ui.renderMarketDetail({
    market: decoded, evaluation, evidence: snapshot, nowSec: 1799950000,
  });
  assert.ok(html.includes(soft.address), "panel names the soft-flag market");
  assert.ok(html.includes(soft.subject_mint), "panel names the soft-flag mint");
  assert.ok(html.includes(ui.formatTimestamp(soft.created_at)), "panel shows the market creation time");
  assert.ok(html.includes(ui.formatTimestamp(soft.expiry)), "panel shows the market expiry");
  assert.ok(html.includes("FLAGGED"), "panel shows the FLAGGED verdict");
  assert.ok(html.includes("Soft score only"), "panel carries the soft-score-only caption");
  for (const m of markets) {
    if (m.address === soft.address) continue;
    assert.ok(!html.includes(m.address), "panel must not name market " + m.address);
    assert.ok(!html.includes(m.subject_mint), "panel must not name mint " + m.subject_mint);
  }
});
