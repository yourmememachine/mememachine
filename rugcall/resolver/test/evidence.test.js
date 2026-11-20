"use strict";

// Tests for rugcall/app/evidence.js: golden verdict fixtures, bundle
// hashing, a source diff against the vendored kernel snapshot, and a
// corpus comparison that drives the original kernel functions in a
// vm context over a deterministic boundary grid.

const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const ev = require("../../app/evidence.js");

const FIXTURES = path.join(__dirname, "fixtures");
const EVIDENCE_DIR = path.join(FIXTURES, "evidence");
const CREATED = 1799913600;
const EXPIRY = 1800000000;
const RESOLVER_PUBKEY = "Dm95oxQp8YghEsCads1SXWkGwy4BaYLjCYv4fieG7GtH";

function loadEvidence(name) {
  return JSON.parse(
    fs.readFileSync(path.join(EVIDENCE_DIR, `${name}.json`), "utf8")
  );
}

function loadCrewStore() {
  return JSON.parse(
    fs.readFileSync(path.join(FIXTURES, "crew-store.json"), "utf8")
  );
}

function loadMarkets() {
  return JSON.parse(
    fs.readFileSync(path.join(FIXTURES, "markets.json"), "utf8")
  );
}

function evaluate(name, crewStore) {
  return ev.evaluateEvidence(
    loadEvidence(name),
    crewStore || ev.emptyCrewStore()
  );
}

function firedNames(result) {
  return result.hardTriggers.filter((t) => t.fired).map((t) => t.name);
}

function trigger(result, id) {
  return result.hardTriggers.find((t) => t.id === id);
}

function totalOf(result) {
  return typeof result.softScore === "number"
    ? result.softScore
    : result.softScore.total;
}

function checkGolden(name, expected, crewStore) {
  const r = evaluate(name, crewStore);
  if (expected.verdict !== undefined) {
    assert.equal(r.displayVerdict, expected.verdict, `${name} verdict`);
  }
  assert.deepEqual(firedNames(r), expected.fired || [], `${name} fired`);
  assert.equal(r.primaryHardTrigger, expected.primary || 0, `${name} primary`);
  if (expected.total !== undefined) {
    assert.equal(totalOf(r), expected.total, `${name} soft total`);
  }
  if (expected.fam) {
    assert.deepEqual(r.families, expected.fam, `${name} families`);
  }
  if (expected.familiesHit !== undefined) {
    assert.equal(r.familiesHit, expected.familiesHit, `${name} familiesHit`);
  }
  assert.equal(r.partial, expected.partial === true, `${name} partial`);
  assert.equal(r.complete, expected.complete === true, `${name} complete`);
  if (expected.missing) {
    assert.ok(
      expected.missing.test(r.missingCore.join(",")),
      `${name} missingCore ${JSON.stringify(r.missingCore)} should match ${expected.missing}`
    );
  }
  if (expected.noMissing) {
    assert.deepEqual(r.missingCore, [], `${name} missingCore empty`);
  }
  if (expected.blockers) {
    assert.ok(
      expected.blockers.test(r.resolutionBlockers.join(",")),
      `${name} resolutionBlockers ${JSON.stringify(r.resolutionBlockers)} should match ${expected.blockers}`
    );
  }
  if (expected.noBlockers) {
    assert.deepEqual(
      r.resolutionBlockers,
      [],
      `${name} resolutionBlockers empty`
    );
  }
  if (expected.event) {
    const t = trigger(r, expected.event.id);
    assert.equal(t.eventTimestamp, expected.event.ts, `${name} event ts`);
  }
  return r;
}

const ZERO_FAM = { bundle: 0, side: 0, history: 0, structure: 0 };

// ---------------------------------------------------------------------
// (a) Golden verdict fixtures
// ---------------------------------------------------------------------

test("golden clean: no red flags, score 0, complete", () => {
  checkGolden("clean", {
    verdict: "NO RED FLAGS FOUND",
    fired: [],
    primary: 0,
    total: 0,
    fam: ZERO_FAM,
    familiesHit: 0,
    partial: false,
    complete: true,
    noMissing: true,
    noBlockers: true,
  });
});

test("golden h2-bundle: coordinated 55 percent bundle fires H2", () => {
  checkGolden("h2-bundle", {
    verdict: "FLAGGED",
    fired: ["H2"],
    primary: 2,
    total: 30,
    fam: { bundle: 30, side: 0, history: 0, structure: 0 },
    complete: true,
    event: { id: 2, ts: CREATED + 60 },
  });
});

test("golden h2-boundary: bundle pct exactly 40 fires H2", () => {
  checkGolden("h2-boundary", {
    verdict: "FLAGGED",
    fired: ["H2"],
    primary: 2,
    total: 24,
    fam: { bundle: 24, side: 0, history: 0, structure: 0 },
    complete: true,
    event: { id: 2, ts: CREATED + 60 },
  });
});

test("golden h2-below: bundle pct 39.9 does not fire H2", () => {
  checkGolden("h2-below", {
    verdict: "CAUTION",
    fired: [],
    primary: 0,
    total: 24,
    fam: { bundle: 24, side: 0, history: 0, structure: 0 },
    complete: true,
  });
});

test("golden h2-nocoord: 55 percent without coordination does not fire H2", () => {
  checkGolden("h2-nocoord", {
    verdict: "CAUTION",
    fired: [],
    primary: 0,
    total: 24,
    fam: { bundle: 24, side: 0, history: 0, structure: 0 },
    complete: true,
  });
});

test("golden h1-rate: strike rate 0.5 over 6 launches fires H1", () => {
  checkGolden("h1-rate", {
    verdict: "FLAGGED",
    fired: ["H1"],
    primary: 1,
    total: 0,
    fam: ZERO_FAM,
    complete: true,
    event: { id: 1, ts: CREATED - 86400 },
  });
});

test("golden h1-absolute: three fast exits fire H1 regardless of rate", () => {
  checkGolden("h1-absolute", {
    verdict: "FLAGGED",
    fired: ["H1"],
    primary: 1,
    total: 0,
    fam: ZERO_FAM,
    complete: true,
    event: { id: 1, ts: CREATED - 86400 },
  });
});

test("golden h1-negative: strike rate 0.43 does not fire H1, history scores 12", () => {
  checkGolden("h1-negative", {
    verdict: "NO RED FLAGS FOUND",
    fired: [],
    primary: 0,
    total: 12,
    fam: { bundle: 0, side: 0, history: 12, structure: 0 },
    complete: true,
  });
});

test("golden h3-exit: 95 percent sold fires H3 at the crossing time", () => {
  checkGolden("h3-exit", {
    verdict: "FLAGGED",
    fired: ["H3"],
    primary: 3,
    total: 0,
    fam: ZERO_FAM,
    complete: true,
    event: { id: 3, ts: CREATED + 7200 },
  });
});

test("golden h3-boundary: sold pct exactly 90 fires H3", () => {
  checkGolden("h3-boundary", {
    verdict: "FLAGGED",
    fired: ["H3"],
    primary: 3,
    total: 0,
    complete: true,
    event: { id: 3, ts: CREATED + 7200 },
  });
});

test("golden h3-below: sold pct 89.9 does not fire H3", () => {
  checkGolden("h3-below", {
    verdict: "NO RED FLAGS FOUND",
    fired: [],
    primary: 0,
    total: 0,
    fam: ZERO_FAM,
    complete: true,
  });
});

test("golden h3-unchecked: unchecked sells cannot fire H3 and block resolution", () => {
  const r = checkGolden("h3-unchecked", {
    fired: [],
    primary: 0,
    total: 0,
    partial: false,
    complete: false,
    blockers: /sell/i,
  });
  assert.equal(trigger(r, 3).fired, false);
});

test("golden h4-gone: drained liquidity fires H4 at fetch time", () => {
  checkGolden("h4-gone", {
    verdict: "FLAGGED",
    fired: ["H4"],
    primary: 4,
    total: 0,
    fam: ZERO_FAM,
    complete: true,
    event: { id: 4, ts: CREATED + 3600 },
  });
});

test("golden h5-crew: crew wallet in two prior flagged coins fires H5", () => {
  checkGolden(
    "h5-crew",
    {
      verdict: "FLAGGED",
      fired: ["H5"],
      primary: 5,
      total: 0,
      fam: ZERO_FAM,
      complete: true,
      event: { id: 5, ts: CREATED },
    },
    loadCrewStore()
  );
});

test("golden soft-flagged: soft score 46 across two families flags with no hard trigger", () => {
  checkGolden("soft-flagged", {
    verdict: "FLAGGED",
    fired: [],
    primary: 0,
    total: 46,
    fam: { bundle: 30, side: 16, history: 0, structure: 0 },
    familiesHit: 2,
    complete: true,
  });
});

test("golden soft-exact45: soft score exactly 45 flags", () => {
  checkGolden("soft-exact45", {
    verdict: "FLAGGED",
    fired: [],
    primary: 0,
    total: 45,
    fam: { bundle: 14, side: 16, history: 15, structure: 0 },
    familiesHit: 3,
    complete: true,
  });
});

test("golden soft-44: soft score 44 is caution, not flagged", () => {
  checkGolden("soft-44", {
    verdict: "CAUTION",
    fired: [],
    primary: 0,
    total: 44,
    fam: { bundle: 22, side: 16, history: 0, structure: 6 },
    familiesHit: 3,
    complete: true,
  });
});

test("golden partial-bundle: missing bundle is partial and incomplete", () => {
  checkGolden("partial-bundle", {
    verdict: "CAUTION",
    fired: [],
    primary: 0,
    total: 0,
    partial: true,
    complete: false,
    missing: /bundle/i,
  });
});

test("golden partial-funders: missing funders is partial and incomplete", () => {
  checkGolden("partial-funders", {
    verdict: "CAUTION",
    fired: [],
    primary: 0,
    partial: true,
    complete: false,
    missing: /fund/i,
  });
});

test("golden partial-history: missing history is partial and incomplete", () => {
  checkGolden("partial-history", {
    verdict: "CAUTION",
    fired: [],
    primary: 0,
    partial: true,
    complete: false,
    missing: /hist/i,
  });
});

test("golden partial-insider: zero supply total makes the insider figure missing", () => {
  checkGolden("partial-insider", {
    verdict: "CAUTION",
    fired: [],
    primary: 0,
    partial: true,
    complete: false,
    missing: /insider/i,
  });
});

test("golden partial-bundleflag: bundle partial flag is partial and incomplete", () => {
  checkGolden("partial-bundleflag", {
    verdict: "CAUTION",
    fired: [],
    primary: 0,
    partial: true,
    complete: false,
    missing: /bundle/i,
  });
});

test("golden partial-sells: unchecked sells block resolution without making the evidence partial", () => {
  checkGolden("partial-sells", {
    fired: [],
    primary: 0,
    partial: false,
    complete: false,
    blockers: /sell/i,
  });
});

test("golden partial-liquidity: unknown liquidity blocks resolution without making the evidence partial", () => {
  checkGolden("partial-liquidity", {
    fired: [],
    primary: 0,
    partial: false,
    complete: false,
    blockers: /liquid/i,
  });
});

test("golden late-trigger: H3 fires at evidence level even after expiry", () => {
  checkGolden("late-trigger", {
    verdict: "FLAGGED",
    fired: ["H3"],
    primary: 3,
    total: 0,
    complete: true,
    event: { id: 3, ts: EXPIRY + 3600 },
  });
});

test("golden at-expiry-trigger: H3 fires with its event exactly at expiry", () => {
  checkGolden("at-expiry-trigger", {
    verdict: "FLAGGED",
    fired: ["H3"],
    primary: 3,
    total: 0,
    complete: true,
    event: { id: 3, ts: EXPIRY },
  });
});

// ---------------------------------------------------------------------
// (b) Bundle hashing
// ---------------------------------------------------------------------

const GOLDEN_NAMES = fs
  .readdirSync(EVIDENCE_DIR)
  .filter((f) => f.endsWith(".json"))
  .map((f) => f.slice(0, -5))
  .sort();

function marketFor(name, evidence) {
  const markets = loadMarkets();
  const found = markets.markets.find((m) => m.evidence_file === `${name}.json`);
  if (found) return found;
  const template = markets.markets[0];
  return {
    ...template,
    subject_mint: evidence.mint,
    evidence_file: `${name}.json`,
  };
}

function bundleFor(name, evidence, crewStore) {
  const store = crewStore || ev.emptyCrewStore();
  const result = ev.evaluateEvidence(evidence, store);
  const bundle = ev.buildBundle({
    market: marketFor(name, evidence),
    subjectMint: evidence.mint,
    resolver: RESOLVER_PUBKEY,
    expirySec: EXPIRY,
    fetchedAtSec: evidence.fetchedAtSec,
    evidence,
    crewStore: store,
    result,
  });
  return { bundle, result };
}

function crewFor(name) {
  return name === "h5-crew" ? loadCrewStore() : ev.emptyCrewStore();
}

test("hash: building the same bundle twice is deterministic", () => {
  const a = bundleFor("h2-bundle", loadEvidence("h2-bundle"));
  const b = bundleFor("h2-bundle", loadEvidence("h2-bundle"));
  assert.equal(ev.canonicalizeBundle(a.bundle), ev.canonicalizeBundle(b.bundle));
  assert.equal(ev.hashBundle(a.bundle).hex, ev.hashBundle(b.bundle).hex);
  assert.deepEqual(ev.hashBundle(a.bundle).bytes, ev.hashBundle(b.bundle).bytes);
});

test("hash: hashBundle matches an independent sha256 of the canonical string", () => {
  const { bundle } = bundleFor("soft-flagged", loadEvidence("soft-flagged"));
  const canonical = ev.canonicalizeBundle(bundle);
  const digest = crypto.createHash("sha256").update(canonical, "utf8").digest();
  const hashed = ev.hashBundle(bundle);
  assert.equal(hashed.hex, digest.toString("hex"));
  assert.deepEqual(Buffer.from(hashed.bytes), digest);
});

test("hash: object key order in the evidence does not change the hash", () => {
  const reverseKeys = (value) => {
    if (Array.isArray(value)) return value.map(reverseKeys);
    if (value && typeof value === "object") {
      const out = {};
      for (const key of Object.keys(value).reverse()) {
        out[key] = reverseKeys(value[key]);
      }
      return out;
    }
    return value;
  };
  const original = loadEvidence("h2-bundle");
  const reversed = reverseKeys(loadEvidence("h2-bundle"));
  assert.notDeepEqual(Object.keys(reversed), Object.keys(original));
  const a = bundleFor("h2-bundle", original);
  const b = bundleFor("h2-bundle", reversed);
  assert.equal(ev.canonicalizeBundle(a.bundle), ev.canonicalizeBundle(b.bundle));
  assert.equal(ev.hashBundle(a.bundle).hex, ev.hashBundle(b.bundle).hex);
});

test("hash: order of funders.linked does not change the hash", () => {
  const w1 = "LinkedWa11etA11111111111111111111111111111111";
  const w2 = "LinkedWa11etB11111111111111111111111111111111";
  const make = (linked) => {
    const evidence = loadEvidence("clean");
    evidence.deep.funders.linked = linked;
    evidence.deep.pctByWallet = { [w1]: 1.1, [w2]: 2.2 };
    return evidence;
  };
  const a = bundleFor(
    "clean",
    make([
      { wallet: w1, pct: 1.1 },
      { wallet: w2, pct: 2.2 },
    ])
  );
  const b = bundleFor(
    "clean",
    make([
      { wallet: w2, pct: 2.2 },
      { wallet: w1, pct: 1.1 },
    ])
  );
  assert.equal(ev.canonicalizeBundle(a.bundle), ev.canonicalizeBundle(b.bundle));
  assert.equal(ev.hashBundle(a.bundle).hex, ev.hashBundle(b.bundle).hex);
});

test("hash: changing sells soldPct changes the hash", () => {
  const a = bundleFor("h3-exit", loadEvidence("h3-exit"));
  const changed = loadEvidence("h3-exit");
  changed.deep.sells.soldPct = 94.9;
  const b = bundleFor("h3-exit", changed);
  assert.notEqual(ev.hashBundle(a.bundle).hex, ev.hashBundle(b.bundle).hex);
});

test("hash: every golden produces a 64 char lowercase non-zero hash and 32 bytes", () => {
  assert.ok(GOLDEN_NAMES.length >= 26, `goldens found: ${GOLDEN_NAMES.length}`);
  for (const name of GOLDEN_NAMES) {
    const { bundle } = bundleFor(name, loadEvidence(name), crewFor(name));
    const hashed = ev.hashBundle(bundle);
    assert.match(hashed.hex, /^[0-9a-f]{64}$/, `${name} hex shape`);
    assert.notEqual(hashed.hex, "0".repeat(64), `${name} non-zero`);
    assert.equal(Buffer.from(hashed.bytes).length, 32, `${name} byte length`);
  }
});

test("hash: verifyBundle re-derives the verdict for the resolvable goldens", () => {
  for (const name of [
    "clean",
    "h2-bundle",
    "soft-flagged",
    "soft-exact45",
    "h3-exit",
    "h4-gone",
  ]) {
    const { bundle, result } = bundleFor(name, loadEvidence(name), crewFor(name));
    const { hex } = ev.hashBundle(bundle);
    const verified = ev.verifyBundle(bundle, hex);
    assert.equal(verified.hashOk, true, `${name} hashOk`);
    assert.equal(verified.verdictOk, true, `${name} verdictOk`);
    assert.ok(verified.result, `${name} result present`);
    assert.equal(
      verified.result.displayVerdict,
      result.displayVerdict,
      `${name} re-derived verdict`
    );
  }
});

test("hash: a tampered bundle fails verification", () => {
  const tamperFirstValue = (value) => {
    if (typeof value === "number") return value + 1;
    if (typeof value === "string") return `${value}x`;
    if (Array.isArray(value)) {
      for (let i = 0; i < value.length; i += 1) {
        const next = tamperFirstValue(value[i]);
        if (next !== value[i]) {
          const copy = value.slice();
          copy[i] = next;
          return copy;
        }
      }
      return value;
    }
    if (value && typeof value === "object") {
      for (const key of Object.keys(value)) {
        const next = tamperFirstValue(value[key]);
        if (next !== value[key]) return { ...value, [key]: next };
      }
    }
    return value;
  };
  const { bundle } = bundleFor("clean", loadEvidence("clean"));
  const { hex } = ev.hashBundle(bundle);
  const tampered = tamperFirstValue(JSON.parse(JSON.stringify(bundle)));
  assert.notDeepEqual(tampered, bundle);
  assert.notEqual(ev.hashBundle(tampered).hex, hex);
  assert.equal(ev.verifyBundle(tampered, hex).hashOk, false);
});

// ---------------------------------------------------------------------
// (c) Source diff against the vendored kernel snapshot
// ---------------------------------------------------------------------

// String-aware comment stripper: comment markers inside string or
// template literals are data, not comments, and escaped quotes do
// not end a string.
function stripComments(src) {
  let out = "";
  let i = 0;
  let state = "code";
  let quote = "";
  while (i < src.length) {
    const c = src[i];
    const next = i + 1 < src.length ? src[i + 1] : "";
    if (state === "code") {
      if (c === "/" && next === "/") {
        state = "line";
        out += " ";
        i += 2;
      } else if (c === "/" && next === "*") {
        state = "block";
        out += " ";
        i += 2;
      } else if (c === '"' || c === "'" || c === "`") {
        state = "string";
        quote = c;
        out += c;
        i += 1;
      } else {
        out += c;
        i += 1;
      }
    } else if (state === "line") {
      if (c === "\n") {
        state = "code";
        out += c;
      }
      i += 1;
    } else if (state === "block") {
      if (c === "*" && next === "/") {
        state = "code";
        i += 2;
      } else {
        i += 1;
      }
    } else {
      // string
      out += c;
      if (c === "\\") {
        if (i + 1 < src.length) {
          out += src[i + 1];
          i += 2;
        } else {
          i += 1;
        }
      } else if (c === quote) {
        state = "code";
        i += 1;
      } else {
        i += 1;
      }
    }
  }
  return out;
}

function normalizeSource(src) {
  return stripComments(src).replace(/\s+/g, " ").trim();
}

// Extract one top-level function declaration, from its "function"
// keyword through the matching closing brace. The brace scan is
// string- and comment-aware, so braces inside literals do not count.
function extractFunction(src, name) {
  const marker = `function ${name}(`;
  const start = src.indexOf(marker);
  if (start === -1) return null;
  let i = src.indexOf("{", start);
  if (i === -1) return null;
  let depth = 0;
  let state = "code";
  let quote = "";
  const begin = i;
  while (i < src.length) {
    const c = src[i];
    const next = i + 1 < src.length ? src[i + 1] : "";
    if (state === "code") {
      if (c === "/" && next === "/") {
        state = "line";
        i += 2;
        continue;
      }
      if (c === "/" && next === "*") {
        state = "block";
        i += 2;
        continue;
      }
      if (c === '"' || c === "'" || c === "`") {
        state = "string";
        quote = c;
        i += 1;
        continue;
      }
      if (c === "{") depth += 1;
      if (c === "}") {
        depth -= 1;
        if (depth === 0) return src.slice(start, i + 1);
      }
      i += 1;
    } else if (state === "line") {
      if (c === "\n") state = "code";
      i += 1;
    } else if (state === "block") {
      if (c === "*" && next === "/") {
        state = "code";
        i += 2;
      } else {
        i += 1;
      }
    } else {
      if (c === "\\") i += 2;
      else if (c === quote) {
        state = "code";
        i += 1;
      } else i += 1;
    }
  }
  return null;
}

test("stripper: a // inside a double-quoted string is kept, a real comment is removed", () => {
  const src = 'var u = "http://example.invalid/a"; // trailing note\nvar v = 1;';
  const out = stripComments(src);
  assert.ok(out.includes('"http://example.invalid/a"'));
  assert.ok(!out.includes("trailing note"));
  assert.ok(out.includes("var v = 1;"));
});

test("stripper: an escaped quote does not end the string, so a later // stays data", () => {
  const src = "var s = 'it\\'s // not a comment'; // real comment";
  const out = stripComments(src);
  assert.ok(out.includes("// not a comment"));
  assert.ok(!out.includes("real comment"));
});

test("stripper: block comments are removed and string contents survive", () => {
  const src = "a /* gone\nstill gone */ b; var t = `x /* kept */ y`;";
  const out = stripComments(src);
  assert.ok(!out.includes("still gone"));
  assert.ok(out.includes("`x /* kept */ y`"));
  assert.ok(/a\s+b;/.test(out));
});

const KERNEL_FUNCTIONS = [
  "fin",
  "f1s",
  "mergeHist",
  "deepCheckedCount",
  "evaluateH5",
  "computeInsider",
  "recordAppearances",
  "recordFlaggedCrew",
  "computeVerdict",
  "alertSentenceFor",
];

function moduleKernelRegion() {
  const text = fs.readFileSync(
    path.join(__dirname, "..", "..", "app", "evidence.js"),
    "utf8"
  );
  const begin = text.search(/BEGIN[^\n]*KERNEL/);
  const end = text.search(/END[^\n]*KERNEL/);
  if (begin !== -1 && end !== -1 && end > begin) return text.slice(begin, end);
  return text;
}

function snapshotText() {
  return fs.readFileSync(
    path.join(FIXTURES, "computeVerdict.original.js"),
    "utf8"
  );
}

test("diff: kernel markers are present in the evidence module", () => {
  const text = fs.readFileSync(
    path.join(__dirname, "..", "..", "app", "evidence.js"),
    "utf8"
  );
  assert.ok(/BEGIN[^\n]*KERNEL/.test(text), "BEGIN kernel marker");
  assert.ok(/END[^\n]*KERNEL/.test(text), "END kernel marker");
});

for (const name of KERNEL_FUNCTIONS) {
  test(`diff: kernel function ${name} matches the vendored snapshot`, () => {
    const fromModule = extractFunction(moduleKernelRegion(), name);
    const fromSnapshot = extractFunction(snapshotText(), name);
    assert.ok(fromModule, `${name} not found in the evidence module kernel`);
    assert.ok(fromSnapshot, `${name} not found in the vendored snapshot`);
    const a = normalizeSource(fromModule);
    const b = normalizeSource(fromSnapshot);
    if (a !== b) {
      let at = 0;
      while (at < Math.min(a.length, b.length) && a[at] === b[at]) at += 1;
      assert.fail(
        `${name} differs at offset ${at}:\nmodule:   ...${a.slice(Math.max(0, at - 80), at + 80)}...\nsnapshot: ...${b.slice(Math.max(0, at - 80), at + 80)}...`
      );
    }
  });
}

// ---------------------------------------------------------------------
// (d) Corpus comparison against the original kernel in a vm context
// ---------------------------------------------------------------------

function makeVmContext() {
  const ctx = {
    SESSION_FLAGGED_WALLETS: {},
    SESSION_WALLET_APPEARANCES: {},
    SESSION_FLAGGED_ORDER: [],
    LAUNCH_ORDER: [],
    LAUNCHES: {},
    CAPS: { h5Wallets: 1000 },
    EVICTED: { h5: 0 },
    console,
  };
  vm.createContext(ctx);
  vm.runInContext(snapshotText(), ctx);
  return ctx;
}

const vmCtx = makeVmContext();

const HAND_INSIDER = {
  proven: 0,
  inferred: 0,
  suspect: 0,
  unresolved: 0,
  boughtAtLaunch: 0,
  sniperPct: 0,
  wallets: [],
  inferredWallets: [],
  sniperWallets: [],
};

function runOriginal(evidence, moduleResult) {
  const e = {
    mint: evidence.mint,
    light: structuredClone(evidence.light),
    deep: structuredClone(evidence.deep),
    insider: structuredClone(HAND_INSIDER),
  };
  e.deep.combinedHistory = vmCtx.mergeHist(
    e.deep.history,
    e.deep.funderHistory
  );
  const h5Fired =
    moduleResult.primaryHardTrigger === 5 ||
    moduleResult.hardTriggers.some((t) => t.id === 5 && t.fired);
  e.deep.h5 = h5Fired;
  e.deep.h5Wallets = h5Fired ? ["CrewWa11et1111111111111111111111111111111111"] : [];
  if (
    Array.isArray(moduleResult.missingCore) &&
    moduleResult.missingCore.some((m) => /insider/i.test(String(m)))
  ) {
    e.insider = null;
  }
  vmCtx.computeVerdict(e);
  // Rebuild the result with this realm's prototypes: the vm context
  // has its own Object and Array prototypes, which deepStrictEqual
  // would otherwise reject even when every value matches.
  return {
    verdict: String(e.verdict),
    hard: Array.from(e.hard, (h) => String(h)),
    score: {
      total: Number(e.score.total),
      famsHit: Number(e.score.famsHit),
      fam: {
        bundle: Number(e.score.fam.bundle),
        side: Number(e.score.fam.side),
        history: Number(e.score.fam.history),
        structure: Number(e.score.fam.structure),
      },
    },
  };
}

function compareCase(label, evidence) {
  const moduleResult = ev.evaluateEvidence(
    structuredClone(evidence),
    ev.emptyCrewStore()
  );
  const original = runOriginal(evidence, moduleResult);
  const mapped = {
    verdict: moduleResult.displayVerdict,
    hard: firedNames(moduleResult),
    total: totalOf(moduleResult),
    fam: moduleResult.families,
    famsHit: moduleResult.familiesHit,
  };
  const expected = {
    verdict: original.verdict,
    hard: original.hard,
    total: original.score.total,
    fam: original.score.fam,
    famsHit: original.score.famsHit,
  };
  assert.deepEqual(
    mapped,
    expected,
    `${label} mismatch on input ${JSON.stringify(evidence)}`
  );
}

function corpusEvidence(mutate) {
  const evidence = loadEvidence("clean");
  mutate(evidence);
  return evidence;
}

function sideWalletStrings(n) {
  const out = [];
  for (let i = 0; i < n; i += 1) {
    out.push(`SideWa11et${String.fromCharCode(65 + i)}111111111111111111111111111111`);
  }
  return out;
}

test("corpus: bundle pct x coordination x count grid", () => {
  const pcts = [0, 9.9, 10, 19.9, 20, 29.9, 30, 39.9, 40, 55];
  const coords = ["none", "sharedFunder", "creatorInWindow", "matching"];
  const counts = [0, 7, 8];
  let idx = 0;
  for (const pct of pcts) {
    for (const coord of coords) {
      for (const count of counts) {
        idx += 1;
        const evidence = corpusEvidence((e) => {
          e.deep.bundle.pct = pct;
          e.deep.bundle.count = count;
          e.deep.funders.sharedFunderInWindow = coord === "sharedFunder";
          e.deep.bundle.creatorInWindow = coord === "creatorInWindow";
          e.deep.bundle.matching = coord === "matching";
        });
        try {
          compareCase(`bundle[${idx}]`, evidence);
        } catch (err) {
          err.message = `bundle case ${idx} (pct=${pct}, coord=${coord}, count=${count}): ${err.message}`;
          throw err;
        }
      }
    }
  }
});

test("corpus: sells soldPct x checked grid", () => {
  let idx = 0;
  for (const soldPct of [0, 89.9, 90, 100]) {
    for (const checked of [true, false]) {
      idx += 1;
      const evidence = corpusEvidence((e) => {
        e.deep.sells = {
          soldPct,
          initial: 100,
          checked,
          crossingTs: checked ? CREATED + 7200 : null,
        };
      });
      try {
        compareCase(`sells[${idx}]`, evidence);
      } catch (err) {
        err.message = `sells case ${idx} (soldPct=${soldPct}, checked=${checked}): ${err.message}`;
        throw err;
      }
    }
  }
});

test("corpus: liquidity states", () => {
  let idx = 0;
  for (const state of ["live", "curve", "gone", "unknown"]) {
    idx += 1;
    const evidence = corpusEvidence((e) => {
      e.deep.liquidity = { state, usd: state === "unknown" ? null : 5000 };
    });
    try {
      compareCase(`liquidity[${idx}]`, evidence);
    } catch (err) {
      err.message = `liquidity case ${idx} (state=${state}): ${err.message}`;
      throw err;
    }
  }
});

test("corpus: side wallet counts", () => {
  let idx = 0;
  for (const n of [0, 2, 3, 5, 6, 10, 11]) {
    idx += 1;
    const evidence = corpusEvidence((e) => {
      e.deep.funders.linkedOutside = sideWalletStrings(n);
    });
    try {
      compareCase(`side[${idx}]`, evidence);
    } catch (err) {
      err.message = `side case ${idx} (n=${n}): ${err.message}`;
      throw err;
    }
  }
});

test("corpus: holder structure top10Excl grid", () => {
  let idx = 0;
  for (const top10Excl of [0, 39.9, 40, 59.9, 60]) {
    idx += 1;
    const evidence = corpusEvidence((e) => {
      e.light.top10Excl = top10Excl;
    });
    try {
      compareCase(`structure[${idx}]`, evidence);
    } catch (err) {
      err.message = `structure case ${idx} (top10Excl=${top10Excl}): ${err.message}`;
      throw err;
    }
  }
});

test("corpus: creator history tuples cover every soft branch and both H1 paths", () => {
  const tuples = [
    // Rate exactly 0.5 with strikes >= 3: falls through every soft
    // branch to 0, but fires H1 on the rate path.
    { launches: 5, determined: 6, strikes: 3, fastExit: 0, ageDays: 45, ageIsFloor: false },
    // Soft rate branch, 12 points.
    { launches: 6, determined: 8, strikes: 2, fastExit: 0, ageDays: 45, ageIsFloor: false },
    // Soft rate branch at the 0.2 boundary.
    { launches: 5, determined: 10, strikes: 2, fastExit: 0, ageDays: 45, ageIsFloor: false },
    // Launches 3-4 with a strike, 8 points.
    { launches: 3, determined: 3, strikes: 1, fastExit: 0, ageDays: 45, ageIsFloor: false },
    { launches: 4, determined: 4, strikes: 2, fastExit: 0, ageDays: 45, ageIsFloor: false },
    // Strikes 1-2 outside the other branches, 10 points.
    { launches: 2, determined: 2, strikes: 1, fastExit: 0, ageDays: 45, ageIsFloor: false },
    { launches: 2, determined: 3, strikes: 2, fastExit: 0, ageDays: 45, ageIsFloor: false },
    // No strikes at all.
    { launches: 10, determined: 10, strikes: 0, fastExit: 0, ageDays: 45, ageIsFloor: false },
    // High-volume rate path to H1, soft 0.
    { launches: 10, determined: 10, strikes: 5, fastExit: 0, ageDays: 45, ageIsFloor: false },
    // Absolute H1 path: fastExit >= 3 with no strikes.
    { launches: 2, determined: 2, strikes: 0, fastExit: 3, ageDays: 45, ageIsFloor: false },
    // Age bonus under 7 days.
    { launches: 6, determined: 8, strikes: 2, fastExit: 0, ageDays: 5, ageIsFloor: false },
    // Age bonus under 30 days.
    { launches: 6, determined: 8, strikes: 2, fastExit: 0, ageDays: 20, ageIsFloor: false },
    // Age is a floor: no bonus even when young.
    { launches: 6, determined: 8, strikes: 2, fastExit: 0, ageDays: 5, ageIsFloor: true },
    // Rate just below 0.5: soft branch, no H1.
    { launches: 5, determined: 51, strikes: 25, fastExit: 0, ageDays: 45, ageIsFloor: false },
    // Unknown history with nothing determined.
    { launches: 0, determined: 0, strikes: 0, fastExit: 0, ageDays: 45, ageIsFloor: false, unknown: true },
  ];
  tuples.forEach((tuple, i) => {
    const evidence = corpusEvidence((e) => {
      e.deep.history = {
        launches: tuple.launches,
        inactive: 0,
        determined: tuple.determined,
        strikes: tuple.strikes,
        fastExit: tuple.fastExit,
        sampled: tuple.determined,
        sigCount: 10,
        ageDays: tuple.ageDays,
        ageIsFloor: tuple.ageIsFloor,
        unknown: tuple.unknown === true,
        details: [],
      };
    });
    try {
      compareCase(`history[${i + 1}]`, evidence);
    } catch (err) {
      err.message = `history case ${i + 1} (${JSON.stringify(tuple)}): ${err.message}`;
      throw err;
    }
  });
});

test("corpus: each core family nulled in turn", () => {
  const cases = [
    ["bundle-null", (e) => { e.deep.bundle = null; }],
    ["funders-null", (e) => { e.deep.funders = null; }],
    ["history-null", (e) => { e.deep.history = null; e.deep.funderHistory = null; }],
    ["insider-null", (e) => { e.light.total = "0"; }],
    ["bundle-partial", (e) => { e.deep.bundle.partial = true; }],
  ];
  cases.forEach(([label, mutate], i) => {
    const evidence = corpusEvidence(mutate);
    try {
      compareCase(`null-core[${i + 1}]`, evidence);
    } catch (err) {
      err.message = `null-core case ${i + 1} (${label}): ${err.message}`;
      throw err;
    }
  });
});

// ---------------------------------------------------------------------
// (e) Snapshot freshness (env-gated)
// ---------------------------------------------------------------------

test("freshness: vendored snapshot still matches the source file when SOLANA_OS_HTML is set", () => {
  const sourcePath = process.env.SOLANA_OS_HTML;
  if (!sourcePath) {
    console.log(
      "skip: SOLANA_OS_HTML is not set, snapshot freshness not checked"
    );
    assert.ok(true);
    return;
  }
  const source = fs.readFileSync(sourcePath, "utf8");
  for (const name of KERNEL_FUNCTIONS) {
    const fromSource = extractFunction(source, name);
    const fromSnapshot = extractFunction(snapshotText(), name);
    assert.ok(fromSource, `${name} not found in ${sourcePath}`);
    assert.ok(fromSnapshot, `${name} not found in the snapshot`);
    assert.equal(
      normalizeSource(fromSource),
      normalizeSource(fromSnapshot),
      `${name} has drifted from the snapshot`
    );
  }
});
