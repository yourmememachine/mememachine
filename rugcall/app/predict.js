/*
 * RugCall data layer (predict.js).
 *
 * Zero-dependency, environment-agnostic: the same file loads as a
 * Node module and as a browser classic script. It decodes the
 * on-chain account layouts defined by the RugCall program
 * (programs/crates/rug-call/src/state.rs), reads markets and
 * positions from either recorded fixtures or a devnet transport,
 * and computes the display math (phases, pool odds, claim and buy
 * previews) as pure functions over decoded objects.
 *
 * Honesty furniture, printed by every surface that uses this
 * module:
 *   FIXTURE_BANNER     -- the demo banner for the seeded board.
 *   FIXTURE_DATA_LABEL -- shown while the board is served from
 *                         fixtures because no program id is set.
 * Amounts are decimal strings at every boundary of this module;
 * BigInt appears only inside the math. Base58 encoding and the
 * parimutuel payout come from predict-tx.js (the RugCallTx
 * module) and are never reimplemented here.
 */
"use strict";

var FIXTURE_BANNER = "Devnet demo — play money (dUSDC), evidence read live from Solana mainnet";
var FIXTURE_DATA_LABEL = "Fixture data — devnet program not deployed yet (program_id null)";

/* Account lengths and discriminators, mirroring state.rs. */
var MARKET_LEN = 142;
var POSITION_LEN = 83;
var CONFIG_LEN = 122;
var LIVE_MARKET_LEN = 34;
var DISC_CONFIG = 1;
var DISC_MARKET = 2;
var DISC_POSITION = 3;
var DISC_LIVE_MARKET = 4;

var STATE_OPEN = 1;
var STATE_RESOLVED = 2;
var STATE_CANCELLED = 3;
var OUTCOME_NONE = 0;
var OUTCOME_FLAGGED = 1;
var OUTCOME_SURVIVES = 2;
var SIDE_FLAGGED = 1;
var SIDE_SURVIVES = 2;
var CANCEL_GRACE_SECS = 86400;
var U64_MAX = 18446744073709551615n;
var SAFE_MAX = 9007199254740991n;

/* ------------------------------------------------------------------ */
/* Errors                                                              */
/* ------------------------------------------------------------------ */

function PredictDecodeError(code, message) {
  var err = new Error(message || code);
  err.name = "PredictDecodeError";
  err.code = code;
  return err;
}

function PredictSourceError(code, message) {
  var err = new Error(message || code);
  err.name = "PredictSourceError";
  err.code = code;
  return err;
}

/* ------------------------------------------------------------------ */
/* predict-tx.js access (base58 + payout live there, nowhere else)     */
/* ------------------------------------------------------------------ */

var txCache = null;
function loadTx() {
  if (txCache) return txCache;
  var t = null;
  if (typeof require === "function") {
    try { t = require("./predict-tx.js"); } catch (err) { t = null; }
  }
  if (!t && typeof globalThis !== "undefined" && globalThis.RugCallTx) t = globalThis.RugCallTx;
  if (!t && typeof window !== "undefined" && window.RugCallTx) t = window.RugCallTx;
  if (t) txCache = t;
  return t;
}

function base58Encode(bytes) {
  var t = loadTx();
  if (!t || typeof t.base58Encode !== "function") {
    throw PredictDecodeError("TX_UNAVAILABLE", "RugCallTx.base58Encode is not available");
  }
  return t.base58Encode(bytes);
}

function parimutuelPayout(stake, total, winning) {
  var t = loadTx();
  if (!t || typeof t.parimutuelPayout !== "function") {
    throw PredictDecodeError("TX_UNAVAILABLE", "RugCallTx.parimutuelPayout is not available");
  }
  var out = t.parimutuelPayout(stake, total, winning);
  if (out === null || typeof out === "undefined") return null;
  return String(out);
}

/* ------------------------------------------------------------------ */
/* Byte helpers                                                        */
/* ------------------------------------------------------------------ */

function toBytes(input) {
  if (input instanceof Uint8Array) return input;
  if (typeof ArrayBuffer !== "undefined" && input instanceof ArrayBuffer) return new Uint8Array(input);
  if (Array.isArray(input)) return Uint8Array.from(input);
  if (input && typeof input === "object" && typeof input.length === "number") return Uint8Array.from(input);
  throw PredictDecodeError("BAD_LENGTH", "account data must be bytes");
}

function checkLen(bytes, len, label) {
  if (bytes.length !== len) {
    throw PredictDecodeError("BAD_LENGTH", label + " must be exactly " + len + " bytes, got " + bytes.length);
  }
}

function checkDisc(bytes, want, label) {
  if (bytes[0] !== want) {
    throw PredictDecodeError("BAD_DISCRIMINATOR", label + " discriminator must be " + want + ", got " + bytes[0]);
  }
}

function readPubkey(view, bytes, offset) {
  var slice = new Uint8Array(32);
  for (var i = 0; i < 32; i++) slice[i] = bytes[offset + i];
  return base58Encode(slice);
}

function readU64(view, offset) {
  return view.getBigUint64(offset, true).toString();
}

function readI64(view, offset) {
  var v = view.getBigInt64(offset, true);
  if (v > SAFE_MAX || v < -SAFE_MAX) {
    throw PredictDecodeError("BAD_TIMESTAMP", "timestamp outside the safe integer range: " + v.toString());
  }
  return Number(v);
}

function bytesToHex(bytes, start, end) {
  var out = "";
  for (var i = start; i < end; i++) {
    var h = bytes[i].toString(16);
    out += h.length === 1 ? "0" + h : h;
  }
  return out;
}

function stateNameOf(state) {
  return state === STATE_OPEN ? "open" : state === STATE_RESOLVED ? "resolved" : "cancelled";
}

function outcomeNameOf(outcome) {
  return outcome === OUTCOME_NONE ? "none" : outcome === OUTCOME_FLAGGED ? "flagged" : "survives";
}

/* ------------------------------------------------------------------ */
/* Decoders (layouts mirror state.rs byte for byte)                    */
/* ------------------------------------------------------------------ */

function decodeMarket(input, address) {
  var bytes = toBytes(input);
  checkLen(bytes, MARKET_LEN, "Market");
  checkDisc(bytes, DISC_MARKET, "Market");
  var view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  var state = bytes[97];
  if (state !== 1 && state !== 2 && state !== 3) {
    throw PredictDecodeError("BAD_STATE", "market state must be 1, 2 or 3, got " + state);
  }
  var outcome = bytes[98];
  if (outcome !== 0 && outcome !== 1 && outcome !== 2) {
    throw PredictDecodeError("BAD_OUTCOME", "market outcome must be 0, 1 or 2, got " + outcome);
  }
  var hardTrigger = bytes[131];
  if (!(hardTrigger >= 0 && hardTrigger <= 5)) {
    throw PredictDecodeError("BAD_TRIGGER", "hard trigger must be in 0..5, got " + hardTrigger);
  }
  var flaggedPool = readU64(view, 81);
  var survivesPool = readU64(view, 89);
  var evidenceHash = bytesToHex(bytes, 99, 131);
  return {
    address: typeof address === "string" ? address : null,
    subjectMint: readPubkey(view, bytes, 1),
    creator: readPubkey(view, bytes, 33),
    createdAt: readI64(view, 65),
    expiry: readI64(view, 73),
    flaggedPool: flaggedPool,
    survivesPool: survivesPool,
    totalPool: (BigInt(flaggedPool) + BigInt(survivesPool)).toString(),
    state: state,
    stateName: stateNameOf(state),
    outcome: outcome,
    outcomeName: outcomeNameOf(outcome),
    evidenceHash: evidenceHash,
    evidenceHashIsZero: /^0{64}$/.test(evidenceHash),
    hardTrigger: hardTrigger,
    resolvedAt: readI64(view, 132),
    bump: bytes[140],
    vaultBump: bytes[141]
  };
}

function decodePosition(input, address) {
  var bytes = toBytes(input);
  checkLen(bytes, POSITION_LEN, "Position");
  checkDisc(bytes, DISC_POSITION, "Position");
  var view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  var claimedByte = bytes[81];
  if (claimedByte !== 0 && claimedByte !== 1) {
    throw PredictDecodeError("BAD_BOOL", "position claimed byte must be 0 or 1, got " + claimedByte);
  }
  var flaggedAmount = readU64(view, 65);
  var survivesAmount = readU64(view, 73);
  return {
    address: typeof address === "string" ? address : null,
    market: readPubkey(view, bytes, 1),
    user: readPubkey(view, bytes, 33),
    flaggedAmount: flaggedAmount,
    survivesAmount: survivesAmount,
    totalStake: (BigInt(flaggedAmount) + BigInt(survivesAmount)).toString(),
    claimed: claimedByte === 1,
    bump: bytes[82]
  };
}

function decodeConfig(input, address) {
  var bytes = toBytes(input);
  checkLen(bytes, CONFIG_LEN, "Config");
  checkDisc(bytes, DISC_CONFIG, "Config");
  var view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return {
    address: typeof address === "string" ? address : null,
    admin: readPubkey(view, bytes, 1),
    resolver: readPubkey(view, bytes, 33),
    collateralMint: readPubkey(view, bytes, 65),
    feeBps: readU64(view, 97),
    defaultDurationSecs: readI64(view, 105),
    minStake: readU64(view, 113),
    bump: bytes[121]
  };
}

function decodeLiveMarket(input, address) {
  var bytes = toBytes(input);
  checkLen(bytes, LIVE_MARKET_LEN, "LiveMarket");
  checkDisc(bytes, DISC_LIVE_MARKET, "LiveMarket");
  var view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return {
    address: typeof address === "string" ? address : null,
    market: readPubkey(view, bytes, 1),
    bump: bytes[33]
  };
}

/* ------------------------------------------------------------------ */
/* Pure helpers                                                        */
/* ------------------------------------------------------------------ */

function marketPhase(market, nowSec) {
  if (!market) return "cancelled";
  if (market.state === STATE_RESOLVED || market.stateName === "resolved") return "resolved";
  if (market.state === STATE_CANCELLED || market.stateName === "cancelled") return "cancelled";
  var now = typeof nowSec === "number" ? nowSec : 0;
  if (now > market.expiry + CANCEL_GRACE_SECS) return "open-expired-past-grace";
  if (now > market.expiry) return "open-expired-grace";
  return "open-live";
}

function computeOdds(market) {
  var flagged = BigInt(market.flaggedPool);
  var survives = BigInt(market.survivesPool);
  var total = flagged + survives;
  if (total === 0n) {
    return { totalPool: "0", flaggedBps: null, survivesBps: null, flaggedPct: null, survivesPct: null, oneSided: false };
  }
  var flaggedBps = Number((flagged * 10000n) / total);
  var survivesBps = Number((survives * 10000n) / total);
  return {
    totalPool: total.toString(),
    flaggedBps: flaggedBps,
    survivesBps: survivesBps,
    flaggedPct: flaggedBps / 100,
    survivesPct: survivesBps / 100,
    oneSided: flagged === 0n || survives === 0n
  };
}

function totalStakeOf(position) {
  return (BigInt(position.flaggedAmount) + BigInt(position.survivesAmount)).toString();
}

/*
 * Claim preview with the frozen branch precedence: no position,
 * then already claimed, then market still open, then the
 * cancelled refund, then the resolved branches (inconsistent
 * outcome, zero winning pool refund, no winning stake, payout).
 * The total pool is built with checked addition inside the
 * payout branch only, so an overflowing pool can never block a
 * refund whose amount does not depend on the pools.
 */
function computeClaimPreview(market, position) {
  if (!position) return { kind: "none", reason: "no-position" };
  if (position.claimed) return { kind: "none", reason: "already-claimed" };
  if (!market || market.state === STATE_OPEN) return { kind: "none", reason: "market-open" };
  var totalStake = totalStakeOf(position);
  if (market.state === STATE_CANCELLED) {
    if (BigInt(totalStake) > 0n) return { kind: "refund", amountRaw: totalStake, reason: "cancelled" };
    return { kind: "no-claim", reason: "zero-total-stake" };
  }
  if (market.state === STATE_RESOLVED) {
    if (market.outcome !== OUTCOME_FLAGGED && market.outcome !== OUTCOME_SURVIVES) {
      return { kind: "error", reason: "inconsistent-state" };
    }
    var winningPool = market.outcome === OUTCOME_FLAGGED ? market.flaggedPool : market.survivesPool;
    var winningStake = market.outcome === OUTCOME_FLAGGED ? position.flaggedAmount : position.survivesAmount;
    if (BigInt(winningPool) === 0n) {
      if (BigInt(totalStake) > 0n) return { kind: "refund", amountRaw: totalStake, reason: "zero-winning-pool" };
      return { kind: "no-claim", reason: "zero-total-stake" };
    }
    if (BigInt(winningStake) === 0n) return { kind: "no-claim", reason: "no-winning-stake" };
    var totalPool = BigInt(market.flaggedPool) + BigInt(market.survivesPool);
    if (totalPool > U64_MAX) return { kind: "error", reason: "payout-overflow" };
    var payout = parimutuelPayout(winningStake, totalPool.toString(), winningPool);
    if (payout === null) return { kind: "error", reason: "payout-overflow" };
    return { kind: "payout", amountRaw: payout, reason: "winning-stake" };
  }
  return { kind: "error", reason: "inconsistent-state" };
}

function isCanonicalU64String(s) {
  if (typeof s !== "string" || !/^(0|[1-9][0-9]*)$/.test(s)) return false;
  try { return BigInt(s) <= U64_MAX; } catch (err) { return false; }
}

/*
 * Buy preview mirroring BuyPosition's check order: side, then
 * minimum stake, then market open, then the expiry tie (a buy at
 * exactly expiry is refused). Payout figures are computed
 * against the post-buy pools: what this buy alone would return
 * if its side wins, and what the caller's whole position on that
 * side would return.
 */
function previewBuy(market, position, side, amountRaw, config, nowSec) {
  function refused(reason) {
    return { kind: "refused", reason: reason, payoutOnThisBuyAlone: null, projectedPositionPayout: null };
  }
  if (side !== SIDE_FLAGGED && side !== SIDE_SURVIVES) return refused("invalid-side");
  if (!isCanonicalU64String(amountRaw)) return refused("below-min-stake");
  var minStake = config && typeof config.minStake !== "undefined" ? String(config.minStake) : "0";
  if (!isCanonicalU64String(minStake)) minStake = "0";
  if (BigInt(amountRaw) < BigInt(minStake)) return refused("below-min-stake");
  if (!market || market.state !== STATE_OPEN) return refused("market-not-open");
  var now = typeof nowSec === "number" ? nowSec : 0;
  if (now >= market.expiry) return refused("at-or-after-expiry");
  var amount = BigInt(amountRaw);
  var flaggedAfter = BigInt(market.flaggedPool) + (side === SIDE_FLAGGED ? amount : 0n);
  var survivesAfter = BigInt(market.survivesPool) + (side === SIDE_SURVIVES ? amount : 0n);
  var totalAfter = flaggedAfter + survivesAfter;
  if (totalAfter > U64_MAX || flaggedAfter > U64_MAX || survivesAfter > U64_MAX) {
    return { kind: "ok", reason: null, side: side, amountRaw: amountRaw, payoutOnThisBuyAlone: null, projectedPositionPayout: null };
  }
  var winningAfter = side === SIDE_FLAGGED ? flaggedAfter : survivesAfter;
  var existing = 0n;
  if (position) existing = BigInt(side === SIDE_FLAGGED ? position.flaggedAmount : position.survivesAmount);
  var alone = parimutuelPayout(amount.toString(), totalAfter.toString(), winningAfter.toString());
  var projected = parimutuelPayout((existing + amount).toString(), totalAfter.toString(), winningAfter.toString());
  return { kind: "ok", reason: null, side: side, amountRaw: amountRaw, payoutOnThisBuyAlone: alone, projectedPositionPayout: projected };
}

function formatDusdc(rawString) {
  var v = BigInt(rawString);
  var neg = v < 0n;
  if (neg) v = -v;
  var whole = v / 1000000n;
  var frac = v % 1000000n;
  var fracStr = frac.toString();
  while (fracStr.length < 6) fracStr = "0" + fracStr;
  return (neg ? "-" : "") + whole.toString() + "." + fracStr;
}

function parseDusdcToRaw(str) {
  if (typeof str !== "string") return null;
  var m = /^(0|[1-9][0-9]*)(?:\.([0-9]{1,6}))?$/.exec(str);
  if (!m) return null;
  var whole = BigInt(m[1]);
  var fracStr = m[2] || "";
  while (fracStr.length < 6) fracStr += "0";
  var raw = whole * 1000000n + (fracStr ? BigInt(fracStr) : 0n);
  if (raw > U64_MAX) return null;
  return raw.toString();
}

/*
 * Per-trigger display states from an evaluateEvidence result.
 * A trigger reads as unknown whenever the data it depends on was
 * never checked: H1 needs a history with a numeric strike count,
 * H2 needs both the bundle and the funder read, H3 needs the
 * sells check to have run, H4 needs a liquidity state other than
 * unknown, and H5 needs the funder read. A definite trigger is
 * then fired or not-fired on its recorded flag.
 */
function evidenceTriggerStates(evalResult) {
  var unknown5 = ["unknown", "unknown", "unknown", "unknown", "unknown"];
  if (!evalResult || !Array.isArray(evalResult.hardTriggers)) return unknown5.slice();
  var missing = Array.isArray(evalResult.missingCore) ? evalResult.missingCore : null;
  function trig(id) {
    for (var i = 0; i < evalResult.hardTriggers.length; i++) {
      if (evalResult.hardTriggers[i] && evalResult.hardTriggers[i].id === id) return evalResult.hardTriggers[i];
    }
    return null;
  }
  function vals(t) { return (t && t.values && typeof t.values === "object") ? t.values : {}; }
  var t1 = trig(1), t2 = trig(2), t3 = trig(3), t4 = trig(4), t5 = trig(5);
  var v1 = vals(t1), v2 = vals(t2), v3 = vals(t3), v4 = vals(t4);
  var definite = [
    typeof v1.strikes === "number" && (!missing || missing.indexOf("history") === -1),
    typeof v2.pct === "number" && typeof v2.sharedFunderInWindow === "boolean" &&
      (!missing || (missing.indexOf("bundle") === -1 && missing.indexOf("funders") === -1)),
    v3.checked === true,
    typeof v4.state === "string" && v4.state !== "unknown",
    !missing || missing.indexOf("funders") === -1
  ];
  var list = [t1, t2, t3, t4, t5];
  return list.map(function (t, i) {
    if (!t || !definite[i]) return "unknown";
    return t.fired ? "fired" : "not-fired";
  });
}

function enrichMarket(market, opts) {
  var o = opts || {};
  var position = typeof o.position !== "undefined" ? o.position : null;
  var evidenceResult = typeof o.evidenceResult !== "undefined" ? o.evidenceResult : null;
  var dexMeta = typeof o.dexMeta !== "undefined" ? o.dexMeta : null;
  var nowSec = typeof o.nowSec === "number" ? o.nowSec : Math.floor(Date.now() / 1000);
  var view = {};
  Object.keys(market || {}).forEach(function (k) { view[k] = market[k]; });
  view.position = position;
  view.phase = marketPhase(market, nowSec);
  view.odds = computeOdds(market);
  view.claimPreview = computeClaimPreview(market, position);
  view.softFlagOnly = !!(evidenceResult && evidenceResult.displayVerdict === "FLAGGED" && evidenceResult.primaryHardTrigger === 0);
  view.triggerStates = evidenceTriggerStates(evidenceResult);
  view.evidence = evidenceResult;
  view.dexMeta = dexMeta;
  return view;
}

function buildPortfolio(markets, positions, nowSec) {
  var byAddress = {};
  (markets || []).forEach(function (m) { if (m && m.address) byAddress[m.address] = m; });
  var out = [];
  (positions || []).forEach(function (p) {
    var market = p ? byAddress[p.market] : null;
    if (!market) return;
    out.push({ market: market, position: p, claimPreview: computeClaimPreview(market, p) });
  });
  return out;
}

/* ------------------------------------------------------------------ */
/* Fixture normalization (same validators as the decoders)             */
/* ------------------------------------------------------------------ */

function fixtureFail(message) {
  throw PredictSourceError("FIXTURE_INVALID", message);
}

function normU64(value, label) {
  var s = typeof value === "number" ? String(value) : value;
  if (!isCanonicalU64String(s)) fixtureFail(label + " is not a canonical u64 string: " + JSON.stringify(value));
  return s;
}

function normTimestamp(value, label) {
  if (typeof value !== "number" || !Number.isSafeInteger(value)) fixtureFail(label + " is not a safe integer timestamp");
  return value;
}

function normFixtureMarket(raw) {
  if (!raw || typeof raw !== "object") fixtureFail("fixture market is not an object");
  if (typeof raw.address !== "string" || !raw.address) fixtureFail("fixture market is missing an address");
  if (typeof raw.subject_mint !== "string" || !raw.subject_mint) fixtureFail("market " + raw.address + " is missing subject_mint");
  if (typeof raw.creator !== "string" || !raw.creator) fixtureFail("market " + raw.address + " is missing creator");
  var state = raw.state;
  if (state !== 1 && state !== 2 && state !== 3) fixtureFail("market " + raw.address + " has bad state " + state);
  var outcome = typeof raw.outcome === "number" ? raw.outcome : 0;
  if (outcome !== 0 && outcome !== 1 && outcome !== 2) fixtureFail("market " + raw.address + " has bad outcome " + outcome);
  var trigger = typeof raw.hard_trigger === "number" ? raw.hard_trigger : 0;
  if (!(trigger >= 0 && trigger <= 5)) fixtureFail("market " + raw.address + " has bad hard_trigger " + trigger);
  var hash = typeof raw.evidence_hash === "string" ? raw.evidence_hash.toLowerCase() : "0".repeat(64);
  if (!/^[0-9a-f]{64}$/.test(hash)) fixtureFail("market " + raw.address + " has a malformed evidence_hash");
  var flaggedPool = normU64(raw.flagged_pool, "market " + raw.address + " flagged_pool");
  var survivesPool = normU64(raw.survives_pool, "market " + raw.address + " survives_pool");
  return {
    address: raw.address,
    subjectMint: raw.subject_mint,
    creator: raw.creator,
    createdAt: normTimestamp(raw.created_at, "market " + raw.address + " created_at"),
    expiry: normTimestamp(raw.expiry, "market " + raw.address + " expiry"),
    flaggedPool: flaggedPool,
    survivesPool: survivesPool,
    totalPool: (BigInt(flaggedPool) + BigInt(survivesPool)).toString(),
    state: state,
    stateName: stateNameOf(state),
    outcome: outcome,
    outcomeName: outcomeNameOf(outcome),
    evidenceHash: hash,
    evidenceHashIsZero: /^0{64}$/.test(hash),
    hardTrigger: trigger,
    resolvedAt: normTimestamp(typeof raw.resolved_at === "number" ? raw.resolved_at : 0, "market " + raw.address + " resolved_at"),
    bump: typeof raw.bump === "number" ? raw.bump : 0,
    vaultBump: typeof raw.vault_bump === "number" ? raw.vault_bump : 0,
    liveMarket: typeof raw.live_market === "string" ? raw.live_market : null,
    evidenceFile: typeof raw.evidence_file === "string" ? raw.evidence_file : null
  };
}

function normFixturePosition(raw) {
  if (!raw || typeof raw !== "object") fixtureFail("fixture position is not an object");
  if (typeof raw.address !== "string" || !raw.address) fixtureFail("fixture position is missing an address");
  if (typeof raw.market !== "string" || !raw.market) fixtureFail("position " + raw.address + " is missing market");
  if (typeof raw.user !== "string" || !raw.user) fixtureFail("position " + raw.address + " is missing user");
  var flaggedAmount = normU64(raw.flagged_amount, "position " + raw.address + " flagged_amount");
  var survivesAmount = normU64(raw.survives_amount, "position " + raw.address + " survives_amount");
  return {
    address: raw.address,
    market: raw.market,
    user: raw.user,
    flaggedAmount: flaggedAmount,
    survivesAmount: survivesAmount,
    totalStake: (BigInt(flaggedAmount) + BigInt(survivesAmount)).toString(),
    claimed: raw.claimed === true,
    bump: typeof raw.bump === "number" ? raw.bump : 0,
    crafted: raw.crafted === true
  };
}

function normFixtureConfig(bundle) {
  var cfg = bundle ? bundle.config : null;
  if (typeof cfg === "string") {
    return {
      address: cfg,
      admin: null,
      resolver: typeof bundle.resolver === "string" ? bundle.resolver : null,
      collateralMint: null,
      feeBps: "0",
      defaultDurationSecs: 86400,
      minStake: "1000000",
      bump: 254
    };
  }
  if (!cfg || typeof cfg !== "object") fixtureFail("fixture bundle has no config object");
  return {
    address: typeof cfg.address === "string" ? cfg.address : null,
    admin: typeof cfg.admin === "string" ? cfg.admin : null,
    resolver: typeof cfg.resolver === "string" ? cfg.resolver : (typeof bundle.resolver === "string" ? bundle.resolver : null),
    collateralMint: typeof cfg.collateral_mint === "string" ? cfg.collateral_mint : (cfg.collateral_mint === null ? null : null),
    feeBps: typeof cfg.fee_bps !== "undefined" ? normU64(cfg.fee_bps, "config fee_bps") : "0",
    defaultDurationSecs: typeof cfg.default_duration_secs === "number" ? cfg.default_duration_secs : 86400,
    minStake: typeof cfg.min_stake !== "undefined" ? normU64(cfg.min_stake, "config min_stake") : "1000000",
    bump: typeof cfg.bump === "number" ? cfg.bump : 254
  };
}

function sortMarkets(markets) {
  return markets.sort(function (a, b) {
    if (b.createdAt !== a.createdAt) return b.createdAt - a.createdAt;
    if (a.address < b.address) return -1;
    if (a.address > b.address) return 1;
    return 0;
  });
}

/* ------------------------------------------------------------------ */
/* Data sources                                                        */
/*
 * A source reads through one of two backends:
 *  - fixture: an injected bundle in the recorded-fixture field
 *    naming (subject_mint, flagged_pool, ...), normalized through
 *    the same validators the decoders apply. Every call returns
 *    fresh objects.
 *  - devnet: an injected transport with
 *      getProgramAccounts(programId, { dataSize, memcmp }) ->
 *        [{ pubkey, data }]
 *      getAccountInfo(address) -> { data } | null
 *      getMultipleAccounts(addresses) -> [{ data } | null]
 *    where data is the raw account bytes. A transport failure
 *    rejects with PredictSourceError code RPC_FAILED; a source
 *    that cannot answer the question as asked rejects with
 *    NOT_CONFIGURED. A source never answers a failure with an
 *    empty list.
 */
/* ------------------------------------------------------------------ */

function createFixtureSource(programId, fixtureBundle, nowSec) {
  function bundle() {
    if (!fixtureBundle || typeof fixtureBundle !== "object") {
      throw PredictSourceError("FIXTURE_INVALID", "no fixture bundle was supplied");
    }
    return fixtureBundle;
  }
  function markets() {
    var b = bundle();
    if (!Array.isArray(b.markets)) throw PredictSourceError("FIXTURE_INVALID", "fixture bundle has no markets array");
    return b.markets.map(normFixtureMarket);
  }
  function positions() {
    var b = bundle();
    if (typeof b.positions === "undefined") return [];
    if (!Array.isArray(b.positions)) throw PredictSourceError("FIXTURE_INVALID", "fixture bundle positions is not an array");
    return b.positions.map(normFixturePosition);
  }
  return {
    kind: "fixture",
    programId: programId,
    nowSec: typeof nowSec === "number" ? nowSec : null,
    getConfig: async function () { return normFixtureConfig(bundle()); },
    listMarkets: async function () { return sortMarkets(markets()); },
    getMarket: async function (address) {
      var found = markets().filter(function (m) { return m.address === address; });
      return found.length ? found[0] : null;
    },
    getLiveMarketForMint: async function (mint) {
      var open = markets().filter(function (m) { return m.subjectMint === mint && m.state === STATE_OPEN; });
      if (!open.length) return null;
      var m = sortMarkets(open)[0];
      return { address: m.liveMarket, market: m.address, bump: null };
    },
    listPositions: async function (filter) {
      var f = filter || {};
      return positions().filter(function (p) {
        if (typeof f.market === "string" && p.market !== f.market) return false;
        if (typeof f.user === "string" && p.user !== f.user) return false;
        return true;
      });
    },
    getPosition: async function (address) {
      var found = positions().filter(function (p) { return p.address === address; });
      return found.length ? found[0] : null;
    },
    listPositionsForUser: async function (user) {
      return positions().filter(function (p) { return p.user === user; });
    }
  };
}

function createDevnetSource(programId, transport, nowSec) {
  function needTransport() {
    if (!transport || typeof transport !== "object") {
      throw PredictSourceError("NOT_CONFIGURED", "no transport was supplied for the devnet source");
    }
    return transport;
  }
  async function call(fn) {
    try {
      return await fn();
    } catch (err) {
      if (err && err.name === "PredictSourceError") throw err;
      if (err && err.name === "PredictDecodeError") throw err;
      throw PredictSourceError("RPC_FAILED", "transport failed: " + (err && err.message ? err.message : String(err)));
    }
  }
  function discMemcmp(disc) {
    return { offset: 0, bytes: base58Encode(Uint8Array.of(disc)) };
  }
  function accountData(entry) {
    if (!entry) return null;
    if (entry.data) return entry.data;
    if (entry.account && entry.account.data) return entry.account.data;
    return null;
  }
  var source = {
    kind: "devnet",
    programId: programId,
    nowSec: typeof nowSec === "number" ? nowSec : null,
    getConfig: async function () {
      var t = needTransport();
      var rows = await call(function () {
        return t.getProgramAccounts(programId, { dataSize: CONFIG_LEN, memcmp: [discMemcmp(DISC_CONFIG)] });
      });
      if (!Array.isArray(rows) || rows.length === 0) {
        throw PredictSourceError("NOT_CONFIGURED", "no config account found for this program");
      }
      return decodeConfig(accountData(rows[0]), rows[0].pubkey || null);
    },
    listMarkets: async function () {
      var t = needTransport();
      var rows = await call(function () {
        return t.getProgramAccounts(programId, { dataSize: MARKET_LEN, memcmp: [discMemcmp(DISC_MARKET)] });
      });
      var decoded = (rows || []).map(function (r) { return decodeMarket(accountData(r), r.pubkey || null); });
      return sortMarkets(decoded);
    },
    getMarket: async function (address) {
      var t = needTransport();
      var info = await call(function () { return t.getAccountInfo(address); });
      var data = accountData(info);
      if (!data) return null;
      return decodeMarket(data, address);
    },
    getLiveMarketForMint: async function (mint) {
      var t = needTransport();
      var rows = await call(function () {
        return t.getProgramAccounts(programId, { dataSize: LIVE_MARKET_LEN, memcmp: [discMemcmp(DISC_LIVE_MARKET)] });
      });
      var lives = (rows || []).map(function (r) { return decodeLiveMarket(accountData(r), r.pubkey || null); });
      if (!lives.length) return null;
      var marketsByAddress = {};
      if (typeof t.getMultipleAccounts === "function") {
        var infos = await call(function () {
          return t.getMultipleAccounts(lives.map(function (l) { return l.market; }));
        });
        lives.forEach(function (l, i) {
          var data = infos && infos[i] ? accountData(infos[i]) : null;
          if (data) {
            try { marketsByAddress[l.market] = decodeMarket(data, l.market); } catch (err) { /* skip unreadable */ }
          }
        });
      } else {
        for (var i = 0; i < lives.length; i++) {
          var m = await source.getMarket(lives[i].market);
          if (m) marketsByAddress[lives[i].market] = m;
        }
      }
      for (var j = 0; j < lives.length; j++) {
        var mk = marketsByAddress[lives[j].market];
        if (mk && mk.subjectMint === mint && mk.state === STATE_OPEN) return lives[j];
      }
      return null;
    },
    listPositions: async function (filter) {
      var f = filter || {};
      var hasMarket = typeof f.market === "string" && !!f.market;
      var hasUser = typeof f.user === "string" && !!f.user;
      if (!hasMarket && !hasUser) {
        throw PredictSourceError("NOT_CONFIGURED", "listPositions on the devnet source requires a market or user filter");
      }
      var t = needTransport();
      var memcmp = [discMemcmp(DISC_POSITION)];
      if (hasMarket) memcmp.push({ offset: 1, bytes: f.market });
      if (hasUser) memcmp.push({ offset: 33, bytes: f.user });
      var rows = await call(function () {
        return t.getProgramAccounts(programId, { dataSize: POSITION_LEN, memcmp: memcmp });
      });
      return (rows || []).map(function (r) { return decodePosition(accountData(r), r.pubkey || null); });
    },
    getPosition: async function (address) {
      var t = needTransport();
      var info = await call(function () { return t.getAccountInfo(address); });
      var data = accountData(info);
      if (!data) return null;
      return decodePosition(data, address);
    },
    listPositionsForUser: async function (user) {
      return source.listPositions({ user: user });
    }
  };
  return source;
}

function createDataSource(opts) {
  var o = opts || {};
  var programId = typeof o.programId === "undefined" ? null : o.programId;
  if (programId === null) return createFixtureSource(null, o.fixtureBundle, o.nowSec);
  return createDevnetSource(programId, o.transport, o.nowSec);
}

/* ------------------------------------------------------------------ */
/* Exports (Node module + browser global)                              */
/* ------------------------------------------------------------------ */

var RugCallPredict = {
  FIXTURE_BANNER: FIXTURE_BANNER,
  FIXTURE_DATA_LABEL: FIXTURE_DATA_LABEL,
  MARKET_LEN: MARKET_LEN,
  POSITION_LEN: POSITION_LEN,
  CONFIG_LEN: CONFIG_LEN,
  LIVE_MARKET_LEN: LIVE_MARKET_LEN,
  PredictDecodeError: PredictDecodeError,
  PredictSourceError: PredictSourceError,
  decodeMarket: decodeMarket,
  decodePosition: decodePosition,
  decodeConfig: decodeConfig,
  decodeLiveMarket: decodeLiveMarket,
  createDataSource: createDataSource,
  marketPhase: marketPhase,
  computeOdds: computeOdds,
  computeClaimPreview: computeClaimPreview,
  previewBuy: previewBuy,
  formatDusdc: formatDusdc,
  parseDusdcToRaw: parseDusdcToRaw,
  evidenceTriggerStates: evidenceTriggerStates,
  enrichMarket: enrichMarket,
  buildPortfolio: buildPortfolio
};

if (typeof module !== "undefined" && module.exports) module.exports = RugCallPredict;
if (typeof window !== "undefined") window.RugCall = RugCallPredict;
