// Vendored snapshot of the verdict kernel functions from the Solana OS
// single-file app, lifted mechanically (function text copied verbatim)
// for offline comparison against rugcall/app/evidence.js.
//
// Source: workspace/your_files/solana-os/solana-os.html
// Source sha256: 722ffc445a202f10509462bfda52aafddf141f6919968816e077b80fc0deb536
// Lift date: 2026-11-20
//
// The functions below are plain declarations. Globals they reference
// (SESSION_FLAGGED_WALLETS, SESSION_WALLET_APPEARANCES,
// SESSION_FLAGGED_ORDER, LAUNCH_ORDER, LAUNCHES, CAPS, EVICTED) are
// only touched when a function that uses them is called, so this file
// is safe to require() in a test harness that supplies its own
// globals (for example inside a vm context).

function fin(x){ return (typeof x === "number" && isFinite(x)) ? x : 0; }

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
