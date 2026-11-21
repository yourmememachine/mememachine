/*
 * RugCall app UI (predict-ui.js).
 *
 * DOM UI module for the RugCall prediction market app. It renders and
 * wires against three injected collaborators:
 *   - a data source from predict.js (window.RugCall.createDataSource),
 *   - a predict client from predict-tx.js (window.RugCallTx),
 *   - a wallet (a provider adapter or the demo wallet).
 * The evidence panel evaluates recorded evidence through the shared
 * evidence module (evaluateEvidence / emptyCrewStore globals, or the
 * Node export of evidence.js when loaded under Node).
 *
 * The pure string-render helpers are exported for Node so they can be
 * unit-checked without a DOM; loading this file without a DOM does
 * not touch document or window at import time beyond feature checks.
 *
 * Honesty rules enforced here:
 *   - Fixture mode never renders a signature, an explorer link, or a
 *     success word as if a transaction really landed. Simulated
 *     result envelopes are labelled simulated everywhere they appear.
 *   - A missing token price or liquidity renders as unavailable,
 *     never as a fabricated zero.
 *   - An evidence trigger whose inputs were not checked renders as
 *     UNKNOWN, never as not-fired.
 *   - Amounts are displayed through formatDusdc from raw strings.
 */
"use strict";

/* ------------------------------------------------------------------ */
/* Constants (fallbacks; the predict.js constants win when present)    */
/* ------------------------------------------------------------------ */

var FALLBACK_FIXTURE_BANNER =
  "Fixture mode - recorded test markets and balances. Nothing on this page is a live transaction.";
var FALLBACK_FIXTURE_DATA_LABEL = "Fixture data";
var NETWORK_BANNER_TEXT = "Markets live on Devnet \u2014 set your wallet to Devnet";
var GRACE_SEC = 86400;

var MARKET_TERMS_TEXT =
  "Markets resolve on hard triggers H1\u2013H5 only. Soft scores never resolve a market. " +
  "Trigger event timestamps - not detection times - decide boundary cases. " +
  "If evidence is missing at expiry, resolution defers to a 24h grace window; " +
  "after the grace window anyone can cancel the market for full refunds.";

/* ------------------------------------------------------------------ */
/* Small utilities                                                     */
/* ------------------------------------------------------------------ */

function esc(value) {
  if (value === null || typeof value === "undefined") return "";
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function shortMint(address) {
  if (!address || typeof address !== "string") return "";
  if (address.length <= 12) return address;
  return address.slice(0, 4) + "..." + address.slice(-4);
}

function toBig(raw) {
  try {
    if (raw === null || typeof raw === "undefined" || raw === "") return 0n;
    return BigInt(String(raw));
  } catch (err) {
    return 0n;
  }
}

function groupThousands(digits) {
  return String(digits).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/* Fallback dUSDC formatting (6 decimals), used only when the
   predict.js helper is not loaded. Raw values are decimal strings. */
function fallbackFormatDusdc(raw) {
  if (raw === null || typeof raw === "undefined" || raw === "") return "0 dUSDC";
  var neg = false;
  var v;
  try {
    v = BigInt(String(raw));
  } catch (err) {
    return String(raw) + " dUSDC";
  }
  if (v < 0n) { neg = true; v = -v; }
  var whole = v / 1000000n;
  var frac = v % 1000000n;
  var out = groupThousands(whole.toString());
  if (frac !== 0n) {
    var f = frac.toString().padStart(6, "0").replace(/0+$/, "");
    out += "." + f;
  }
  return (neg ? "-" : "") + out + " dUSDC";
}

function fallbackParseDusdcToRaw(text) {
  if (text === null || typeof text === "undefined") return null;
  var s = String(text).trim().replace(/,/g, "");
  if (!s) return null;
  if (!/^\d+(\.\d+)?$/.test(s)) return null;
  var parts = s.split(".");
  var whole = parts[0] || "0";
  var frac = ((parts[1] || "") + "000000").slice(0, 6);
  if (parts[1] && parts[1].length > 6 && /[^0]/.test(parts[1].slice(6))) return null;
  try {
    return (BigInt(whole) * 1000000n + BigInt(frac || "0")).toString();
  } catch (err) {
    return null;
  }
}

function rugCallApi() {
  if (typeof globalThis !== "undefined" && globalThis.RugCall) return globalThis.RugCall;
  return null;
}

function formatDusdc(raw) {
  var api = rugCallApi();
  if (api && typeof api.formatDusdc === "function") {
    var out = api.formatDusdc(raw);
    return /dUSDC$/.test(out) ? out : out + " dUSDC";
  }
  return fallbackFormatDusdc(raw);
}

function capName(name) {
  if (!name) return "";
  var n = String(name);
  return n.charAt(0).toUpperCase() + n.slice(1);
}

var REASON_LABELS = {
  "invalid-side": "invalid side",
  "below-min-stake": "below the minimum stake",
  "market-not-open": "market is not open",
  "at-or-after-expiry": "market has expired",
  "cancelled": "market cancelled",
  "zero-winning-pool": "one-sided pool - exact stakes back",
  "no-winning-stake": "no winning stake",
  "inconsistent-state": "inconsistent market state",
  "already-claimed": "already claimed",
  "winning-stake": "winning stake",
  "market-open": "market still open",
  "no-position": "no position",
  "zero-total-stake": "no stake recorded"
};

function reasonLabel(reason) {
  if (!reason) return "";
  return REASON_LABELS[reason] || String(reason);
}

function parseDusdcToRaw(text) {
  var api = rugCallApi();
  if (api && typeof api.parseDusdcToRaw === "function") return api.parseDusdcToRaw(text);
  return fallbackParseDusdcToRaw(text);
}

function fixtureBannerText() {
  var api = rugCallApi();
  return (api && api.FIXTURE_BANNER) || FALLBACK_FIXTURE_BANNER;
}

function fixtureDataLabel() {
  var api = rugCallApi();
  return (api && api.FIXTURE_DATA_LABEL) || FALLBACK_FIXTURE_DATA_LABEL;
}

function evidenceApi() {
  var g = typeof globalThis !== "undefined" ? globalThis : {};
  if (typeof g.evaluateEvidence === "function") {
    return { evaluateEvidence: g.evaluateEvidence, emptyCrewStore: g.emptyCrewStore };
  }
  if (typeof require !== "undefined") {
    try {
      var mod = require("./evidence.js");
      if (mod && typeof mod.evaluateEvidence === "function") return mod;
    } catch (err) { /* browser or not present */ }
  }
  return null;
}

/* ------------------------------------------------------------------ */
/* Market math fallbacks (predict.js helpers win when present)         */
/* ------------------------------------------------------------------ */

function fallbackMarketPhase(market, nowSec) {
  if (!market) return "unknown";
  var stateName = String(market.stateName || "").toLowerCase();
  if (stateName === "resolved" || market.state === 2) return "resolved";
  if (stateName === "cancelled" || market.state === 3) return "cancelled";
  var now = typeof nowSec === "number" ? nowSec : Math.floor(Date.now() / 1000);
  var expiry = typeof market.expiry === "number" ? market.expiry : Number(market.expiry || 0);
  if (now < expiry) return "open-live";
  if (now < expiry + GRACE_SEC) return "open-expired-grace";
  return "open-expired-past-grace";
}

function marketPhase(market, nowSec) {
  var api = rugCallApi();
  if (api && typeof api.marketPhase === "function") {
    try { return api.marketPhase(market, nowSec); } catch (err) { /* fall through */ }
  }
  return fallbackMarketPhase(market, nowSec);
}

function fallbackComputeOdds(market) {
  var flagged = toBig(market && market.flaggedPool);
  var survives = toBig(market && market.survivesPool);
  var total = flagged + survives;
  if (total === 0n) {
    return { flaggedBps: null, survivesBps: null, flaggedPct: null, survivesPct: null, oneSided: false, totalPool: "0" };
  }
  /* Canonical floored bps pair, like predict.js computeOdds:
     each side is floored independently (they can sum to 9999). */
  var fBps = Number((flagged * 10000n) / total);
  var sBps = Number((survives * 10000n) / total);
  return {
    flaggedBps: fBps,
    survivesBps: sBps,
    flaggedPct: fBps / 100,
    survivesPct: sBps / 100,
    oneSided: flagged === 0n || survives === 0n,
    totalPool: total.toString()
  };
}

function computeOdds(market) {
  var api = rugCallApi();
  if (api && typeof api.computeOdds === "function") {
    try {
      var o = api.computeOdds(market);
      if (o && typeof o === "object" && "totalPool" in o) return o;
    } catch (err) { /* fall through */ }
  }
  return fallbackComputeOdds(market);
}

function fallbackClaimPreview(market, position) {
  if (!market || !position) return { kind: "none" };
  var flagged = toBig(position.flaggedAmount != null ? position.flaggedAmount : position.flagged_amount);
  var survives = toBig(position.survivesAmount != null ? position.survivesAmount : position.survives_amount);
  var totalStake = flagged + survives;
  var claimed = !!(position.claimed);
  if (claimed) return { kind: "none", reason: "already-claimed" };
  if (totalStake === 0n) return { kind: "none" };
  var phase = fallbackMarketPhase(market, Math.floor(Date.now() / 1000));
  if (phase === "cancelled") {
    return { kind: "refund", amountRaw: totalStake.toString(), reason: "market-cancelled" };
  }
  if (phase !== "resolved") return { kind: "none" };
  var totalPool = toBig(market.totalPool != null ? market.totalPool : (toBig(market.flaggedPool) + toBig(market.survivesPool)));
  var flaggedWon = market.outcome === 1 || String(market.outcomeName || "").toLowerCase() === "flagged";
  var survivesWon = market.outcome === 2 || String(market.outcomeName || "").toLowerCase() === "survives";
  if (!flaggedWon && !survivesWon) return { kind: "error", reason: "inconsistent-state" };
  var winStake = flaggedWon ? flagged : survives;
  var winPool = flaggedWon ? toBig(market.flaggedPool) : toBig(market.survivesPool);
  /* Mirror the program (and predict.js): the zero-winning-pool
     refund is checked BEFORE the winning-stake check — a holder
     with no winning-side stake is still refunded in full when
     the winning pool is zero. A one-sided pool where the winning
     side IS the whole pool is not a refund: it pays through the
     payout branch below (winStake * totalPool / winPool, which
     is exactly the winning-side stake). */
  if (winPool === 0n) {
    if (totalStake > 0n) return { kind: "refund", amountRaw: totalStake.toString(), reason: "zero-winning-pool" };
    return { kind: "no-claim", reason: "zero-total-stake" };
  }
  if (winStake === 0n) return { kind: "no-claim", reason: "no-winning-stake" };
  var payout = (winStake * totalPool) / winPool;
  return { kind: "payout", amountRaw: payout.toString() };
}

function computeClaimPreview(market, position) {
  var api = rugCallApi();
  if (api && typeof api.computeClaimPreview === "function") {
    try {
      var p = api.computeClaimPreview(market, position);
      if (p && typeof p.kind === "string") return p;
    } catch (err) {
      return { kind: "error", reason: String(err && err.message ? err.message : err) };
    }
  }
  return fallbackClaimPreview(market, position);
}

/* Local parimutuel preview math. Returns raw decimal strings. */
function localPreviewBuy(market, position, side, amountRaw) {
  var amount = toBig(amountRaw);
  var flagged = toBig(market.flaggedPool);
  var survives = toBig(market.survivesPool);
  var total = flagged + survives;
  var sidePool = side === "survives" ? survives : flagged;
  var existing = 0n;
  if (position) {
    existing = side === "survives"
      ? toBig(position.survivesAmount != null ? position.survivesAmount : position.survives_amount)
      : toBig(position.flaggedAmount != null ? position.flaggedAmount : position.flagged_amount);
  }
  var newTotal = total + amount;
  var newSidePool = sidePool + amount;
  if (amount === 0n || newSidePool === 0n) {
    return { thisBuyReturnRaw: "0", totalReturnRaw: "0", errors: [] };
  }
  var thisReturn = (amount * newTotal) / newSidePool;
  var totalReturn = ((existing + amount) * newTotal) / newSidePool;
  return { thisBuyReturnRaw: thisReturn.toString(), totalReturnRaw: totalReturn.toString(), errors: [] };
}

function previewBuy(market, position, side, amountRaw, config, nowSec) {
  var api = rugCallApi();
  if (api && typeof api.previewBuy === "function") {
    var sideNum = side === "survives" || side === 2 ? 2 : 1;
    try {
      var r = api.previewBuy(market, position, sideNum, amountRaw, config || null, nowSec);
      if (r && r.kind === "refused") {
        return { thisBuyReturnRaw: null, totalReturnRaw: null, errors: [reasonLabel(r.reason)] };
      }
      if (r && r.kind === "ok") {
        return {
          thisBuyReturnRaw: r.payoutOnThisBuyAlone != null ? String(r.payoutOnThisBuyAlone) : null,
          totalReturnRaw: r.projectedPositionPayout != null ? String(r.projectedPositionPayout) : null,
          errors: []
        };
      }
    } catch (err) { /* fall through to the local math */ }
  }
  return localPreviewBuy(market, position, side, amountRaw);
}

/* Map an evaluateEvidence result to five trigger states. A trigger
   whose underlying check never ran is "unknown", never "not-fired". */
function fallbackTriggerStates(evaluation) {
  if (!evaluation || !Array.isArray(evaluation.hardTriggers)) {
    return ["unknown", "unknown", "unknown", "unknown", "unknown"];
  }
  var missing = evaluation.missingCore || [];
  var blockers = evaluation.resolutionBlockers || [];
  var unknownById = {
    1: missing.indexOf("history") !== -1,
    2: missing.indexOf("bundle") !== -1 || missing.indexOf("funders") !== -1 || missing.indexOf("bundlePartial") !== -1,
    3: blockers.indexOf("sellsUnchecked") !== -1,
    4: blockers.indexOf("liquidityUnknown") !== -1,
    5: missing.indexOf("funders") !== -1
  };
  return evaluation.hardTriggers.map(function (t) {
    if (t.fired) return "fired";
    if (unknownById[t.id]) return "unknown";
    return "not-fired";
  });
}

function evidenceTriggerStates(evaluation, evidence) {
  var api = rugCallApi();
  if (api && typeof api.evidenceTriggerStates === "function") {
    try {
      var s = api.evidenceTriggerStates(evaluation, evidence);
      if (Array.isArray(s) && s.length === 5) return s;
    } catch (err) { /* fall through */ }
  }
  return fallbackTriggerStates(evaluation);
}

function buildPortfolio(positions, markets, nowSec) {
  var api = rugCallApi();
  if (api && typeof api.buildPortfolio === "function") {
    try {
      var p = api.buildPortfolio(markets, positions, nowSec);
      if (p) return p;
    } catch (err) { /* fall through */ }
  }
  var byAddress = {};
  (markets || []).forEach(function (m) { byAddress[m.address] = m; });
  var entries = (positions || []).map(function (pos) {
    var market = byAddress[pos.market || pos.marketAddress] || null;
    return { position: pos, market: market, claim: market ? fallbackClaimPreview(market, pos) : { kind: "none" } };
  });
  return { entries: entries, positions: entries };
}

/* ------------------------------------------------------------------ */
/* View-model helpers                                                  */
/* ------------------------------------------------------------------ */

function tokenMetaFor(market, tokenMetaByMint) {
  if (!market) return null;
  var meta = market.tokenMeta || market.dexMeta || market.meta || null;
  if (!meta && tokenMetaByMint && market.subjectMint) {
    meta = tokenMetaByMint[market.subjectMint] || null;
  }
  return meta || null;
}

function metaPriceText(meta) {
  if (!meta) return null;
  var p = meta.priceUsd != null ? meta.priceUsd : meta.price != null ? meta.price : null;
  if (p === null || typeof p === "undefined") return null;
  return "$" + String(p);
}

function metaLiquidityText(meta) {
  if (!meta) return null;
  var l = meta.liquidityUsd != null ? meta.liquidityUsd : meta.liquidity != null ? meta.liquidity : null;
  if (l === null || typeof l === "undefined") return null;
  if (typeof l === "object") l = l.usd;
  if (l === null || typeof l === "undefined") return null;
  return "$" + groupThousands(String(Math.round(Number(l))));
}

function formatTimeRemaining(expiry, nowSec) {
  var now = typeof nowSec === "number" ? nowSec : Math.floor(Date.now() / 1000);
  var exp = typeof expiry === "number" ? expiry : Number(expiry || 0);
  var diff = exp - now;
  if (diff <= 0) return "Expired";
  var h = Math.floor(diff / 3600);
  var m = Math.floor((diff % 3600) / 60);
  if (h >= 24) return Math.floor(h / 24) + "d " + (h % 24) + "h left";
  if (h > 0) return h + "h " + m + "m left";
  return m + "m left";
}

function formatTimestamp(sec) {
  if (!sec) return "n/a";
  var n = typeof sec === "number" ? sec : Number(sec);
  if (!n) return "n/a";
  return new Date(n * 1000).toISOString().replace("T", " ").replace(/\.\d+Z$/, " UTC");
}

function positionFor(positions, marketAddress, userAddress) {
  if (!Array.isArray(positions)) return null;
  for (var i = 0; i < positions.length; i++) {
    var p = positions[i];
    var pm = p.market || p.marketAddress;
    if (pm !== marketAddress) continue;
    if (userAddress && p.user && p.user !== userAddress) continue;
    if (userAddress && p.owner && p.owner !== userAddress) continue;
    return p;
  }
  return null;
}

function positionStakeText(position) {
  if (!position) return "";
  var f = position.flaggedAmount != null ? position.flaggedAmount : position.flagged_amount || "0";
  var s = position.survivesAmount != null ? position.survivesAmount : position.survives_amount || "0";
  var parts = [];
  if (String(f) !== "0") parts.push("Flagged " + formatDusdc(f));
  if (String(s) !== "0") parts.push("Survives " + formatDusdc(s));
  return parts.join(" / ");
}

/* ------------------------------------------------------------------ */
/* Pure string renderers                                               */
/* ------------------------------------------------------------------ */

function renderBanner(opts) {
  opts = opts || {};
  var parts = [];
  parts.push('<div class="rc-banner rc-banner-fixture" role="note">' + esc(fixtureBannerText()) + "</div>");
  if (opts.sourceKind === "fixture") {
    parts.push('<span class="rc-chip rc-chip-fixture">' + esc(fixtureDataLabel()) + "</span>");
  }
  if (opts.walletConnected) {
    var net = opts.network || "unknown";
    if (net !== "devnet" && net !== "devnet-fixture") {
      parts.push('<div class="rc-banner rc-banner-amber" role="alert">' + esc(NETWORK_BANNER_TEXT) + "</div>");
    } else {
      parts.push('<div class="rc-banner rc-banner-ok">Wallet network: Devnet</div>');
    }
  }
  return parts.join("\n");
}

function renderOddsBar(odds, market) {
  if (!odds || odds.flaggedBps === null || typeof odds.flaggedBps === "undefined") {
    return '<div class="rc-odds rc-odds-empty"><div class="rc-odds-track rc-odds-unsegmented"></div>' +
      '<div class="rc-odds-label">No stakes yet</div></div>';
  }
  var fPct = typeof odds.flaggedPct === "number" ? odds.flaggedPct : odds.flaggedBps / 100;
  var sPct = typeof odds.survivesPct === "number" ? odds.survivesPct : odds.survivesBps / 100;
  var html = '<div class="rc-odds"><div class="rc-odds-track">' +
    '<div class="rc-odds-flagged" style="width:' + fPct + '%"></div>' +
    '<div class="rc-odds-survives" style="width:' + sPct + '%"></div></div>' +
    '<div class="rc-odds-label">Flagged ' + fPct.toFixed(1) + "% / Survives " + sPct.toFixed(1) + "%</div>";
  if (odds.oneSided) {
    html += '<div class="rc-note">One-sided pool: if the staked side wins, exact stakes are returned - ' +
      "there is no opposing pool to win from.</div>";
  }
  html += "</div>";
  return html;
}

function renderVerdictChip(evaluation) {
  if (!evaluation) return '<span class="rc-chip">Evidence not loaded</span>';
  var v = evaluation.displayVerdict || "UNKNOWN";
  var cls = v === "FLAGGED" ? "rc-chip-flagged" : v === "CAUTION" ? "rc-chip-caution" : "rc-chip-clean";
  var html = '<span class="rc-chip ' + cls + '">' + esc(v) + "</span>";
  if (evaluation.partial) html += ' <span class="rc-chip rc-chip-partial">PARTIAL</span>';
  return html;
}

function renderMarketCard(vm) {
  var market = vm.market;
  var meta = vm.tokenMeta || null;
  var odds = vm.odds || computeOdds(market);
  var phase = vm.phase || marketPhase(market, vm.nowSec);
  var title;
  var priceLine;
  if (meta && (meta.symbol || meta.name)) {
    title = esc(meta.symbol || "") + (meta.name ? " - " + esc(meta.name) : "");
    var price = metaPriceText(meta);
    var liq = metaLiquidityText(meta);
    priceLine = (price ? "Price " + esc(price) : "Price unavailable") +
      " / " + (liq ? "Liquidity " + esc(liq) : "Liquidity unavailable");
  } else {
    title = shortMint(market.subjectMint);
    priceLine = "Price/liquidity unavailable";
  }
  var html = '<article class="rc-card" data-market="' + esc(market.address) + '">';
  html += '<header class="rc-card-head"><span class="rc-card-title">' + title + "</span> " +
    renderVerdictChip(vm.evaluation || null) + "</header>";
  html += '<div class="rc-card-meta">' + esc(priceLine) + "</div>";
  html += '<div class="rc-card-meta">Mint ' + esc(shortMint(market.subjectMint)) + " / " +
    esc(formatTimeRemaining(market.expiry, vm.nowSec)) + " / Phase " + esc(phase) + "</div>";
  html += '<div class="rc-card-pool">Pool total: ' + esc(formatDusdc(market.totalPool != null ? market.totalPool : odds.totalPool)) + "</div>";
  html += renderOddsBar(odds, market);
  if (vm.position) {
    var stake = positionStakeText(vm.position);
    if (stake) html += '<div class="rc-chip rc-chip-position">Your position: ' + esc(stake) + "</div>";
  }
  if (phase === "resolved") {
    html += '<div class="rc-card-outcome">Outcome: ' + esc(capName(market.outcomeName || "")) +
      (market.hardTrigger ? " via H" + esc(market.hardTrigger) : "") + "</div>";
  }
  if (phase === "cancelled") {
    html += '<div class="rc-card-outcome">Cancelled \u2014 stakes refundable in full</div>';
  }
  html += '<button type="button" class="rc-btn" data-action="select-market" data-market="' +
    esc(market.address) + '">View market</button>';
  html += "</article>";
  return html;
}

function groupMarkets(markets, nowSec) {
  var groups = { open: [], grace: [], pastGrace: [], settled: [] };
  (markets || []).forEach(function (m) {
    var phase = marketPhase(m, nowSec);
    if (phase === "open-live") groups.open.push(m);
    else if (phase === "open-expired-grace") groups.grace.push(m);
    else if (phase === "open-expired-past-grace") groups.pastGrace.push(m);
    else groups.settled.push(m);
  });
  return groups;
}

function renderMarketList(opts) {
  var groups = groupMarkets(opts.markets || [], opts.nowSec);
  function cards(list) {
    if (!list.length) return '<p class="rc-empty">None.</p>';
    return list.map(function (m) {
      return renderMarketCard({
        market: m,
        nowSec: opts.nowSec,
        tokenMeta: tokenMetaFor(m, opts.tokenMetaByMint),
        evaluation: opts.evaluationFor ? opts.evaluationFor(m) : null,
        position: positionFor(opts.positions, m.address, opts.userAddress)
      });
    }).join("\n");
  }
  var expired = groups.grace.concat(groups.pastGrace);
  var html = "";
  html += '<section class="rc-group"><h3>Open</h3>' + cards(groups.open) + "</section>";
  html += '<section class="rc-group"><h3>Expired \u2014 awaiting resolution</h3>';
  if (groups.grace.length) {
    html += '<h4>In the 24h grace window</h4><p class="rc-note">Resolution is deferred while evidence is retried. ' +
      "No claim is available yet.</p>" + cards(groups.grace);
  }
  if (groups.pastGrace.length) {
    html += '<h4>Past the grace window</h4><p class="rc-note">This market is cancellable by anyone; ' +
      "refunds follow a cancel. No claim is available until it is cancelled.</p>" + cards(groups.pastGrace);
  }
  if (!expired.length) html += '<p class="rc-empty">None.</p>';
  html += "</section>";
  html += '<section class="rc-group"><h3>Settled</h3>' + cards(groups.settled) + "</section>";
  return html;
}

function renderTerms() {
  return '<section class="rc-terms"><h4>Market terms</h4><p>' + esc(MARKET_TERMS_TEXT) + "</p>" +
    "<p>This market asks: will the evidence engine hard-flag this launch before expiry (24h after market creation)? " +
    "The FLAGGED side wins only if a hard trigger H1\u2013H5 fires with a trigger timestamp at or before expiry. " +
    "The SURVIVES side wins if no hard trigger has fired by expiry. If evidence cannot be obtained at expiry, " +
    "the market is cancelled and every stake is refunded.</p></section>";
}

var TRIGGER_LABELS = {
  1: "H1 - Creator history",
  2: "H2 - First-60-seconds bundle with coordination",
  3: "H3 - Creator/insider sell-through of 90% or more",
  4: "H4 - Liquidity gone",
  5: "H5 - Wallets linked to a previously flagged crew"
};

function renderEvidencePanel(opts) {
  var evaluation = opts.evaluation || null;
  var evidence = opts.evidence || null;
  if (!evaluation) {
    return '<section class="rc-evidence"><h4>Evidence</h4><p class="rc-note">No recorded evidence snapshot for this market.</p></section>';
  }
  var states = opts.triggerStates || evidenceTriggerStates(evaluation, evidence);
  var html = '<section class="rc-evidence"><h4>Evidence</h4>';
  html += "<div>Verdict: " + renderVerdictChip(evaluation) + "</div>";
  if (evaluation.verdictWhy) html += '<p class="rc-note">' + esc(evaluation.verdictWhy) + "</p>";
  if (evaluation.sentence) html += "<p>" + esc(evaluation.sentence) + "</p>";
  if (evaluation.partial) {
    var missing = (evaluation.missingCore || []).join(", ") || "none named";
    var blockers = (evaluation.resolutionBlockers || []).join(", ") || "none named";
    html += '<div class="rc-banner rc-banner-amber" role="alert">PARTIAL evidence - missing core: ' +
      esc(missing) + ". Resolution blockers: " + esc(blockers) + ".</div>";
  }
  if (typeof evaluation.softScore === "number") {
    html += '<div class="rc-score">Soft score: ' + esc(evaluation.softScore);
    if (evaluation.families) {
      html += " (bundle " + esc(evaluation.families.bundle || 0) +
        ", side wallets " + esc(evaluation.families.side || 0) +
        ", history " + esc(evaluation.families.history || 0) +
        ", structure " + esc(evaluation.families.structure || 0) + ")";
    }
    html += "</div>";
  }
  var softFlagOnly = evaluation.displayVerdict === "FLAGGED" && !evaluation.primaryHardTrigger;
  html += '<ul class="rc-triggers">';
  (evaluation.hardTriggers || []).forEach(function (t, i) {
    var state = states[i] || "unknown";
    var label = state === "fired" ? "FIRED" : state === "unknown" ? "UNKNOWN" : "not fired";
    html += "<li>" + esc(TRIGGER_LABELS[t.id] || ("H" + t.id)) + ': <strong class="rc-trigger-' +
      esc(state) + '">' + label + "</strong>";
    if (t.eventTimestamp) html += " (event " + esc(formatTimestamp(t.eventTimestamp)) + ")";
    html += "</li>";
  });
  html += "</ul>";
  if (softFlagOnly) {
    html += '<p class="rc-caption">Soft score only \u2014 this market resolves on hard triggers H1\u2013H5 only</p>';
  }
  html += "</section>";
  return html;
}

function renderClaimArea(opts) {
  var preview = opts.preview || { kind: "none" };
  var market = opts.market;
  var position = opts.position || null;
  var html = '<section class="rc-claim"><h4>My position and claim</h4>';
  if (position) {
    var stake = positionStakeText(position);
    html += "<p>" + (stake ? esc(stake) : "No stake recorded.") + "</p>";
    if (position.claimed) {
      html += '<p class="rc-claim-state">Already claimed.</p></section>';
      return html;
    }
  } else {
    html += "<p>No position on this market.</p>";
  }
  if (preview.kind === "payout") {
    html += '<p class="rc-claim-state">Claimable payout: ' + esc(formatDusdc(preview.amountRaw || "0")) + "</p>";
    html += '<button type="button" class="rc-btn rc-btn-primary" data-action="claim" data-market="' +
      esc(market.address) + '">Claim</button>';
  } else if (preview.kind === "refund") {
    html += '<p class="rc-claim-state">Refund available: ' + esc(formatDusdc(preview.amountRaw || "0")) +
      (preview.reason ? " (" + esc(reasonLabel(preview.reason)) + ")" : "") + "</p>";
    html += '<button type="button" class="rc-btn rc-btn-primary" data-action="claim" data-market="' +
      esc(market.address) + '">Claim refund</button>';
  } else if (preview.kind === "no-claim") {
    html += '<p class="rc-claim-state">No claim: this position holds no winning stake on the resolved side, ' +
      "so there is nothing to pay out.</p>";
  } else if (preview.kind === "error") {
    html += '<p class="rc-banner rc-banner-amber" role="alert">Claim state could not be determined' +
      (preview.reason ? ": " + esc(reasonLabel(preview.reason)) : "") +
      ". This is shown as a warning rather than a zero amount.</p>";
  } else {
    html += '<p class="rc-claim-state">Nothing claimable right now.</p>';
  }
  html += "</section>";
  return html;
}

function renderResultEnvelope(result, opts) {
  opts = opts || {};
  if (!result) return "";
  var fixture = !!opts.fixture || result.mode === "fixture";
  var html = '<div class="rc-result">';
  var refusalReasons = [];
  if (result.preflight && Array.isArray(result.preflight.errors)) {
    result.preflight.errors.forEach(function (e) {
      refusalReasons.push(e.message || e.code || "refused");
    });
  }
  if (result.message) refusalReasons.push(result.message);
  if (!refusalReasons.length && result.code) refusalReasons.push(result.code);
  if (result.status === "refused" || result.ok === false) {
    html += '<div class="rc-banner rc-banner-amber" role="alert">Refused: ' +
      refusalReasons.map(function (r) { return esc(r); }).join("; ") + "</div></div>";
    return html;
  }
  if (fixture || result.status === "simulated") {
    html += '<p class="rc-simulated">Simulated (fixture mode) \u2014 nothing was submitted. ' +
      "No transaction exists for this action.</p>";
  } else if (result.status === "submitted" && result.submitted) {
    html += "<p>Transaction sent.</p>";
    if (result.signature) html += "<p>Signature: " + esc(result.signature) + "</p>";
    if (result.explorerUrl) {
      html += '<p><a href="' + esc(result.explorerUrl) + '" rel="noopener">View on explorer</a></p>';
    }
  } else if (result.status === "refused") {
    html += "<p>Action refused - see the reasons above.</p>";
  }
  if (result.instruction) {
    html += "<p>Instruction: " + esc(result.instruction.name || "") +
      " (ordinal " + esc(result.instruction.ordinal) + ", " +
      esc(result.instruction.dataLen != null ? result.instruction.dataLen : "") + " bytes)</p>";
  }
  if (result.derived && typeof result.derived === "object") {
    var rows = Object.keys(result.derived).map(function (k) {
      return "<li>" + esc(k) + ": " + esc(result.derived[k]) + "</li>";
    }).join("");
    if (rows) html += "<ul>" + rows + "</ul>";
  }
  html += "</div>";
  return html;
}

function renderPositionForm(opts) {
  var market = opts.market;
  var side = opts.side || "flagged";
  var amountText = opts.amountText || "";
  var html = '<section class="rc-buy"><h4>Buy a position</h4>';
  html += '<div class="rc-side-toggle" role="group" aria-label="Side">' +
    '<button type="button" class="rc-btn' + (side === "flagged" ? " rc-btn-active" : "") +
    '" data-action="side" data-side="flagged">Flagged</button>' +
    '<button type="button" class="rc-btn' + (side === "survives" ? " rc-btn-active" : "") +
    '" data-action="side" data-side="survives">Survives</button></div>';
  html += '<label>Amount (dUSDC) <input type="text" inputmode="decimal" class="rc-amount" ' +
    'data-field="amount" value="' + esc(amountText) + '" placeholder="e.g. 100"></label>';
  html += '<div class="rc-preview" data-region="buy-preview">' +
    renderBuyPreview(opts.preview || null, side) + "</div>";
  if (opts.refusals && opts.refusals.length) {
    html += '<div class="rc-banner rc-banner-amber" role="alert">' +
      opts.refusals.map(function (r) { return esc(typeof r === "string" ? r : (r.message || r.code || "")); }).join("; ") +
      "</div>";
  }
  html += '<button type="button" class="rc-btn rc-btn-primary" data-action="buy" data-market="' +
    esc(market.address) + '">Buy</button>';
  html += '<p class="rc-note">Positions are hold-to-resolution. There is no exit before the market settles.</p>';
  html += "</section>";
  return html;
}

function renderBuyPreview(preview, side) {
  if (!preview) return '<p class="rc-note">Enter an amount to preview the return.</p>';
  if (preview.errors && preview.errors.length) {
    return '<p class="rc-note">' +
      preview.errors.map(function (e) { return esc(typeof e === "string" ? e : (e.message || e.code || "")); }).join("; ") +
      "</p>";
  }
  if (preview.thisBuyReturnRaw === null || typeof preview.thisBuyReturnRaw === "undefined") {
    return '<p class="rc-note">Preview unavailable for this amount.</p>';
  }
  var sideLabel = side === "survives" ? "Survives" : "Flagged";
  return "<p>This buy alone returns approx. " + esc(formatDusdc(preview.thisBuyReturnRaw)) +
    " if " + sideLabel + " wins.</p>" +
    "<p>Your total position would return approx. " + esc(formatDusdc(preview.totalReturnRaw)) +
    " if " + sideLabel + " wins.</p>";
}

function renderMarketDetail(opts) {
  var market = opts.market;
  var odds = opts.odds || computeOdds(market);
  var phase = marketPhase(market, opts.nowSec);
  var html = '<section class="rc-detail">';
  html += "<h3>Market detail</h3>";
  html += renderTerms();
  html += '<section class="rc-subject"><h4>Subject</h4><p>Mint: ' + esc(market.subjectMint) + "</p>" +
    "<p>Market: " + esc(market.address) + " / Creator: " + esc(shortMint(market.creator)) + "</p>" +
    "<p>Created " + esc(formatTimestamp(market.createdAt)) + " / Expires " +
    esc(formatTimestamp(market.expiry)) + " / " + esc(formatTimeRemaining(market.expiry, opts.nowSec)) + "</p></section>";
  html += renderEvidencePanel({ evaluation: opts.evaluation || null, evidence: opts.evidence || null });
  html += '<section class="rc-pools"><h4>Pools</h4>' + renderOddsBar(odds, market) +
    "<p>Flagged pool: " + esc(formatDusdc(market.flaggedPool)) +
    " / Survives pool: " + esc(formatDusdc(market.survivesPool)) +
    " / Total: " + esc(formatDusdc(market.totalPool != null ? market.totalPool : odds.totalPool)) + "</p></section>";
  var isOpen = phase === "open-live";
  if (isOpen) {
    html += renderPositionForm({
      market: market, side: opts.side, amountText: opts.amountText,
      preview: opts.preview || null, refusals: opts.refusals || []
    });
  } else if (phase === "open-expired-grace" || phase === "open-expired-past-grace") {
    html += '<p class="rc-note">Buying is closed. ' +
      (phase === "open-expired-past-grace"
        ? "This market is cancellable by anyone; refunds follow a cancel."
        : "Awaiting resolution inside the grace window.") + "</p>";
  }
  html += renderClaimArea({
    market: market, position: opts.position || null,
    preview: opts.claimPreview || computeClaimPreview(market, opts.position || null)
  });
  if (phase === "resolved") {
    html += '<section class="rc-resolution"><h4>Resolution record</h4>' +
      "<p>Outcome: " + esc(capName(market.outcomeName || "")) +
      " / Hard trigger: " + (market.hardTrigger ? "H" + esc(market.hardTrigger) : "none") +
      " / Resolved at: " + esc(formatTimestamp(market.resolvedAt)) + "</p>" +
      "<p>Evidence hash: " + esc(market.evidenceHash || "") + "</p>";
    if (opts.bundleUrl) {
      html += '<p><a href="' + esc(opts.bundleUrl) + '" rel="noopener">Evidence bundle</a></p>';
    } else {
      html += '<p class="rc-note">Evidence bundle hash recorded on the market; no published bundle link is configured.</p>';
    }
    html += "</section>";
  }
  if (opts.lastResult) {
    html += renderResultEnvelope(opts.lastResult, { fixture: opts.fixture });
  }
  html += "</section>";
  return html;
}

function renderPortfolio(portfolio, opts) {
  opts = opts || {};
  var entries = [];
  if (Array.isArray(portfolio)) entries = portfolio;
  else if (portfolio && Array.isArray(portfolio.entries)) entries = portfolio.entries;
  else if (portfolio && Array.isArray(portfolio.positions)) entries = portfolio.positions;
  if (!entries.length) {
    return '<section class="rc-portfolio"><h3>Portfolio</h3><p class="rc-empty">No positions yet.</p></section>';
  }
  var rows = entries.map(function (entry) {
    var pos = entry.position || entry;
    var market = entry.market || null;
    var claim = entry.claim || entry.claimPreview || (market ? computeClaimPreview(market, pos) : { kind: "none" });
    var addr = market ? market.address : (pos.market || pos.marketAddress || "");
    var claimText = claim.kind === "payout" ? "payout " + formatDusdc(claim.amountRaw || "0")
      : claim.kind === "refund" ? "refund " + formatDusdc(claim.amountRaw || "0")
      : claim.kind;
    var settled = market && (market.state === 2 || market.state === 3 ||
      market.stateName === "resolved" || market.stateName === "cancelled");
    return "<li>" + esc(shortMint(addr)) + " - " + esc(positionStakeText(pos) || "position") +
      " - " + (settled ? "settled (" + esc(capName(market.outcomeName || market.stateName)) + ") - " : "open - ") +
      esc(claimText) + "</li>";
  }).join("");
  return '<section class="rc-portfolio"><h3>Portfolio</h3><ul>' + rows + "</ul></section>";
}

function renderCreatePreview(evaluation) {
  if (!evaluation) return "";
  return '<div class="rc-create-preview"><h4>Evidence preview</h4>' +
    renderEvidencePanel({ evaluation: evaluation, evidence: null }) + "</div>";
}

/* ------------------------------------------------------------------ */
/* DOM app                                                             */
/* ------------------------------------------------------------------ */

/**
 * Mount the RugCall app into a root element.
 * deps: {
 *   dataSource, client, wallet, userAddress, config,
 *   network, classifyNetworkFn, crewStore,
 *   tokenMetaByMint, evidenceForMarket(market), evidenceForMint(mint),
 *   bundleUrlForMarket(market), nowSec
 * }
 */
function mountApp(root, deps) {
  if (!root || typeof root.innerHTML === "undefined") {
    throw new Error("mountApp requires a DOM element");
  }
  deps = deps || {};
  var source = deps.dataSource || null;
  var client = deps.client || null;
  var state = {
    markets: [],
    positions: [],
    selected: null,
    side: "flagged",
    amountText: "",
    lastResult: null,
    createMint: "",
    createResult: null,
    createEvaluation: null,
    network: deps.network || null,
    error: null
  };
  var userAddress = deps.userAddress ||
    (deps.wallet && deps.wallet.publicKey ? String(deps.wallet.publicKey) : null);

  function nowSec() {
    if (typeof deps.nowSec === "number") return deps.nowSec;
    if (source && typeof source.nowSec === "number") return source.nowSec;
    return Math.floor(Date.now() / 1000);
  }

  function evaluationFor(market) {
    try {
      var evApi = evidenceApi();
      if (!evApi) return null;
      var evidence = null;
      if (typeof deps.evidenceForMarket === "function") evidence = deps.evidenceForMarket(market);
      if (!evidence && market.evidence) evidence = market.evidence;
      if (!evidence && market.evidenceSnapshot) evidence = market.evidenceSnapshot;
      if (!evidence) return null;
      var store = deps.crewStore || (evApi.emptyCrewStore ? evApi.emptyCrewStore() : undefined);
      return evApi.evaluateEvidence(evidence, store);
    } catch (err) {
      return null;
    }
  }

  function evidenceForMint(mint) {
    if (typeof deps.evidenceForMint === "function") return deps.evidenceForMint(mint);
    return null;
  }

  function selectedMarket() {
    for (var i = 0; i < state.markets.length; i++) {
      if (state.markets[i].address === state.selected) return state.markets[i];
    }
    return state.markets[0] || null;
  }

  function currentPreview(market, position) {
    var raw = parseDusdcToRaw(state.amountText);
    if (raw === null || raw === "0") return null;
    return previewBuy(market, position, state.side, raw, deps.config || null, nowSec());
  }

  function cardEvaluationFor(market) {
    var full = evaluationFor(market);
    if (full) return full;
    if (typeof deps.summaryFor === "function") {
      var summary = deps.summaryFor(market);
      if (summary && summary.verdict) {
        return { displayVerdict: summary.verdict, partial: !!summary.partial, hardTriggers: [], primaryHardTrigger: 0 };
      }
    }
    return null;
  }

  function render() {
    var market = selectedMarket();
    var position = market ? positionFor(state.positions, market.address, userAddress) : null;
    var portfolio = buildPortfolio(state.positions, state.markets, nowSec());
    var parts = [];
    parts.push(renderBanner({
      sourceKind: source ? source.kind : null,
      walletConnected: !!userAddress,
      network: state.network
    }));
    if (state.error) {
      parts.push('<div class="rc-banner rc-banner-amber" role="alert">' + esc(state.error) + "</div>");
    }
    parts.push(renderPortfolio(portfolio, {}));
    parts.push('<section class="rc-create"><h3>Open a market</h3>' +
      '<label>Subject mint <input type="text" class="rc-mint" data-field="create-mint" value="' +
      esc(state.createMint) + '" placeholder="Paste a mainnet mint"></label>' +
      '<button type="button" class="rc-btn" data-action="create-preview">Preview evidence</button> ' +
      '<button type="button" class="rc-btn rc-btn-primary" data-action="create">Create market</button>' +
      (state.createEvaluation ? renderCreatePreview(state.createEvaluation) : "") +
      (state.createResult ? renderResultEnvelope(state.createResult, { fixture: source && source.kind === "fixture" }) : "") +
      "</section>");
    parts.push("<h3>Markets</h3>");
    parts.push(renderMarketList({
      markets: state.markets, positions: state.positions, userAddress: userAddress,
      nowSec: nowSec(), tokenMetaByMint: deps.tokenMetaByMint,
      evaluationFor: cardEvaluationFor
    }));
    if (market) {
      parts.push(renderMarketDetail({
        market: market,
        nowSec: nowSec(),
        evaluation: evaluationFor(market),
        evidence: typeof deps.evidenceForMarket === "function" ? deps.evidenceForMarket(market) : (market.evidence || null),
        position: position,
        claimPreview: computeClaimPreview(market, position),
        side: state.side,
        amountText: state.amountText,
        preview: currentPreview(market, position),
        fixture: !!(source && source.kind === "fixture"),
        lastResult: state.lastResult,
        bundleUrl: typeof deps.bundleUrlForMarket === "function" ? deps.bundleUrlForMarket(market) : null
      }));
    }
    root.innerHTML = parts.join("\n");
  }

  function refreshPreview() {
    var market = selectedMarket();
    if (!market) return;
    var node = root.querySelector('[data-region="buy-preview"]');
    if (!node) return;
    var position = positionFor(state.positions, market.address, userAddress);
    node.innerHTML = renderBuyPreview(currentPreview(market, position), state.side);
  }

  async function refresh() {
    if (!source) { render(); return; }
    try {
      state.markets = await source.listMarkets();
      if (userAddress && typeof source.listPositionsForUser === "function") {
        state.positions = await source.listPositionsForUser(userAddress);
      } else if (typeof source.listPositions === "function") {
        state.positions = await source.listPositions();
      }
      if (!state.selected && state.markets.length) state.selected = state.markets[0].address;
      state.error = null;
    } catch (err) {
      state.error = "Market data could not be loaded: " + (err && err.message ? err.message : String(err));
    }
    render();
  }

  async function classify() {
    try {
      var fn = deps.classifyNetworkFn || null;
      if (!fn && typeof globalThis !== "undefined" && globalThis.RugCallWallet &&
          typeof globalThis.RugCallWallet.classifyNetwork === "function") {
        fn = globalThis.RugCallWallet.classifyNetwork;
      }
      if (fn && deps.walletProvider) {
        state.network = await fn(deps.walletProvider, null);
      } else if (source && source.kind === "fixture") {
        state.network = state.network || "devnet-fixture";
      }
    } catch (err) {
      state.network = "unknown";
    }
  }

  root.addEventListener("click", function (ev) {
    var btn = ev.target && ev.target.closest ? ev.target.closest("[data-action]") : null;
    if (!btn) return;
    var action = btn.getAttribute("data-action");
    var marketAddress = btn.getAttribute("data-market");
    if (action === "select-market") {
      state.selected = marketAddress;
      state.lastResult = null;
      render();
    } else if (action === "side") {
      state.side = btn.getAttribute("data-side") || "flagged";
      render();
    } else if (action === "buy") {
      var market = selectedMarket();
      var raw = parseDusdcToRaw(state.amountText);
      if (!market || !client || raw === null) {
        state.error = raw === null ? "Enter a valid dUSDC amount." : "No predict client is configured.";
        render();
        return;
      }
      Promise.resolve(client.buyPosition(market.address, state.side, raw)).then(function (res) {
        state.lastResult = res;
        return refresh();
      }).then(function () {
        state.lastResult = state.lastResult;
        render();
      }).catch(function (err) {
        state.error = "Buy failed: " + (err && err.message ? err.message : String(err));
        render();
      });
    } else if (action === "claim") {
      if (!client || !marketAddress) return;
      Promise.resolve(client.claim(marketAddress)).then(function (res) {
        state.lastResult = res;
        return refresh();
      }).then(function () { render(); }).catch(function (err) {
        state.error = "Claim failed: " + (err && err.message ? err.message : String(err));
        render();
      });
    } else if (action === "create-preview") {
      var evApi = evidenceApi();
      var evidence = evidenceForMint(state.createMint.trim());
      state.createEvaluation = null;
      if (evApi && evidence) {
        try {
          state.createEvaluation = evApi.evaluateEvidence(
            evidence, deps.crewStore || (evApi.emptyCrewStore ? evApi.emptyCrewStore() : undefined));
        } catch (err) { state.createEvaluation = null; }
      }
      render();
    } else if (action === "create") {
      if (!client) { state.error = "No predict client is configured."; render(); return; }
      var mint = state.createMint.trim();
      Promise.resolve(client.createMarket(mint)).then(function (res) {
        state.createResult = res;
        return refresh();
      }).then(function () { render(); }).catch(function (err) {
        state.error = "Create failed: " + (err && err.message ? err.message : String(err));
        render();
      });
    }
  });

  root.addEventListener("input", function (ev) {
    var field = ev.target && ev.target.getAttribute ? ev.target.getAttribute("data-field") : null;
    if (field === "amount") {
      state.amountText = ev.target.value;
      refreshPreview();
    } else if (field === "create-mint") {
      state.createMint = ev.target.value;
    }
  });

  classify().then(refresh);
  return {
    refresh: refresh,
    render: render,
    getState: function () { return state; },
    selectMarket: function (address) { state.selected = address; render(); }
  };
}

/* ------------------------------------------------------------------ */
/* Exports (guarded: no DOM access at load time)                       */
/* ------------------------------------------------------------------ */

var RugCallUI = {
  MARKET_TERMS_TEXT: MARKET_TERMS_TEXT,
  NETWORK_BANNER_TEXT: NETWORK_BANNER_TEXT,
  FALLBACK_FIXTURE_BANNER: FALLBACK_FIXTURE_BANNER,
  FALLBACK_FIXTURE_DATA_LABEL: FALLBACK_FIXTURE_DATA_LABEL,
  esc: esc,
  capName: capName,
  reasonLabel: reasonLabel,
  shortMint: shortMint,
  formatDusdc: formatDusdc,
  parseDusdcToRaw: parseDusdcToRaw,
  fallbackFormatDusdc: fallbackFormatDusdc,
  fallbackParseDusdcToRaw: fallbackParseDusdcToRaw,
  marketPhase: marketPhase,
  fallbackMarketPhase: fallbackMarketPhase,
  computeOdds: computeOdds,
  fallbackComputeOdds: fallbackComputeOdds,
  computeClaimPreview: computeClaimPreview,
  fallbackClaimPreview: fallbackClaimPreview,
  previewBuy: previewBuy,
  evidenceTriggerStates: evidenceTriggerStates,
  fallbackTriggerStates: fallbackTriggerStates,
  buildPortfolio: buildPortfolio,
  groupMarkets: groupMarkets,
  formatTimeRemaining: formatTimeRemaining,
  formatTimestamp: formatTimestamp,
  renderBanner: renderBanner,
  renderOddsBar: renderOddsBar,
  renderVerdictChip: renderVerdictChip,
  renderMarketCard: renderMarketCard,
  renderMarketList: renderMarketList,
  renderTerms: renderTerms,
  renderEvidencePanel: renderEvidencePanel,
  renderClaimArea: renderClaimArea,
  renderResultEnvelope: renderResultEnvelope,
  renderBuyPreview: renderBuyPreview,
  renderPositionForm: renderPositionForm,
  renderMarketDetail: renderMarketDetail,
  renderPortfolio: renderPortfolio,
  renderCreatePreview: renderCreatePreview,
  mountApp: mountApp
};

if (typeof module !== "undefined" && module.exports) {
  module.exports = RugCallUI;
}
if (typeof window !== "undefined") {
  window.RugCallUI = RugCallUI;
}
