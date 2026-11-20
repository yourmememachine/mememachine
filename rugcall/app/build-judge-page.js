#!/usr/bin/env node
/*
 * RugCall judge page generator.
 *
 * Reads index.html (the dev shell) and writes a fully self-contained
 * judge.html:
 *   - every sibling script tag is replaced by the file's verbatim
 *     contents inlined in a classic script block,
 *   - replay.json and the app fixture bundle are embedded as JSON
 *     script blocks and assigned to window.RUGCALL_EMBEDDED,
 *   - the dev-only fetch fallback block is removed, so the output
 *     has no local fetch targets,
 *   - a meta tag stamps the sha256 of the source files,
 *   - the program status chip and the program/explorer section are
 *     rendered statically from the single program id binding.
 *
 * Usage:
 *   node build-judge-page.js [--out <path>] [--program-id <id>]
 *                            [--index <path>] [--replay <path>]
 *                            [--fixture <path>]
 *
 * --program-id overrides the program id in the embedded copies and
 * in the statically rendered program section (used by tests to check
 * the deployed treatment without touching the real inputs).
 *
 * Exits non-zero when any required input is missing, when an inlined
 * script contains the byte sequence that would close a script block,
 * or when the output would still reference a sibling script or a
 * local fetch target.
 */
"use strict";

const fs = require("fs");
const path = require("path");
const crypto = require("crypto");

const SCRIPT_FILES = ["evidence.js", "predict-tx.js", "predict.js", "predict-wallet.js", "predict-ui.js"];
const CHIP_NOT_DEPLOYED = "Program not yet deployed — this page replays fixture markets through the real resolver";
const PLACEHOLDER_TEXT = "Program ID: not yet deployed (devnet funding pending)";

function sha256Hex(buf) {
  return crypto.createHash("sha256").update(buf).digest("hex");
}

function fail(message) {
  process.stderr.write("build-judge-page: " + message + "\n");
  process.exit(1);
}

function parseArgs(argv) {
  const args = { out: null, programId: undefined, index: null, replay: null, fixture: null };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--out") args.out = argv[++i];
    else if (a === "--program-id") args.programId = argv[++i];
    else if (a === "--index") args.index = argv[++i];
    else if (a === "--replay") args.replay = argv[++i];
    else if (a === "--fixture") args.fixture = argv[++i];
    else if (a === "--help") {
      process.stdout.write("Usage: node build-judge-page.js [--out <path>] [--program-id <id>] [--index <path>] [--replay <path>] [--fixture <path>]\n");
      process.exit(0);
    } else {
      fail("unknown argument: " + a);
    }
  }
  return args;
}

function jsonForEmbed(obj) {
  return JSON.stringify(obj).replace(/<\//g, "<\\/");
}

function fieldOf(obj, names) {
  if (!obj) return undefined;
  for (const n of names) {
    if (typeof obj[n] !== "undefined" && obj[n] !== null) return obj[n];
  }
  return undefined;
}

function explorerHref(id) {
  return "https://explorer.solana.com/address/" + id + "?cluster=devnet";
}

/* Statically rendered program section. Exactly one link per known
   account: the program, plus the replay market and the resolver when
   the replay record names them. No nested div elements, so the dev
   shell markup can be replaced with a simple span. */
function renderProgramSection(programId, replay) {
  if (!programId) {
    return '<p class="program-placeholder">' + PLACEHOLDER_TEXT + "</p>" +
      '<p>Explorer entries stay disabled until a program id exists: ' +
      '<span class="explorer-disabled" aria-disabled="true">Program</span> · ' +
      '<span class="explorer-disabled" aria-disabled="true">Market</span> · ' +
      '<span class="explorer-disabled" aria-disabled="true">Resolver</span></p>';
  }
  const market = replay && replay.market
    ? (typeof replay.market === "string" ? replay.market : fieldOf(replay.market, ["address", "id"]))
    : null;
  const resolver = replay && replay.record ? fieldOf(replay.record, ["resolver"]) : null;
  let items = '<li><a href="' + explorerHref(programId) + '" rel="noopener">Program ' + programId + "</a></li>";
  if (market) items += '<li><a href="' + explorerHref(market) + '" rel="noopener">Market ' + market + "</a></li>";
  if (resolver) items += '<li><a href="' + explorerHref(resolver) + '" rel="noopener">Resolver ' + resolver + "</a></li>";
  return "<p>Program ID: <code>" + programId + "</code> (devnet)</p><ul>" + items + "</ul>";
}

/* Locate the evidence fixture the replay was generated from. Returns
   an absolute path or null. Candidates follow the replay shape:
   explicit fields first, then the market's evidence_file under the
   resolver fixtures, then the trigger's canonical fixture name. */
function resolveEvidenceFixture(appDir, replay, fixtureBundle) {
  const rugcallDir = path.dirname(appDir);
  const repoRoot = path.resolve(rugcallDir, "..");
  const evidenceDir = path.join(rugcallDir, "resolver", "test", "fixtures", "evidence");
  const candidates = [];
  const pushPath = (p) => { if (typeof p === "string" && p) candidates.push(p); };
  if (replay) {
    pushPath(replay.evidenceFixture);
    pushPath(replay.evidence_fixture);
    const gen = replay.generatedFrom;
    if (typeof gen === "string") pushPath(gen);
    if (gen && typeof gen === "object") {
      pushPath(gen.evidenceFixture);
      pushPath(gen.evidence_fixture);
      pushPath(gen.evidenceFile);
      pushPath(gen.evidence_file);
      pushPath(gen.evidence);
    }
    const marketFile = replay.market && typeof replay.market === "object"
      ? fieldOf(replay.market, ["evidence_file", "evidenceFile"]) : null;
    if (marketFile) candidates.push(path.join(evidenceDir, marketFile));
    const recordFile = replay.record ? fieldOf(replay.record, ["evidence_file", "evidenceFile"]) : null;
    if (recordFile) candidates.push(path.join(evidenceDir, recordFile));
    const trigger = replay.record
      ? fieldOf(replay.record, ["hard_trigger", "hardTrigger", "trigger"]) : null;
    if (trigger === 2 || trigger === "2") candidates.push(path.join(evidenceDir, "h2-bundle.json"));
  }
  if (fixtureBundle && Array.isArray(fixtureBundle.markets) && replay && replay.market) {
    const addr = typeof replay.market === "string" ? replay.market : fieldOf(replay.market, ["address", "id"]);
    const mint = typeof replay.market === "object" ? fieldOf(replay.market, ["subjectMint", "subject_mint"]) : null;
    for (const m of fixtureBundle.markets) {
      if (m && (m.address === addr || (mint && (m.subjectMint === mint || m.subject_mint === mint)))) {
        const f = fieldOf(m, ["evidence_file", "evidenceFile"]);
        if (f) candidates.push(path.join(evidenceDir, f));
      }
    }
  }
  for (const c of candidates) {
    const tries = [c, path.resolve(appDir, c), path.resolve(rugcallDir, c), path.resolve(repoRoot, c)];
    for (const t of tries) {
      if (fs.existsSync(t) && fs.statSync(t).isFile()) return t;
    }
  }
  return null;
}

function build(options) {
  const opts = options || {};
  const appDir = __dirname;
  const indexPath = opts.index || path.join(appDir, "index.html");
  const replayPath = opts.replay || path.join(appDir, "replay.json");
  const fixturePath = opts.fixture || path.join(appDir, "test", "fixtures", "app-markets.json");
  const outPath = opts.out || path.join(appDir, "judge.html");

  const required = [indexPath, replayPath, fixturePath].concat(
    SCRIPT_FILES.map((n) => path.join(appDir, n))
  );
  const missing = required.filter((p) => !fs.existsSync(p));
  if (missing.length) {
    fail("missing input(s): " + missing.join(", ") +
      " - judge.html generation waits until every input has landed.");
  }

  const scriptBytes = {};
  for (const name of SCRIPT_FILES) {
    const bytes = fs.readFileSync(path.join(appDir, name));
    const text = bytes.toString("utf8");
    if (/<\/script/i.test(text)) {
      fail("inlined script " + name + " contains the byte sequence that closes a script block; refusing to build.");
    }
    scriptBytes[name] = { bytes, text };
  }

  let replay = JSON.parse(fs.readFileSync(replayPath, "utf8"));
  let fixtureBundle = JSON.parse(fs.readFileSync(fixturePath, "utf8"));

  if (typeof opts.programId !== "undefined" && opts.programId !== null) {
    replay = JSON.parse(JSON.stringify(replay));
    fixtureBundle = JSON.parse(JSON.stringify(fixtureBundle));
    replay.config = replay.config || {};
    replay.config.programId = opts.programId;
    fixtureBundle.program_id = opts.programId;
  }

  const programId = (replay.config && replay.config.programId) ||
    fixtureBundle.program_id || null;

  let html = fs.readFileSync(indexPath, "utf8");

  /* Remove the dev-only fetch fallback: the generated page must have
     no local fetch targets. */
  const lines = html.split("\n");
  const kept = [];
  let skipping = false;
  let strippedFetch = false;
  for (const line of lines) {
    if (line.indexOf("DEV-FETCH-BEGIN") !== -1) { skipping = true; strippedFetch = true; continue; }
    if (line.indexOf("DEV-FETCH-END") !== -1) { skipping = false; continue; }
    if (!skipping) kept.push(line);
  }
  if (!strippedFetch) {
    process.stderr.write("build-judge-page: warning: no DEV-FETCH block found in index.html\n");
  }
  html = kept.join("\n");

  /* Inline each sibling script verbatim. */
  for (const name of SCRIPT_FILES) {
    const re = new RegExp('<script\\s+src="' + name.replace(".", "\\.") + '"\\s*>\\s*</script>');
    if (!re.test(html)) fail("index.html has no sibling script tag for " + name);
    const contents = scriptBytes[name].text;
    html = html.replace(re, () => '<script data-inline="' + name + '">' + contents + "</script>");
  }
  if (/<script[^>]*\ssrc\s*=/i.test(html)) {
    fail("output would still contain a script src reference; refusing to build.");
  }
  if (/fetch\(\s*["']\.\//.test(html)) {
    fail("output would still contain a local fetch target; refusing to build.");
  }

  /* Embed the replay and the fixture bundle. */
  if (html.indexOf("<!-- EMBEDDED-DATA -->") === -1) {
    fail("index.html has no EMBEDDED-DATA injection point.");
  }
  const embedded =
    '<script type="application/json" id="rugcall-embedded-replay">' + jsonForEmbed(replay) + "</script>\n" +
    '<script type="application/json" id="rugcall-embedded-fixtures">' + jsonForEmbed(fixtureBundle) + "</script>\n" +
    "<script>window.RUGCALL_EMBEDDED = { replay: JSON.parse(document.getElementById(\"rugcall-embedded-replay\").textContent), fixtureBundle: JSON.parse(document.getElementById(\"rugcall-embedded-fixtures\").textContent) };</script>";
  html = html.replace("<!-- EMBEDDED-DATA -->", () => embedded);

  /* Source hash stamp. */
  const evidenceFixturePath = resolveEvidenceFixture(appDir, replay, fixtureBundle);
  const hashes = {
    "evidence.js": sha256Hex(scriptBytes["evidence.js"].bytes),
    "predict.js": sha256Hex(scriptBytes["predict.js"].bytes),
    "predict-tx.js": sha256Hex(scriptBytes["predict-tx.js"].bytes),
    "predict-wallet.js": sha256Hex(scriptBytes["predict-wallet.js"].bytes),
    "predict-ui.js": sha256Hex(scriptBytes["predict-ui.js"].bytes),
    "app-markets.json": sha256Hex(fs.readFileSync(fixturePath)),
    "evidenceFixture": evidenceFixturePath ? sha256Hex(fs.readFileSync(evidenceFixturePath)) : null
  };
  if (!evidenceFixturePath) {
    process.stderr.write("build-judge-page: warning: could not locate the replay's evidence fixture; its hash is stamped as null\n");
  }
  const meta = '<meta name="rugcall-source-hashes" content=\'' + JSON.stringify(hashes) + "'>";
  if (html.indexOf("<!-- SOURCE-HASHES-META -->") !== -1) {
    html = html.replace("<!-- SOURCE-HASHES-META -->", () => meta);
  } else {
    html = html.replace(/<meta charset[^>]*>/, (m) => m + "\n" + meta);
  }

  /* Static program chip + program/explorer section. */
  const chipText = programId ? "Program " + programId + " · Devnet" : CHIP_NOT_DEPLOYED;
  html = html.replace(/(<span id="program-status-chip"[^>]*>)[\s\S]*?(<\/span>)/,
    (m, a, b) => a + chipText + b);
  html = html.replace(/(<div id="s5-program-body">)[\s\S]*?(<\/div>)/,
    (m, a, b) => a + renderProgramSection(programId, replay) + b);

  fs.writeFileSync(outPath, html);
  return { outPath, programId, hashes };
}

if (require.main === module) {
  const args = parseArgs(process.argv);
  const result = build({
    out: args.out ? path.resolve(args.out) : null,
    index: args.index ? path.resolve(args.index) : null,
    replay: args.replay ? path.resolve(args.replay) : null,
    fixture: args.fixture ? path.resolve(args.fixture) : null,
    programId: args.programId
  });
  process.stdout.write("build-judge-page: wrote " + result.outPath +
    " (programId: " + (result.programId || "null") + ")\n");
}

module.exports = { build, renderProgramSection, resolveEvidenceFixture, sha256Hex };
