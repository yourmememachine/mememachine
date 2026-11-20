/*
 * RugCall evidence module.
 *
 * The verdict kernel in this file is copied verbatim from the Solana OS
 * export of 2026-11-19, with its session state made explicit as module
 * state declared above the kernel region. The fetchers are a port with
 * dependency injection and added per-trigger event-timestamp capture.
 * The bundle builder and hash are new. The OS does not consume this
 * module yet, so the two implementations are kept in agreement by the
 * diff-check and comparison tests in rugcall/resolver/test/ -- not by
 * shared execution.
 *
 * Conventions and choices:
 * - Every timestamp in an evidence object is in unix SECONDS
 *   (creationTime, fetchedAtSec, sells.crossingTs, history detail
 *   eventTs, trace "at"). The OS keeps milliseconds internally; the
 *   port converts at the fetch boundary. The kernel functions were
 *   checked line by line on this point: none of them reads
 *   creationTime at all, so the seconds convention cannot change a
 *   verdict -- creationTime is only used here, outside the kernel, to
 *   stamp per-trigger event times.
 * - deep.funders.curPct is not stored in evidence (it is a function).
 *   evaluateEvidence rebuilds it as a closure with the same formula
 *   the deep check used, reading the copied deep.ownerMap,
 *   deep._bought and light.total. Evidence additionally carries
 *   deep.pctByWallet, a materialized wallet -> pct map; when the raw
 *   fields behind the original formula are absent for a wallet, the
 *   closure falls back to that map.
 * - H4 (liquidity pulled) has no on-chain event time available to this
 *   module: its eventTimestamp is evidence.fetchedAtSec, the moment
 *   the state was observed. That is a detection bound, not the time
 *   the pool was drained.
 * - A history detail's eventTs is the earliest pairCreatedAt (in
 *   seconds) that the aggregator reports for that prior launch -- the
 *   timing data the history step already reads. When no pair carries
 *   a creation time, eventTs is null.
 * - sells.crossingTs is the blockTime (seconds) of the first sell
 *   transaction, walking the distinct fetched transactions in
 *   chronological order, at which the creator/cluster cumulative sold
 *   amount first reaches 90% of the launch position the sells figure
 *   is measured against. It is null when the position never crosses
 *   90% or when the crossing transaction carries no blockTime.
 */
"use strict";

var cryptoMod = null;
try {
  if (typeof require !== "undefined") cryptoMod = require("node:crypto");
} catch (err) {
  cryptoMod = null;
}

/* Session state the kernel closes over, with the same names and the
   same initial values the OS gives them. evaluateEvidence loads a
   crew store into these variables before it runs the kernel. */
let SESSION_FLAGGED_WALLETS = {};
let SESSION_FLAGGED_ORDER = [];
let SESSION_WALLET_APPEARANCES = {};
let LAUNCHES = {};
let LAUNCH_ORDER = [];
let CAPS = { launches:300, ledger:500, wireRender:100, binRender:100, h5Wallets:500, caches:400 };
let EVICTED = { launches:0, ledger:0, h5:0 };

// === BEGIN OS KERNEL (verbatim from the Solana OS export of 2026-11-19) ===
function fin(x){ return (typeof x === "number" && isFinite(x)) ? x : 0; } /* fix 14: NaN/undefined can never reach a % */
function f1s(x){ return (Math.round(fin(x) * 10) / 10).toFixed(1); }

function mergeHist(a, b){
  if (!a) return b; if (!b) return a;
  var byMint = {};
  a.details.forEach(function(d){ byMint[d.mint] = d; });
  b.details.forEach(function(d){ if (!(d.mint in byMint)) byMint[d.mint] = d; });
  var launches = Math.max(a.launches, Object.keys(byMint).length) + Math.max(0, b.launches - b.details.length);
  var inactive = 0, determined = 0, strikes = 0, fastExit = 0;
  Object.keys(byMint).forEach(function(m){
    var d = byMint[m];
    if (d.inactive !== null && typeof d.inactive !== "undefined"){
      determined++;
      if (d.inactive) inactive++;
      if (d.fastDead || d.exit) strikes++;
      if (d.fastDead && d.exit) fastExit++;
    }
  });
  return { launches:launches, inactive:inactive, determined:determined, strikes:strikes, fastExit:fastExit, details:[], ageDays:a.ageDays, ageIsFloor:a.ageIsFloor, sigCount:a.sigCount, unknown:a.unknown && b.unknown, sampled:a.sampled + b.sampled, merged:true };
}

function deepCheckedCount(){
  var n = 0;
  LAUNCH_ORDER.forEach(function(m){ var x = LAUNCHES[m]; if (x && (x.deepComplete || x.deep)) n++; });
  return Math.max(n, 1);
}
function evaluateH5(linkedHere, mint){
  var fires = [], totalDeep = deepCheckedCount();
  (linkedHere || []).forEach(function(row){
    var w = row.wallet;
    if (!w || w === mint) return;
    if (fin(row.pct) < 0.5) return; /* (d) dust in this coin */
    var app = SESSION_WALLET_APPEARANCES[w];
    var appCount = app ? Object.keys(app.coins).length : 0;
    if (appCount >= 4 && totalDeep >= 10 && (appCount / totalDeep) > 0.10) return; /* (c) ubiquity */
    var rec = SESSION_FLAGGED_WALLETS[w];
    if (!rec) return;
    var qualifying = 0;
    Object.keys(rec.coins).forEach(function(m){
      if (m === mint) return;
      var c = rec.coins[m];
      if (c && c.linked && fin(c.pct) >= 0.5) qualifying++;
    });
    if (qualifying >= 2) fires.push(w);
  });
  return fires;
}

function computeInsider(e, D){
  var total = e.light ? e.light.total : 0;
  if (!D.funders || !(total > 0)) return null;
  var creatorFunded = {}, fingerprint = {};
  if (D.creator && D.funders.links[D.creator]) D.funders.links[D.creator].forEach(function(w){ creatorFunded[w] = 1; });
  if (D.bundle && D.bundle.matching) (D.bundle.slotWallets || []).forEach(function(w){ fingerprint[w] = 1; });
  var provenRows = [], inferredRows = [], seen = {};
  function addRow(list, w, via){
    if (!w || seen[w]) return; seen[w] = 1;
    list.push({ wallet:w, pct:fin(D.funders.curPct(w)), via:via });
  }
  if (D.creator) addRow(provenRows, D.creator, "creator wallet");
  D.funders.linked.forEach(function(lw){
    if (creatorFunded[lw.wallet]) addRow(provenRows, lw.wallet, "funded directly by the creator (link shown)");
    else if (fingerprint[lw.wallet]) addRow(provenRows, lw.wallet, "launch-slot buy matching the bundle fingerprint");
  });
  if (D.bundle && D.bundle.matching) (D.bundle.slotWallets || []).forEach(function(w){ addRow(provenRows, w, "launch-slot buy matching the bundle fingerprint"); });
  D.funders.linked.forEach(function(lw){ addRow(inferredRows, lw.wallet, "shared funder link \u2014 an inference, not proof"); });
  var proven = provenRows.reduce(function(sm, r){ return sm + r.pct; }, 0);
  var inferred = inferredRows.reduce(function(sm, r){ return sm + r.pct; }, 0);
  var linkedSet = {}; D.funders.linked.forEach(function(lw){ linkedSet[lw.wallet] = 1; });
  var sniperPct = 0, sniperRows = [];
  if (D.bundle) D.bundle.wallets.forEach(function(bw){
    if (!linkedSet[bw.wallet] && !fingerprint[bw.wallet] && bw.wallet !== D.creator){ sniperPct += fin(bw.pct); sniperRows.push({ wallet:bw.wallet, pct:fin(bw.pct) }); }
  });
  var boughtUnion = {};
  if (D.creator && D._bought) boughtUnion[D.creator] = D._bought[D.creator] || 0;
  D.funders.linked.forEach(function(lw){ if (D._bought) boughtUnion[lw.wallet] = D._bought[lw.wallet] || 0; });
  var boughtAmt = 0; Object.keys(boughtUnion).forEach(function(w){ boughtAmt += boughtUnion[w]; });
  return { proven:fin(proven), inferred:fin(inferred), suspect:fin(proven + inferred),
    wallets:provenRows, inferredWallets:inferredRows, suspectWallets:[],
    sniperPct:fin(sniperPct), sniperWallets:sniperRows,
    unresolved:fin(D.funders.unresolvedPct || 0),
    boughtAtLaunch: fin(total > 0 ? (boughtAmt / total) * 100 : 0) };
}

function recordAppearances(e, D){
  var ws = {};
  if (D.bundle) D.bundle.wallets.forEach(function(bw){ ws[bw.wallet] = 1; });
  if (D.funders) D.funders.linked.forEach(function(lw){ ws[lw.wallet] = 1; });
  Object.keys(ws).forEach(function(w){
    var rec = SESSION_WALLET_APPEARANCES[w] || (SESSION_WALLET_APPEARANCES[w] = { coins:{} });
    rec.coins[e.mint] = 1;
  });
}

function recordFlaggedCrew(e, D){
  /* Store LINKED participation + position size per flagged coin (fix 1). */
  var pctByWallet = {};
  if (D.bundle) D.bundle.wallets.forEach(function(bw){ pctByWallet[bw.wallet] = fin(bw.pct); });
  var linkedSet = {};
  if (D.funders) D.funders.linked.forEach(function(lw){ linkedSet[lw.wallet] = 1; pctByWallet[lw.wallet] = fin(lw.pct); });
  var all = Object.keys(pctByWallet);
  if (D.creator){ all.push(D.creator); if (typeof pctByWallet[D.creator] === "undefined") pctByWallet[D.creator] = fin(D.creatorHoldingPct || 0); }
  all.forEach(function(w){
    if (!w) return;
    var rec = SESSION_FLAGGED_WALLETS[w];
    if (!rec){
      if (SESSION_FLAGGED_ORDER.length >= CAPS.h5Wallets){
        var old = SESSION_FLAGGED_ORDER.shift();
        if (old){ delete SESSION_FLAGGED_WALLETS[old]; EVICTED.h5++; } /* fix 3: capped, counted */
      }
      rec = SESSION_FLAGGED_WALLETS[w] = { coins:{} }; SESSION_FLAGGED_ORDER.push(w);
    }
    rec.coins[e.mint] = { linked: !!(linkedSet[w] || w === D.creator), pct: fin(pctByWallet[w] || 0) };
  });
}

function computeVerdict(e){
  var D = e.deep || {}, L = e.light || null;
  var hard = [];
  var hist = D.combinedHistory || D.history;
  /* Fix 2 (A2): H1's numerator is strikes only \u2014 launches that died
     within 24h OR show creator-exit evidence. Rate path: \u22653 strikes
     at \u226550% of sampled outcomes over \u22655 launches. Absolute path:
     \u22653 fast-dead launches WITH exit evidence fires regardless of
     rate (kills the zombie-dilution game). Slow fades never count. */
  if (hist && typeof hist.strikes === "number"){
    var ratePath = hist.launches >= 5 && hist.strikes >= 3 && hist.determined >= 3 && (hist.strikes / Math.max(1, hist.determined)) >= 0.5;
    var absPath = (hist.fastExit || 0) >= 3;
    if (ratePath || absPath) hard.push("H1");
  }
  /* Fixes 8+9 (A3/A4): H2 needs \u226540% of supply in the first-60-seconds
     window AND at least one coordination fact \u2014 \u22652 window wallets
     sharing a funder, the creator buying in the window, or \u22653
     matching bundle-fingerprint amounts. Unlinked same-slot supply
     alone (generic snipers on a hot launch) cannot hard-trigger. */
  if (D.bundle && D.bundle.pct >= 40){
    var coord = !!((D.funders && D.funders.sharedFunderInWindow) || D.bundle.creatorInWindow || D.bundle.matching);
    if (coord) hard.push("H2");
    D.bundle.coordination = coord;
  }
  if (D.sells && D.sells.checked && D.sells.soldPct >= 90) hard.push("H3");
  if (D.liquidity && D.liquidity.state === "gone") hard.push("H4");
  if (D.h5) hard.push("H5");
  var fam = { bundle:0, side:0, history:0, structure:0 };
  if (D.bundle){
    var bp = D.bundle.pct;
    fam.bundle = bp >= 30 ? 24 : bp >= 20 ? 16 : bp >= 10 ? 8 : 0;
    if (D.bundle.count >= 8) fam.bundle += 6;
    fam.bundle = Math.min(30, fam.bundle);
  }
  if (D.funders){
    /* Medium fix (A9): disjoint evidence \u2014 only linked wallets
       OUTSIDE the launch-window buyer set score here. */
    var n = (D.funders.linkedOutside || D.funders.linked).length;
    fam.side = n >= 11 ? 22 : n >= 6 ? 16 : n >= 3 ? 8 : 0;
    fam.side = Math.min(25, fam.side);
  }
  if (hist){
    var hp = 0;
    if (hist.determined > 0 && typeof hist.strikes === "number"){
      var rate = hist.strikes / hist.determined;
      if (hist.launches >= 5 && rate >= 0.2 && rate < 0.5) hp = 12;
      else if (hist.launches >= 3 && hist.launches <= 4 && hist.strikes >= 1) hp = 8;
      else if (hist.strikes >= 1 && hist.strikes <= 2) hp = 10;
    }
    if (typeof hist.ageDays === "number" && !hist.ageIsFloor){ if (hist.ageDays < 7) hp += 6; else if (hist.ageDays < 30) hp += 3; }
    fam.history = Math.min(30, hp);
  }
  if (L){
    var sp = 0;
    if (L.top10Excl >= 60) sp = 12; else if (L.top10Excl >= 40) sp = 6;
    fam.structure = Math.min(15, sp);
  }
  var totalScore = fam.bundle + fam.side + fam.history + fam.structure;
  var famsHit = [fam.bundle, fam.side, fam.history, fam.structure].filter(function(x){ return x > 0; }).length;
  var coreUnknown = !D.bundle || !D.funders || !D.history || !e.insider || (D.bundle && D.bundle.partial);
  /* Fix 10 (A5): unresolved share \u2014 early buyers whose linkage is
     unknowable holding \u226510% of supply caps the verdict at CAUTION.
     Partial unknown is now quantitative, not just narrative. */
  var unresolvedCap = !!(e.insider && fin(e.insider.unresolved) >= 10);
  e.score = { fam:fam, total:totalScore, famsHit:famsHit };
  e.hard = hard;
  var verdict, why;
  if (hard.length){ verdict = "FLAGGED"; why = "hard trigger " + hard[0]; }
  else if (totalScore >= 45 && famsHit >= 2){ verdict = "FLAGGED"; why = "combined score " + totalScore + " across " + famsHit + " signal families"; }
  else if (coreUnknown){ verdict = "CAUTION"; why = "not fully checked \u2014 a core check (bundle pattern, funder links, creator history, or the insider-supply number) is missing or PARTIAL, so this coin is capped at CAUTION"; }
  else if (totalScore >= 20){ verdict = "CAUTION"; why = "combined score " + totalScore; }
  else if (e.insider && e.insider.proven >= 10){ verdict = "CAUTION"; why = "insider supply " + f1s(e.insider.proven) + "% proven (creator + directly linked wallets)"; }
  else if (unresolvedCap){ verdict = "CAUTION"; why = "linkage unknown for wallets holding " + f1s(e.insider.unresolved) + "% of supply \u2014 their funder links could not be read, so this coin is capped at CAUTION"; }
  else if (hist && hist.unknown){ verdict = "CAUTION"; why = "creator history unknown \u2014 a fresh wallet has no track record to check, and no track record is not a good record"; }
  else { verdict = "NO RED FLAGS FOUND"; why = "all core checks ran and none tripped (score " + totalScore + ")"; }
  e.verdict = verdict; e.verdictWhy = why;
  e.sentence = alertSentenceFor(e, hard, fam);
  return verdict;
}

function alertSentenceFor(e, hard, fam){
  var D = e.deep || {}, hist = D.combinedHistory || D.history;
  var pct = function(x){ return (Math.round(x * 10) / 10).toFixed(1) + "%"; };
  if (hard.indexOf("H1") !== -1 && hist)
    return "The wallet that made this coin has launched at least " + hist.launches + " coins before; " + (hist.strikes || 0) + " of the " + hist.determined + " sampled outcomes died within a day or show the creator selling out" + ((hist.fastExit || 0) > 0 ? " (" + hist.fastExit + " of them fast-dead with exit evidence)" : "") + " \u2014 history sampled: " + hist.determined + " prior launch outcomes read from a sample of " + (hist.sampled || 0) + " recent transactions, a floor, not a census.";
  if (hard.indexOf("H2") !== -1 && D.bundle)
    return "One coordinated group bought " + pct(D.bundle.pct) + " of this coin in its first 60 seconds (coordination evidence: " + ((D.funders && D.funders.sharedFunderInWindow) ? "shared funder among the early buyers" : D.bundle.matching ? "matching buy sizes in the launch block" : "the creator buying in the launch window") + "), so they can crash the price whenever they choose.";
  if (hard.indexOf("H3") !== -1 && D.sells)
    return "The creator and their linked wallets have already sold " + pct(D.sells.soldPct) + " of their launch position — they got out before you got in.";
  if (hard.indexOf("H4") !== -1)
    return "The money pool that lets people sell this coin has been drained while wallets still hold it — getting out may no longer be possible.";
  if (hard.indexOf("H5") !== -1)
    return "This creator's wallet may be new, but " + (D.h5Wallets ? D.h5Wallets.length : 1) + " wallet(s) funding this launch are linked by shared funders to at least two earlier coins flagged in this session, holding \u22650.5% in each \u2014 the crew returned, not just the wallet.";
  if (e.verdict === "FLAGGED"){
    var parts = [];
    if (fam.bundle > 0 && D.bundle) parts.push(D.bundle.count + " wallets bought " + pct(D.bundle.pct) + " of this coin in its first 60 seconds");
    if (fam.side > 0 && D.funders) parts.push((D.funders.linkedOutside || D.funders.linked).length + " more wallets outside the launch window are linked by a shared funding source (an inference from funding, link shown in the panel)");
    if (fam.history > 0 && hist && (hist.strikes || 0) > 0) parts.push("the creator's sampled history shows " + hist.strikes + " earlier coin(s) that died within a day or show creator-exit evidence (of " + hist.determined + " sampled)");
    if (!parts.length && e.insider) parts.push("insider supply is " + pct(e.insider.proven) + " proven" + (e.insider.inferred > 0.05 ? " and up to " + pct(e.insider.suspect) + " including inferred shared-funder links" : "") + " in this coin");
    return (parts.length ? parts.join(", and ") : "Several checks tripped at once") + " — most of this coin sits with one connected group that can crash it at will.";
  }
  if (e.verdict === "CAUTION")
    return "Caution on this one: " + e.verdictWhy + ". Read the evidence panel before anyone buys.";
  return "Nothing stood out in the checks that ran" + (e.insider ? ": insider supply " + pct(e.insider.proven) + " proven" + (e.insider.inferred > 0.05 ? ", up to " + pct(e.insider.suspect) + " including inferred shared-funder links" : "") : "") + ". That is the best verdict I give \u2014 it is not a promise about this coin.";
}

// === END OS KERNEL ===

/* ================= helpers ================= */

function cloneData(v){
  if (v === null || typeof v !== "object"){
    return (typeof v === "function") ? undefined : v;
  }
  if (Array.isArray(v)){
    var a = [];
    for (var i = 0; i < v.length; i++) a.push(cloneData(v[i]));
    return a;
  }
  var o = {};
  Object.keys(v).forEach(function(k){
    var cv = cloneData(v[k]);
    if (typeof cv !== "undefined") o[k] = cv;
  });
  return o;
}

function errMessage(err){
  if (!err) return "unknown error";
  return (err && err.message) ? String(err.message) : String(err);
}

function defaultDeep(){
  return { creator:null, creationSlot:0, creationTime:0, bundle:null, bundleError:"",
    funders:null, fundersError:"", history:null, funderHistory:null, combinedHistory:null, historyError:"",
    creatorHoldingPct:null, sells:null, liquidity:null, auth:null, h5:false, h5Wallets:[],
    slot:null, ownerMap:null, pctByWallet:{} };
}

/* Raw token amounts can arrive as decimal strings in hand-built
   evidence. The kernel's arithmetic expects the numbers the OS built
   from them, so the copied fields are normalized here, on the copy
   only -- the caller's evidence object is never touched. */
function normalizeRawAmounts(D, light){
  if (light && typeof light.total === "string"){
    var n = Number(light.total);
    if (isFinite(n)) light.total = n;
  }
  if (D && D.ownerMap && typeof D.ownerMap === "object"){
    Object.keys(D.ownerMap).forEach(function(w){
      if (typeof D.ownerMap[w] === "string"){
        var m = Number(D.ownerMap[w]);
        if (isFinite(m)) D.ownerMap[w] = m;
      }
    });
  }
  if (D && D._bought && typeof D._bought === "object"){
    Object.keys(D._bought).forEach(function(w){
      if (typeof D._bought[w] === "string"){
        var b = Number(D._bought[w]);
        if (isFinite(b)) D._bought[w] = b;
      }
    });
  }
}

/* ================= public wrapper ================= */

function emptyCrewStore(){
  return { flagged:{}, appearances:{}, order:[], countedMints:[], totalDeepChecked:0 };
}

function historyDetailsFor(D){
  var hist = D.combinedHistory || D.history;
  if (hist && Array.isArray(hist.details) && hist.details.length) return hist.details;
  var out = [], seen = {};
  [D.history, D.funderHistory].forEach(function(h){
    if (h && Array.isArray(h.details)) h.details.forEach(function(d){
      if (d && !(d.mint in seen)){ seen[d.mint] = 1; out.push(d); }
    });
  });
  return out;
}

function evaluateEvidence(evidence, crewStoreIn){
  if (!evidence || typeof evidence !== "object") throw new Error("evidence object is required");
  var storeIn = crewStoreIn || emptyCrewStore();

  /* Load the crew store into the kernel's session state (copies --
     the caller's store is never mutated). */
  SESSION_FLAGGED_WALLETS = cloneData(storeIn.flagged || {}) || {};
  SESSION_FLAGGED_ORDER = Array.isArray(storeIn.order) ? storeIn.order.slice() : Object.keys(SESSION_FLAGGED_WALLETS);
  Object.keys(SESSION_FLAGGED_WALLETS).forEach(function(w){
    if (SESSION_FLAGGED_ORDER.indexOf(w) === -1) SESSION_FLAGGED_ORDER.push(w);
  });
  SESSION_WALLET_APPEARANCES = cloneData(storeIn.appearances || {}) || {};
  CAPS = { launches:300, ledger:500, wireRender:100, binRender:100, h5Wallets:500, caches:400 };
  EVICTED = { launches:0, ledger:0, h5:0 };
  var counted = Array.isArray(storeIn.countedMints) ? storeIn.countedMints.slice() : [];
  LAUNCHES = {}; LAUNCH_ORDER = [];
  counted.forEach(function(m){
    LAUNCHES[m] = { mint:m, deep:{}, deepComplete:true };
    LAUNCH_ORDER.push(m);
  });
  /* A store rebuilt from a bundle snapshot knows how many mints were
     deep-checked but not their names; placeholder entries keep the
     H5 denominator at the declared total. They never surface in the
     countedMints list returned below. */
  var declaredTotal = (typeof storeIn.totalDeepChecked === "number" && isFinite(storeIn.totalDeepChecked))
    ? Math.floor(storeIn.totalDeepChecked) : counted.length;
  for (var pi = counted.length; pi < declaredTotal; pi++){
    var pk = "__priorDeepChecked_" + pi;
    LAUNCHES[pk] = { mint:pk, deep:{}, deepComplete:true };
    LAUNCH_ORDER.push(pk);
  }

  var ev = cloneData(evidence) || {};
  var mint = ev.mint;
  var D = (ev.deep && typeof ev.deep === "object") ? ev.deep : defaultDeep();
  if (typeof D.h5 === "undefined") D.h5 = false;
  if (!Array.isArray(D.h5Wallets)) D.h5Wallets = [];
  if (typeof D.bundle === "undefined") D.bundle = null;
  if (typeof D.funders === "undefined") D.funders = null;
  if (typeof D.history === "undefined") D.history = null;
  if (typeof D.funderHistory === "undefined") D.funderHistory = null;
  if (typeof D.sells === "undefined") D.sells = null;
  if (typeof D.liquidity === "undefined") D.liquidity = null;
  if (typeof D.creator === "undefined") D.creator = null;
  if (typeof D.creatorHoldingPct === "undefined") D.creatorHoldingPct = null;
  if (D.combinedHistory === undefined){
    try { D.combinedHistory = mergeHist(D.history || null, D.funderHistory || null); }
    catch (merr) { D.combinedHistory = D.history || D.funderHistory || null; }
  }
  var light = (ev.light && typeof ev.light === "object") ? ev.light : null;
  normalizeRawAmounts(D, light);

  /* Rebuild funders.curPct with the deep check's own formula, over
     the copied data fields, with the pctByWallet fallback. */
  if (D.funders){
    var totalForPct = light ? light.total : 0;
    var pctMap = (D.pctByWallet && typeof D.pctByWallet === "object") ? D.pctByWallet : null;
    D.funders.curPct = function(w){
      if (D.ownerMap && typeof D.ownerMap[w] === "number" && totalForPct > 0) return fin((fin(D.ownerMap[w]) / totalForPct) * 100);
      if (totalForPct > 0 && D._bought && D._bought[w]) return fin((fin(D._bought[w]) / totalForPct) * 100);
      if (pctMap && typeof pctMap[w] === "number") return fin(pctMap[w]);
      if (pctMap && typeof pctMap[w] === "string" && isFinite(Number(pctMap[w]))) return fin(Number(pctMap[w]));
      if (Array.isArray(D.funders.linked)){
        for (var li = 0; li < D.funders.linked.length; li++){
          if (D.funders.linked[li] && D.funders.linked[li].wallet === w) return fin(D.funders.linked[li].pct);
        }
      }
      if (D.bundle && Array.isArray(D.bundle.wallets)){
        for (var bi = 0; bi < D.bundle.wallets.length; bi++){
          if (D.bundle.wallets[bi] && D.bundle.wallets[bi].wallet === w) return fin(D.bundle.wallets[bi].pct);
        }
      }
      return 0;
    };
  }

  var e = { mint:mint, light:light, deep:D, insider:null,
    verdict:null, verdictWhy:"", sentence:"", score:null, hard:[],
    clusterKey:null, deepComplete:false, partialFinal:false, status:"", statusReason:"" };

  /* The scratch entry joins LAUNCHES before the H5 step, exactly as
     the OS entry already exists there with its deep data set, so the
     H5 denominator counts this mint. */
  if (!LAUNCHES[mint]) LAUNCH_ORDER.push(mint);
  LAUNCHES[mint] = e;
  if (counted.indexOf(mint) === -1) counted.push(mint);

  /* Step errors, in the shape the deep check's finish step received:
     a history or funders step that reported an error counts even when
     some data still came back. */
  var stepErr = {};
  if (D.stepErr && typeof D.stepErr === "object"){
    if (D.stepErr.history) stepErr.history = D.stepErr.history;
    if (D.stepErr.funders) stepErr.funders = D.stepErr.funders;
  }
  if (ev.fetchErrors && typeof ev.fetchErrors === "object"){
    if (ev.fetchErrors.history) stepErr.history = ev.fetchErrors.history;
    if (ev.fetchErrors.funders) stepErr.funders = ev.fetchErrors.funders;
  }
  if (D.historyError) stepErr.history = D.historyError;
  if (D.fundersError) stepErr.funders = D.fundersError;

  /* finishDeep's ordering, minus its display and ledger calls. */
  recordAppearances(e, D);
  if (D.funders){
    var h5hits = evaluateH5(D.funders.linked, e.mint);
    if (h5hits.length){ D.h5 = true; D.h5Wallets = h5hits; }
  }
  e.clusterKey = (D.funders && D.funders.links && Object.keys(D.funders.links).length)
    ? Object.keys(D.funders.links)[0] : (D.creator || e.mint);
  e.insider = computeInsider(e, D);
  var bundlePartial = !!(D.bundle && D.bundle.partial);
  var hadErr = !!(stepErr.history || stepErr.funders) || !D.bundle || !D.funders || !D.history || bundlePartial;
  computeVerdict(e);
  e.deepComplete = !hadErr;
  e.status = hadErr ? "PARTIAL" : "DEEP CHECKED";
  if (e.verdict === "FLAGGED") recordFlaggedCrew(e, D);

  /* PARTIAL, two ways the OS derives it: the finish step's hadErr,
     and the verdict step's core-unknown condition (a local there, so
     it is recomputed here from the same inputs). */
  var coreUnknown = !D.bundle || !D.funders || !D.history || !e.insider || !!(D.bundle && D.bundle.partial);
  var partial = hadErr || coreUnknown;
  var missingCore = [];
  if (!D.bundle) missingCore.push("bundle");
  if (!D.funders) missingCore.push("funders");
  if (!D.history) missingCore.push("history");
  if (!e.insider) missingCore.push("insider");
  if (D.bundle && D.bundle.partial) missingCore.push("bundlePartial");

  var hard = Array.isArray(e.hard) ? e.hard : [];
  function firedOf(name){ return hard.indexOf(name) !== -1; }
  var hist = D.combinedHistory || D.history;

  /* H1 event time: the latest eventTs among the strike details that
     qualify under the path that fired. The fired flag itself is read
     off the kernel result above; the path split is needed only to
     choose which details count toward the timestamp. */
  var h1Ts = null;
  if (firedOf("H1") && hist){
    var rateQ = hist.launches >= 5 && hist.strikes >= 3 && hist.determined >= 3 &&
      (hist.strikes / Math.max(1, hist.determined)) >= 0.5;
    var absQ = (hist.fastExit || 0) >= 3;
    historyDetailsFor(D).forEach(function(d){
      var strike = !!(d.fastDead || d.exit);
      var both = !!(d.fastDead && d.exit);
      var counts = (rateQ && absQ) ? strike : absQ ? both : strike;
      if (counts && typeof d.eventTs === "number" && isFinite(d.eventTs)){
        h1Ts = (h1Ts === null) ? d.eventTs : Math.max(h1Ts, d.eventTs);
      }
    });
  }

  var fetchedAtSec = (typeof ev.fetchedAtSec === "number" && isFinite(ev.fetchedAtSec)) ? ev.fetchedAtSec : null;
  var creationSec = (typeof D.creationTime === "number" && isFinite(D.creationTime) && D.creationTime > 0) ? D.creationTime : null;

  var hardTriggers = [
    { id:1, name:"H1", fired:firedOf("H1"),
      eventTimestamp: firedOf("H1") ? h1Ts : null,
      values:{ launches: hist ? (typeof hist.launches === "number" ? hist.launches : null) : null,
        strikes: hist ? (typeof hist.strikes === "number" ? hist.strikes : null) : null,
        determined: hist ? (typeof hist.determined === "number" ? hist.determined : null) : null,
        fastExit: hist ? (typeof hist.fastExit === "number" ? hist.fastExit : null) : null } },
    { id:2, name:"H2", fired:firedOf("H2"),
      eventTimestamp: firedOf("H2") && creationSec !== null ? creationSec + 60 : null,
      values:{ pct: D.bundle ? (typeof D.bundle.pct === "number" ? D.bundle.pct : null) : null,
        sharedFunderInWindow: D.funders ? !!D.funders.sharedFunderInWindow : null,
        creatorInWindow: D.bundle ? !!D.bundle.creatorInWindow : null,
        matching: D.bundle ? !!D.bundle.matching : null } },
    { id:3, name:"H3", fired:firedOf("H3"),
      eventTimestamp: firedOf("H3") && D.sells && typeof D.sells.crossingTs === "number" ? D.sells.crossingTs : null,
      values:{ soldPct: D.sells ? (typeof D.sells.soldPct === "number" ? D.sells.soldPct : null) : null,
        checked: D.sells ? !!D.sells.checked : false } },
    { id:4, name:"H4", fired:firedOf("H4"),
      eventTimestamp: firedOf("H4") ? fetchedAtSec : null,
      values:{ state: D.liquidity ? (typeof D.liquidity.state === "string" ? D.liquidity.state : null) : null,
        usd: D.liquidity ? (typeof D.liquidity.usd === "number" ? D.liquidity.usd : null) : null } },
    { id:5, name:"H5", fired: D.h5 === true,
      eventTimestamp: (D.h5 === true) ? creationSec : null,
      values:{ wallets: Array.isArray(D.h5Wallets) ? D.h5Wallets.slice() : [] } }
  ];
  var primaryHardTrigger = 0;
  for (var ti = 0; ti < hardTriggers.length; ti++){
    if (hardTriggers[ti].fired){ primaryHardTrigger = hardTriggers[ti].id; break; }
  }

  var resolutionBlockers = [];
  if (!D.sells || !D.sells.checked) resolutionBlockers.push("sellsUnchecked");
  if (!D.liquidity || D.liquidity.state === "unknown") resolutionBlockers.push("liquidityUnknown");

  /* A trigger's fired value is definite only when the data it reads
     was actually checked. */
  var definite = [
    !!(hist && typeof hist.strikes === "number"),
    !!(D.bundle && D.funders),
    !!(D.sells && D.sells.checked),
    !!(D.liquidity && typeof D.liquidity.state === "string" && D.liquidity.state !== "unknown"),
    !!D.funders
  ];
  var complete = !partial && resolutionBlockers.length === 0 && hardTriggers.every(function(t, i){
    return definite[i] && (!t.fired || t.eventTimestamp !== null);
  });

  var totalDeep = 0;
  LAUNCH_ORDER.forEach(function(m){
    var x = LAUNCHES[m];
    if (x && (x.deepComplete || x.deep)) totalDeep++;
  });

  return {
    displayVerdict: e.verdict,
    verdictWhy: e.verdictWhy,
    sentence: e.sentence,
    softScore: e.score ? e.score.total : null,
    families: e.score ? cloneData(e.score.fam) : null,
    familiesHit: e.score ? e.score.famsHit : null,
    hardTriggers: hardTriggers,
    primaryHardTrigger: primaryHardTrigger,
    partial: partial,
    missingCore: missingCore,
    resolutionBlockers: resolutionBlockers,
    complete: complete,
    crewStore: {
      flagged: cloneData(SESSION_FLAGGED_WALLETS) || {},
      appearances: cloneData(SESSION_WALLET_APPEARANCES) || {},
      order: SESSION_FLAGGED_ORDER.slice(),
      countedMints: counted.slice(),
      totalDeepChecked: totalDeep
    }
  };
}

/* ================= fetchers (port, dependency-injected) =================
   Transport, caches and the clock come from deps:
     deps.rpc(url, method, params) -> Promise of the parsed reply
     deps.fetchJson(url) -> Promise of the parsed body
     deps.nowMs() -> milliseconds now
     deps.rpcUrl (optional)
   Sampling, caps and exclusions follow the original steps. */

var PUMP_PROGRAM = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
var KNOWN_NON_LAUNCH_MINTS = { "So11111111111111111111111111111111111111112":1, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v":1, "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB":1 };
var DEFAULT_RPC_URL = "https://api.mainnet-beta.solana.com";
var DEX_TOKENS_URL = "https://api.dexscreener.com/latest/dex/tokens/";

function sleepPort(ms){ return new Promise(function(r){ setTimeout(r, ms); }); }

function hashTextHex(text){
  if (!cryptoMod) return null;
  return cryptoMod.createHash("sha256").update(String(text), "utf8").digest("hex");
}

function makeFetchCtx(deps){
  deps = deps || {};
  return {
    deps: deps,
    rpcUrl: (typeof deps.rpcUrl === "string" && deps.rpcUrl) ? deps.rpcUrl : DEFAULT_RPC_URL,
    nowMs: (typeof deps.nowMs === "function") ? deps.nowMs : function(){ return Date.now(); },
    creatorCache: {},
    funderCache: {},
    funderActivityCache: {},
    budget: { deepActive:true, deepCalls:0, deepCap:60 },
    rpcResponseHashes: {},
    dexHash: null
  };
}

function rpcPort(ctx, method, params){
  if (typeof ctx.deps.rpc !== "function"){
    return Promise.reject(new Error("no rpc transport supplied"));
  }
  if (ctx.budget.deepActive) ctx.budget.deepCalls++;
  return Promise.resolve().then(function(){
    return ctx.deps.rpc(ctx.rpcUrl, method, params || []);
  }).then(function(j){
    try { ctx.rpcResponseHashes[method] = hashTextHex(JSON.stringify(j === undefined ? null : j)); } catch (herr){}
    if (j && j.error){
      var msg = (j.error && j.error.message) || "RPC error";
      var e = new Error(msg);
      e.rpcCode = j.error && j.error.code;
      if (e.rpcCode === -32429 || /429|rate|too many/i.test(msg)) e.isRate = true;
      throw e;
    }
    return j;
  });
}

function rateNotePort(err){
  if (err && err.isBudget) return "the per-cycle RPC call budget ran out";
  return err && err.isRate ? "public RPC limit (429)" : "the public RPC did not answer";
}

function txFeePayerPort(tx){
  try {
    var keys = tx.transaction.message.accountKeys || [];
    var k = keys[0];
    return typeof k === "string" ? k : (k && k.pubkey) || null;
  } catch (err){ return null; }
}
function txHasProgramPort(tx, prog){
  try {
    return (tx.transaction.message.accountKeys || []).some(function(k){
      return (typeof k === "string" ? k : k.pubkey) === prog;
    });
  } catch (err){ return false; }
}
function txTokenDeltasPort(tx, mint){
  var out = {};
  if (!tx || !tx.meta) return out;
  var pre = {}, post = tx.meta.postTokenBalances || [];
  (tx.meta.preTokenBalances || []).forEach(function(b){ if (b.mint === mint) pre[b.accountIndex] = fin(Number(b.uiTokenAmount && b.uiTokenAmount.amount)); });
  post.forEach(function(b){
    if (b.mint !== mint || !b.owner) return;
    var d = fin(Number(b.uiTokenAmount && b.uiTokenAmount.amount)) - fin(pre[b.accountIndex] || 0);
    if (d !== 0 && isFinite(d)) out[b.owner] = (out[b.owner] || 0) + d;
  });
  return out;
}
function txFirstSolFunderPort(tx, wallet){
  var found = null;
  function scan(list){
    (list || []).forEach(function(ix){
      if (found) return;
      if (ix && ix.program === "system" && ix.parsed && ix.parsed.type === "transfer" &&
          ix.parsed.info && ix.parsed.info.destination === wallet && ix.parsed.info.source){
        found = ix.parsed.info.source;
      }
    });
  }
  try {
    scan(tx.transaction.message.instructions);
    (tx.meta.innerInstructions || []).forEach(function(g){ scan(g.instructions); });
  } catch (err){}
  return found;
}
function fetchTxsPacedPort(ctx, sigs, cap){
  var out = [], chain = Promise.resolve();
  sigs.slice(0, cap).forEach(function(sg, i){
    chain = chain.then(function(){
      if (i > 0 && i % 4 === 0) return sleepPort(150);
    }).then(function(){
      return rpcPort(ctx, "getTransaction", [sg.signature, { encoding:"jsonParsed", maxSupportedTransactionVersion:0 }])
        .then(function(j){ if (j && j.result) out.push({ sig:sg.signature, slot:sg.slot, blockTime:sg.blockTime, tx:j.result }); })
        .catch(function(){});
    });
  });
  return chain.then(function(){ return out; });
}

function checkMintLivePort(ctx, mint){
  return rpcPort(ctx, "getAccountInfo", [mint, { encoding:"jsonParsed" }]).then(function(j){
    var v = j && j.result && j.result.value;
    var info = v && v.data && v.data.parsed && v.data.parsed.info;
    if (!info || typeof info.supply === "undefined") return { mint:mint, ok:false, error:"not a mint / not found" };
    return { mint:mint, ok:true, mintAuth:!!info.mintAuthority, freezeAuth:!!info.freezeAuthority, flagged:!!(info.mintAuthority||info.freezeAuthority), decimals:info.decimals, supply:info.supply };
  }).catch(function(){ return { mint:mint, ok:false, error:"RPC unreachable" }; });
}

function dexPairsPort(ctx, mint){
  if (typeof ctx.deps.fetchJson !== "function") return Promise.resolve(null);
  return Promise.resolve().then(function(){
    return ctx.deps.fetchJson(DEX_TOKENS_URL + mint);
  }).then(function(j){
    try { ctx.dexHash = hashTextHex(JSON.stringify(j === undefined ? null : j)); } catch (herr){}
    return (j && Array.isArray(j.pairs)) ? j.pairs : [];
  }).catch(function(){ return null; });
}

function traceFunderPort(ctx, wallet){
  if (ctx.funderCache[wallet]) return Promise.resolve(ctx.funderCache[wallet]);
  var res = { traced:false, funder:null, at:0, reason:"" };
  if (ctx.budget.deepActive && ctx.budget.deepCalls > ctx.budget.deepCap - 6){
    res.reason = "funder not traced -- deep-check call budget nearly spent";
    return Promise.resolve(res);
  }
  return rpcPort(ctx, "getSignaturesForAddress", [wallet, { limit:100 }]).then(function(j1){
    var first = (j1 && j1.result) || [];
    if (!first.length){ res.reason = "no transactions found for this wallet"; ctx.funderCache[wallet] = res; return null; }
    if (first.length < 100) return first;
    return rpcPort(ctx, "getSignaturesForAddress", [wallet, { limit:1000 }]).then(function(j2){
      var sigs2 = (j2 && j2.result) || [];
      if (sigs2.length >= 1000){ res.reason = "funder not traced -- wallet history too long to reach its first funding (trace cap hit)"; ctx.funderCache[wallet] = res; return null; }
      return sigs2;
    });
  }).then(function(sigs){
    if (!sigs) return res;
    var oldest = sigs[sigs.length - 1];
    return rpcPort(ctx, "getTransaction", [oldest.signature, { encoding:"jsonParsed", maxSupportedTransactionVersion:0 }]).then(function(tj){
      var tx = tj && tj.result;
      var f = tx ? txFirstSolFunderPort(tx, wallet) : null;
      if (f){ res.traced = true; res.funder = f; res.at = (tx && tx.blockTime ? tx.blockTime : 0); }
      else res.reason = "first funding was not a plain SOL transfer -- linking inconclusive";
      ctx.funderCache[wallet] = res; return res;
    }).catch(function(){ res.reason = "funder transaction could not be fetched"; ctx.funderCache[wallet] = res; return res; });
  }).catch(function(err){
    res.reason = rateNotePort(err) + " while tracing the funder";
    return res;
  });
}

function funderActivityPort(ctx, funder){
  if (ctx.funderActivityCache[funder]) return Promise.resolve(ctx.funderActivityCache[funder]);
  if (ctx.budget.deepActive && ctx.budget.deepCalls > ctx.budget.deepCap - 4) return Promise.resolve({ count:-1, highVolume:false });
  return rpcPort(ctx, "getSignaturesForAddress", [funder, { limit:1000 }]).then(function(j){
    var n = ((j && j.result) || []).length;
    var a = { count:n, highVolume: n >= 1000 };
    ctx.funderActivityCache[funder] = a; return a;
  }).catch(function(){ return { count:-1, highVolume:false }; });
}

function historyForPort(ctx, wallet, currentMint){
  if (ctx.creatorCache[wallet]) return Promise.resolve(ctx.creatorCache[wallet]);
  var res = { launches:0, inactive:0, determined:0, strikes:0, fastExit:0, details:[], ageDays:null, ageIsFloor:false, sigCount:0, unknown:true, sampled:0 };
  return rpcPort(ctx, "getSignaturesForAddress", [wallet, { limit:100 }]).then(function(j){
    var sigs = (j && j.result) || [];
    res.sigCount = sigs.length;
    if (sigs.length){
      var oldest = sigs[sigs.length - 1];
      if (oldest.blockTime){
        res.ageDays = Math.max(0, (ctx.nowMs() - oldest.blockTime * 1000) / 86400000);
        res.ageIsFloor = sigs.length >= 100;
      }
    }
    if (!sigs.length){ res.unknown = true; ctx.creatorCache[wallet] = res; return res; }
    var sample = sigs.slice(0, 4);
    var step = Math.max(1, Math.floor(sigs.length / 8));
    for (var i = 4; i < sigs.length && sample.length < 8; i += step) if (sample.indexOf(sigs[i]) === -1) sample.push(sigs[i]);
    res.sampled = sample.length;
    return fetchTxsPacedPort(ctx, sample, 8).then(function(txs){
      var mints = {};
      var creatorFlow = {};
      txs.forEach(function(rec){
        var seenMints = {};
        ((rec.tx.meta && rec.tx.meta.postTokenBalances) || []).forEach(function(b){ if (b.mint) seenMints[b.mint] = 1; });
        Object.keys(seenMints).forEach(function(mm){
          if (mm === currentMint || KNOWN_NON_LAUNCH_MINTS[mm]) return;
          var dAll = txTokenDeltasPort(rec.tx, mm);
          var d = dAll[wallet];
          if (typeof d === "number" && d !== 0){
            var cf = creatorFlow[mm] || (creatorFlow[mm] = { bought:0, sold:0 });
            if (d > 0) cf.bought += d; else cf.sold += -d;
          }
        });
        if (txFeePayerPort(rec.tx) !== wallet) return;
        if (!txHasProgramPort(rec.tx, PUMP_PROGRAM)) return;
        var seen = {};
        ((rec.tx.meta && rec.tx.meta.postTokenBalances) || []).forEach(function(b){
          if (b.mint && b.mint !== currentMint && !KNOWN_NON_LAUNCH_MINTS[b.mint]) seen[b.mint] = 1;
        });
        Object.keys(seen).forEach(function(m){ mints[m] = 1; });
      });
      var list = Object.keys(mints).slice(0, 5);
      res.launches = Object.keys(mints).length;
      var chain = Promise.resolve();
      list.forEach(function(m){
        chain = chain.then(function(){
          return dexPairsPort(ctx, m).then(function(pairs){
            if (pairs === null){ res.details.push({ mint:m, inactive:null, fastDead:false, exit:false, eventTs:null }); return; }
            var maxLiq = 0, any = false, createdAt = 0;
            pairs.forEach(function(pp){
              if (pp.liquidity && typeof pp.liquidity.usd === "number"){ any = true; if (pp.liquidity.usd > maxLiq) maxLiq = pp.liquidity.usd; }
              if (typeof pp.pairCreatedAt === "number" && pp.pairCreatedAt > 0 && (!createdAt || pp.pairCreatedAt < createdAt)) createdAt = pp.pairCreatedAt;
            });
            var dead = !pairs.length || (any && maxLiq < 100);
            var fastDead = !!(dead && createdAt && (ctx.nowMs() - createdAt) < 86400000);
            var cf = creatorFlow[m];
            var exit = !!(cf && cf.bought > 0 && cf.sold >= 0.9 * cf.bought);
            res.determined++;
            if (dead) res.inactive++;
            if (fastDead || exit) res.strikes++;
            if (fastDead && exit) res.fastExit++;
            res.details.push({ mint:m, inactive:dead, fastDead:fastDead, exit:exit,
              eventTs: createdAt ? Math.floor(createdAt / 1000) : null });
          });
        });
      });
      return chain.then(function(){
        res.unknown = (res.sigCount < 3 && res.launches === 0);
        var ckeys = Object.keys(ctx.creatorCache);
        if (ckeys.length >= CAPS.caches) delete ctx.creatorCache[ckeys[0]];
        ctx.creatorCache[wallet] = res;
        return res;
      });
    });
  }).catch(function(){ return res; });
}

function lightCheckPort(ctx, mint){
  return Promise.all([
    rpcPort(ctx, "getTokenLargestAccounts", [mint]),
    rpcPort(ctx, "getTokenSupply", [mint])
  ]).then(function(res){
    var la = (res[0] && res[0].result && res[0].result.value) || [];
    var sup = res[1] && res[1].result && res[1].result.value;
    if (!sup || typeof sup.amount === "undefined") throw new Error("supply unreadable");
    var total = fin(Number(sup.amount));
    var accts = la.map(function(a){ return { acct:a.address, amount:fin(Number(a.amount)) }; })
      .filter(function(a){ return a.amount > 0; })
      .sort(function(a,b){ return b.amount - a.amount; });
    function pctOf(n){ return total > 0 ? (fin(n) / total) * 100 : 0; }
    function sumK(list, k){ var s = 0; for (var i = 0; i < Math.min(k, list.length); i++) s += list[i].amount; return s; }
    var largest = accts[0] || null;
    var curveSuspect = !!(largest && pctOf(largest.amount) >= 30);
    var rest = curveSuspect ? accts.slice(1) : accts;
    return {
      total:total, decimals:(typeof sup.decimals === "number" ? sup.decimals : 0),
      top1:pctOf(sumK(accts,1)), top5:pctOf(sumK(accts,5)), top10:pctOf(sumK(accts,10)), top10Incl:pctOf(sumK(accts,10)),
      top1Excl:pctOf(sumK(rest,1)), top10Excl:pctOf(sumK(rest,10)),
      curveSuspect:curveSuspect, curveIdentified:false, largestPct: largest ? pctOf(largest.amount) : 0,
      holders:accts, checkedAt:Math.floor(ctx.nowMs() / 1000), slot:null
    };
  });
}

async function deepCheckPort(ctx, mint, light){
  var total = light ? light.total : 0;
  var D = defaultDeep();
  var stepErr = {};
  var earlyTxs = [], newTxs = [];
  try {
    var j = await rpcPort(ctx, "getSignaturesForAddress", [mint, { limit:40 }]);
    var sigs = ((j && j.result) || []).slice();
    if (!sigs.length) throw new Error("no signatures returned for this mint");
    var asc = sigs.slice().sort(function(a,b){ return a.slot - b.slot; });
    D.creationSlot = asc[0].slot;
    var creationMs = asc[0].blockTime ? asc[0].blockTime * 1000 : ctx.nowMs();
    D.creationTime = Math.floor(creationMs / 1000);
    var earliest = asc.slice(0, 12);
    var newest = sigs.slice(0, 6);
    earlyTxs = await fetchTxsPacedPort(ctx, earliest, 12);
    D.creator = earlyTxs.length ? txFeePayerPort(earlyTxs[0].tx) : null;
    var bought = {}, sold = {}, slotBought = {}, windowWallets = [], slotWallets = [];
    var maxEarlyTime = 0;
    earlyTxs.forEach(function(rec){
      var t = rec.blockTime ? rec.blockTime * 1000 : 0;
      if (t > maxEarlyTime) maxEarlyTime = t;
      var deltas = txTokenDeltasPort(rec.tx, mint);
      var inSlot = rec.slot === D.creationSlot;
      var inWindow = t ? (t - creationMs <= 60000) : (rec.slot <= D.creationSlot + 2);
      Object.keys(deltas).forEach(function(owner){
        var d = deltas[owner];
        if (d > 0 && inWindow){
          bought[owner] = (bought[owner] || 0) + d;
          if (windowWallets.indexOf(owner) === -1) windowWallets.push(owner);
          if (inSlot){
            slotBought[owner] = (slotBought[owner] || 0) + d;
            if (owner !== D.creator && slotWallets.indexOf(owner) === -1) slotWallets.push(owner);
          }
        }
        if (d < 0) sold[owner] = (sold[owner] || 0) + (-d);
      });
    });
    var windowAmt = 0, slotAmt = 0;
    windowWallets.forEach(function(w){ if (w !== D.creator) windowAmt += bought[w] || 0; });
    slotWallets.forEach(function(w){ slotAmt += slotBought[w] || 0; });
    var amounts = slotWallets.map(function(w){ return slotBought[w] || 0; }).sort(function(a,b){ return a - b; });
    var matching = false;
    if (amounts.length >= 3){
      var med = amounts[Math.floor(amounts.length / 2)];
      var near = amounts.filter(function(a){ return med > 0 && Math.abs(a - med) / med <= 0.10; }).length;
      matching = near >= 3;
    }
    var reachedCreate = sigs.length < 40;
    var windowCovered = reachedCreate && (sigs.length < 40 || maxEarlyTime - creationMs >= 60000 || earlyTxs.length < 12);
    D.bundle = { pct: total > 0 ? (windowAmt / total) * 100 : 0,
      slotPct: total > 0 ? (slotAmt / total) * 100 : 0,
      count:windowWallets.filter(function(w){ return w !== D.creator; }).length,
      slotCount:slotWallets.length,
      wallets:windowWallets.filter(function(w){ return w !== D.creator; }).map(function(w){ return { wallet:w, pct: total > 0 ? ((bought[w] || 0) / total) * 100 : 0, inSlot: !!slotBought[w] }; }),
      slotWallets:slotWallets.slice(),
      matching:matching, windowFull: windowCovered, partial: !windowCovered,
      reachedCreate: reachedCreate, creatorInWindow: !!(D.creator && bought[D.creator] > 0) };
    D._bought = bought; D._slotBought = slotBought; D._soldEarly = Object.assign({}, sold);
    newTxs = await fetchTxsPacedPort(ctx, newest, 6);
    newTxs.forEach(function(rec){
      var deltas = txTokenDeltasPort(rec.tx, mint);
      Object.keys(deltas).forEach(function(owner){ if (deltas[owner] < 0) sold[owner] = (sold[owner] || 0) + (-deltas[owner]); });
    });
    D._sold = sold;
    var topAccts = light ? light.holders.slice(0, 12).map(function(h){ return h.acct; }) : [];
    var ownerMap = null;
    if (topAccts.length){
      try {
        var mj = await rpcPort(ctx, "getMultipleAccounts", [topAccts, { encoding:"jsonParsed" }]);
        ownerMap = {};
        ((mj && mj.result && mj.result.value) || []).forEach(function(v){
          try {
            var info = v && v.data && v.data.parsed && v.data.info;
            if (info && info.owner) ownerMap[info.owner] = (ownerMap[info.owner] || 0) + Number(info.tokenAmount.amount);
          } catch (err){}
        });
      } catch (oerr){ ownerMap = null; }
    }
    D.ownerMap = ownerMap;
    if (ownerMap && D.creator && typeof ownerMap[D.creator] === "number" && total > 0)
      D.creatorHoldingPct = (ownerMap[D.creator] / total) * 100;
    var buyers = Object.keys(bought).filter(function(w){ return w !== D.creator; }).slice(0, 6);
    var traces = {};
    for (var bi = 0; bi < buyers.length; bi++){
      await sleepPort(120);
      traces[buyers[bi]] = await traceFunderPort(ctx, buyers[bi]);
    }
    var byFunder = {};
    buyers.forEach(function(w){
      var tr = traces[w];
      if (tr && tr.traced && tr.funder) (byFunder[tr.funder] = byFunder[tr.funder] || []).push(w);
    });
    var funderKeys = Object.keys(byFunder);
    for (var fi = 0; fi < funderKeys.length; fi++) await funderActivityPort(ctx, funderKeys[fi]);
    var linked = [], links = {}, inconclusive = [];
    funderKeys.forEach(function(f){
      var ws = byFunder[f];
      var act = ctx.funderActivityCache[f];
      if (ws.length >= 2){
        if (act && act.highVolume){
          inconclusive.push("several early buyers share one very high-volume funder (exchange-like) -- linking inconclusive");
        } else {
          links[f] = ws.slice();
          ws.forEach(function(w){ if (linked.indexOf(w) === -1) linked.push(w); });
        }
      }
    });
    buyers.forEach(function(w){
      var tr = traces[w];
      if (tr && tr.traced && tr.funder === D.creator && linked.indexOf(w) === -1){
        linked.push(w); (links[D.creator] = links[D.creator] || []).push(w);
      }
      if (tr && !tr.traced && tr.reason) inconclusive.push(tr.reason);
    });
    function curPctLocal(w){
      if (D.ownerMap && typeof D.ownerMap[w] === "number" && total > 0) return fin((fin(D.ownerMap[w]) / total) * 100);
      if (total > 0 && bought[w]) return fin((fin(bought[w]) / total) * 100);
      return 0;
    }
    var unresolvedPct = 0, unresolvedWallets = [];
    buyers.forEach(function(w){
      var tr = traces[w];
      var isLinked = linked.indexOf(w) !== -1;
      if (!isLinked && (!tr || !tr.traced)){ unresolvedPct += curPctLocal(w); unresolvedWallets.push(w); }
    });
    var windowSet = {};
    if (D.bundle) D.bundle.wallets.forEach(function(bw){ windowSet[bw.wallet] = 1; });
    var linkedOutside = linked.filter(function(w){ return !windowSet[w]; });
    var sharedFunderInWindow = false;
    Object.keys(links).forEach(function(f){
      var inWin = links[f].filter(function(w){ return windowSet[w]; });
      if (inWin.length >= 2) sharedFunderInWindow = true;
    });
    D.funders = { linked:linked.map(function(w){ return { wallet:w, pct:curPctLocal(w) }; }),
      links:links, inconclusive:inconclusive, tracedCount:buyers.length,
      unresolvedPct:unresolvedPct, unresolvedWallets:unresolvedWallets,
      linkedOutside:linkedOutside, sharedFunderInWindow:sharedFunderInWindow, traces:traces };
    var pctByWallet = {};
    if (D.bundle) D.bundle.wallets.forEach(function(bw){ pctByWallet[bw.wallet] = fin(bw.pct); });
    linked.forEach(function(w){ pctByWallet[w] = curPctLocal(w); });
    D.pctByWallet = pctByWallet;
    var mainFunder = funderKeys.length ? funderKeys[0] : null;
    var histOk = true;
    try {
      D.history = D.creator ? await historyForPort(ctx, D.creator, mint) : null;
      D.funderHistory = (mainFunder && mainFunder !== D.creator) ? await historyForPort(ctx, mainFunder, mint) : null;
      D.combinedHistory = mergeHist(D.history, D.funderHistory);
    } catch (herr){
      histOk = false;
      stepErr.history = herr;
      D.historyError = rateNotePort(herr) + " while sampling creator history";
    }
    if (histOk){
      var cluster = [D.creator].concat(linked).filter(Boolean);
      var init = 0, sld = 0;
      cluster.forEach(function(w){ init += bought[w] || 0; sld += sold[w] || 0; });
      var crossingTs = null;
      if (init > 0 && sld / init >= 0.9){
        var seenSig = {}, recs = [];
        earlyTxs.concat(newTxs).forEach(function(rec){
          if (seenSig[rec.sig]) return;
          seenSig[rec.sig] = 1;
          recs.push(rec);
        });
        recs.sort(function(a,b){
          var ta = a.blockTime || 0, tb = b.blockTime || 0;
          return ta !== tb ? ta - tb : (a.slot || 0) - (b.slot || 0);
        });
        var cum = 0, clusterSet = {};
        cluster.forEach(function(w){ clusterSet[w] = 1; });
        for (var ri = 0; ri < recs.length; ri++){
          var deltas2 = txTokenDeltasPort(recs[ri].tx, mint);
          Object.keys(deltas2).forEach(function(owner){
            if (clusterSet[owner] && deltas2[owner] < 0) cum += -deltas2[owner];
          });
          if (cum / init >= 0.9){ crossingTs = (typeof recs[ri].blockTime === "number") ? recs[ri].blockTime : null; break; }
        }
      }
      D.sells = { soldPct: init > 0 ? Math.min(100, (sld / init) * 100) : 0, initial:init, checked:true, crossingTs:crossingTs };
      var pairs = await dexPairsPort(ctx, mint);
      if (pairs === null){ D.liquidity = { state:"unknown", usd:null }; }
      else if (!pairs.length){ D.liquidity = { state:"curve", usd:0 }; }
      else {
        var maxLiq = null, graduated = false;
        pairs.forEach(function(p){
          if (p.liquidity && typeof p.liquidity.usd === "number") maxLiq = Math.max(maxLiq || 0, p.liquidity.usd);
          if (["raydium","pumpswap","meteora","orca"].indexOf(String(p.dexId || "")) !== -1) graduated = true;
        });
        var holdersRemain = light && light.top10Excl > 5;
        D.liquidity = { state: graduated ? (maxLiq !== null && maxLiq < 50 && holdersRemain ? "gone" : "live") : "curve",
          usd:maxLiq, graduated:graduated };
      }
      var am = await checkMintLivePort(ctx, mint);
      D.auth = am && am.ok ? { mintAuth:am.mintAuth, freezeAuth:am.freezeAuthority } : null;
    }
  } catch (derr){
    stepErr.deep = derr;
  }
  return { deep:D, stepErr:stepErr };
}

/* Never throws: a failed step lands as a null family plus an entry in
   fetchErrors. */
async function fetchEvidence(mint, deps){
  var ctx = makeFetchCtx(deps);
  var fetchedAtSec = Math.floor(ctx.nowMs() / 1000);
  var fetchErrors = {};
  var light = null;
  try {
    light = await lightCheckPort(ctx, mint);
  } catch (lerr){
    fetchErrors.light = errMessage(lerr);
  }
  var deep;
  if (light){
    var res = await deepCheckPort(ctx, mint, light);
    deep = res.deep;
    if (res.stepErr.deep) fetchErrors.deep = errMessage(res.stepErr.deep);
    if (res.stepErr.history) fetchErrors.history = errMessage(res.stepErr.history);
    if (res.stepErr.funders) fetchErrors.funders = errMessage(res.stepErr.funders);
  } else {
    deep = defaultDeep();
  }
  return { mint:mint, fetchedAtSec:fetchedAtSec, light:light, deep:deep,
    fetchErrors:fetchErrors,
    sources:{ rpcUrl:ctx.rpcUrl, rpcResponseHashes:ctx.rpcResponseHashes, dexResponseHash:ctx.dexHash || null } };
}

/* ================= bundle + hash (new) ================= */

function canonNum(x){
  var v = (typeof x === "number" && isFinite(x)) ? x : 0;
  var r = Math.round(v * 10000) / 10000;
  return r === 0 ? 0 : r;
}

function writeCanon(v){
  if (v === null || typeof v === "undefined") return "null";
  if (typeof v === "number") return String(canonNum(v));
  if (typeof v === "string") return JSON.stringify(v);
  if (typeof v === "boolean") return v ? "true" : "false";
  if (Array.isArray(v)){
    var parts = [];
    for (var i = 0; i < v.length; i++) parts.push(writeCanon(v[i]));
    return "[" + parts.join(",") + "]";
  }
  if (typeof v === "object"){
    var keys = Object.keys(v).sort();
    var out = [];
    for (var ki = 0; ki < keys.length; ki++) out.push(JSON.stringify(keys[ki]) + ":" + writeCanon(v[keys[ki]]));
    return "{" + out.join(",") + "}";
  }
  return "null";
}

function canonicalizeBundle(bundle){
  return writeCanon(bundle);
}

/* Missing data is explicit null, never an omitted key. */
function fillNulls(v){
  if (typeof v === "undefined" || typeof v === "function") return null;
  if (v === null || typeof v !== "object") return v;
  if (Array.isArray(v)){
    var a = [];
    for (var i = 0; i < v.length; i++) a.push(fillNulls(v[i]));
    return a;
  }
  var o = {};
  Object.keys(v).forEach(function(k){ o[k] = fillNulls(v[k]); });
  return o;
}

function pickVal(v, fallback){
  return (typeof v === "undefined" || v === null) ? fallback : v;
}

function sortByWallet(rows){
  if (!Array.isArray(rows)) return rows;
  rows.sort(function(a, b){
    var wa = (a && typeof a.wallet === "string") ? a.wallet : "";
    var wb = (b && typeof b.wallet === "string") ? b.wallet : "";
    return wa < wb ? -1 : wa > wb ? 1 : 0;
  });
  return rows;
}
function sortStrings(arr){
  if (!Array.isArray(arr)) return arr;
  arr.sort();
  return arr;
}

function buildBundle(input){
  input = input || {};
  var resultIn = input.result || {};
  var crewIn = input.crewStore || emptyCrewStore();
  var evidenceFilled = fillNulls(typeof input.evidence === "undefined" ? null : input.evidence);

  /* Raw token amounts travel as decimal strings in a bundle. */
  if (evidenceFilled && evidenceFilled.light && typeof evidenceFilled.light.total === "number")
    evidenceFilled.light.total = String(evidenceFilled.light.total);
  if (evidenceFilled && evidenceFilled.deep && evidenceFilled.deep.ownerMap &&
      typeof evidenceFilled.deep.ownerMap === "object"){
    Object.keys(evidenceFilled.deep.ownerMap).forEach(function(w){
      if (typeof evidenceFilled.deep.ownerMap[w] === "number")
        evidenceFilled.deep.ownerMap[w] = String(evidenceFilled.deep.ownerMap[w]);
    });
  }

  /* Set-like arrays are sorted by the builder, before any
     canonicalizing: linked and bundle wallet rows by wallet, the
     plain wallet lists and the crew order lexicographically. */
  if (evidenceFilled && evidenceFilled.deep){
    var d = evidenceFilled.deep;
    if (d.funders){
      sortByWallet(d.funders.linked);
      sortStrings(d.funders.linkedOutside);
    }
    if (d.bundle) sortByWallet(d.bundle.wallets);
    sortStrings(d.h5Wallets);
  }

  var crewSnapshot = {
    flagged: fillNulls(crewIn.flagged || {}),
    appearances: fillNulls(crewIn.appearances || {}),
    order: sortStrings(Array.isArray(crewIn.order) ? crewIn.order.slice() : []),
    totalDeepChecked: (typeof crewIn.totalDeepChecked === "number") ? crewIn.totalDeepChecked : null
  };

  var resultFilled = {
    hardTriggers: fillNulls(pickVal(resultIn.hardTriggers, null)),
    primaryHardTrigger: pickVal(resultIn.primaryHardTrigger, null),
    softScore: pickVal(resultIn.softScore, null),
    families: fillNulls(pickVal(resultIn.families, null)),
    familiesHit: pickVal(resultIn.familiesHit, null),
    partial: pickVal(resultIn.partial, null),
    missingCore: fillNulls(pickVal(resultIn.missingCore, null)),
    resolutionBlockers: fillNulls(pickVal(resultIn.resolutionBlockers, null))
  };

  if (Array.isArray(resultFilled.hardTriggers)){
    resultFilled.hardTriggers.forEach(function(t){
      if (t && t.values && Array.isArray(t.values.wallets)) sortStrings(t.values.wallets);
    });
  }

  var sourcesIn = (input.evidence && input.evidence.sources && typeof input.evidence.sources === "object")
    ? input.evidence.sources : {};

  var bundle = {
    version: 1,
    market: pickVal(input.market, null),
    subjectMint: pickVal(input.subjectMint, null),
    resolver: pickVal(input.resolver, null),
    fetchedAtSec: pickVal(input.fetchedAtSec, null),
    expirySec: pickVal(input.expirySec, null),
    evidence: evidenceFilled,
    crewSnapshot: crewSnapshot,
    totalDeepChecked: (resultIn.crewStore && typeof resultIn.crewStore.totalDeepChecked === "number")
      ? resultIn.crewStore.totalDeepChecked : crewSnapshot.totalDeepChecked,
    result: resultFilled,
    sources: {
      rpcUrl: pickVal(sourcesIn.rpcUrl, null),
      rpcResponseHashes: fillNulls(pickVal(sourcesIn.rpcResponseHashes, {})),
      dexResponseHash: pickVal(sourcesIn.dexResponseHash, null)
    }
  };
  return fillNulls(bundle);
}

function hashBundle(bundle){
  if (!cryptoMod) throw new Error("node:crypto is not available in this environment");
  var canon = canonicalizeBundle(bundle);
  var bytes = cryptoMod.createHash("sha256").update(canon, "utf8").digest();
  var allZero = true;
  for (var i = 0; i < bytes.length; i++){ if (bytes[i] !== 0){ allZero = false; break; } }
  if (allZero) throw new Error("bundle hash is all zero");
  return { hex: bytes.toString("hex"), bytes: bytes };
}

function verifyBundle(bundle, expectedHex){
  var out = { hashOk:false, verdictOk:false, result:null };
  if (!bundle || typeof bundle !== "object") return out;
  try {
    var h = hashBundle(bundle);
    out.hashOk = !!expectedHex && h.hex === String(expectedHex);
  } catch (herr){
    out.hashOk = false;
  }
  var snap = (bundle.crewSnapshot && typeof bundle.crewSnapshot === "object") ? bundle.crewSnapshot : {};
  var store = {
    flagged: cloneData(snap.flagged || {}) || {},
    appearances: cloneData(snap.appearances || {}) || {},
    order: Array.isArray(snap.order) ? snap.order.slice() : [],
    countedMints: [],
    totalDeepChecked: (typeof snap.totalDeepChecked === "number") ? snap.totalDeepChecked : 0
  };
  var re = evaluateEvidence(bundle.evidence, store);
  out.result = re;
  var bres = bundle.result || {};
  var firedAgree = Array.isArray(bres.hardTriggers) && bres.hardTriggers.length === re.hardTriggers.length &&
    bres.hardTriggers.every(function(t, i){ return !!t.fired === !!re.hardTriggers[i].fired; });
  out.verdictOk = !!(bres && bres.primaryHardTrigger === re.primaryHardTrigger && firedAgree &&
    canonNum(bres.softScore) === canonNum(re.softScore));
  return out;
}

if (typeof module !== "undefined") module.exports = {
  emptyCrewStore: emptyCrewStore,
  evaluateEvidence: evaluateEvidence,
  fetchEvidence: fetchEvidence,
  buildBundle: buildBundle,
  canonicalizeBundle: canonicalizeBundle,
  canonNum: canonNum,
  hashBundle: hashBundle,
  verifyBundle: verifyBundle,
  fin: fin,
  f1s: f1s,
  mergeHist: mergeHist,
  deepCheckedCount: deepCheckedCount,
  evaluateH5: evaluateH5,
  computeInsider: computeInsider,
  recordAppearances: recordAppearances,
  recordFlaggedCrew: recordFlaggedCrew,
  computeVerdict: computeVerdict,
  alertSentenceFor: alertSentenceFor
};
