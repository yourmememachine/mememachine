"use strict";

// Tests for the generated RugCall judge page (rugcall/app/judge.html,
// produced by rugcall/app/build-judge-page.js) and its binding to
// rugcall/app/replay.json. The page must be fully self-contained,
// its embedded replay must be the replay on disk, the program
// placeholder treatment must follow program_id, the inlined
// evidence module must be byte-identical to its source, and the
// frozen honesty copy must be present without overclaims.

const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const APP_DIR = path.join(__dirname, "..", "..", "app");
const JUDGE_HTML = path.join(APP_DIR, "judge.html");
const REPLAY_JSON = path.join(APP_DIR, "replay.json");
const GENERATOR = path.join(APP_DIR, "build-judge-page.js");
const EVIDENCE_JS = path.join(APP_DIR, "evidence.js");
const PLACEHOLDER = "Program ID: not yet deployed (devnet funding pending)";
const STUB_PROGRAM_ID = "StubProgram1111111111111111111111111111111";
// The program was deployed to devnet on 2026-11-20; the replay
// binding and the generated page carry this id.
const DEPLOYED_PROGRAM_ID = "DcwvEDi4yCk16wQ2HVzc3dQiBFccj8VxzboGUYdUXseZ";

function readJudge() {
  assert.ok(
    fs.existsSync(JUDGE_HTML),
    "judge.html is missing - run: node rugcall/app/build-judge-page.js " +
      "(generation needs predict.js, predict-tx.js, predict-wallet.js, " +
      "replay.json and the app fixture bundle to have landed)"
  );
  return fs.readFileSync(JUDGE_HTML, "utf8");
}

function loadReplay() {
  return JSON.parse(fs.readFileSync(REPLAY_JSON, "utf8"));
}

function embeddedJson(html, id) {
  const re = new RegExp(
    '<script type="application/json" id="' + id + '">([\\s\\S]*?)</script>'
  );
  const m = html.match(re);
  assert.ok(m, "embedded JSON block #" + id + " is present");
  return JSON.parse(m[1]);
}

function explorerHrefs(html) {
  return html.match(/href="https:\/\/explorer\.solana\.com\/address\/[^"]+"/g) || [];
}

function programSection(html) {
  const m = html.match(/<div id="s5-program-body">([\s\S]*?)<\/div>/);
  assert.ok(m, "program section is present");
  return m[1];
}

test("judge page: generated page is fully self-contained", () => {
  const html = readJudge();
  assert.ok(!/<script[^>]*\ssrc\s*=/i.test(html), "no script src references remain");
  assert.ok(!/fetch\(\s*["']\.\//.test(html), "no local fetch targets remain");
  assert.ok(!/(src|href)\s*=\s*["']\.\//i.test(html), "no relative src/href references remain");
  assert.ok(!/href\s*=\s*["']#["']/.test(html), "no placeholder hash links");
  assert.ok(!/https:\/\/explorer\.solana\.com\/address\/null/.test(html), "no explorer URL built from a null id");
  // The only external hrefs allowed are devnet explorer links, and
  // those exist only when a program id is deployed.
  const hrefs = html.match(/href="https?:\/\/[^"]+"/g) || [];
  for (const h of hrefs) {
    assert.ok(
      h.startsWith('href="https://explorer.solana.com/address/') && h.includes("?cluster=devnet"),
      "external href is a devnet explorer link: " + h
    );
  }
});

test("judge page: embedded replay deep-equals replay.json on disk", () => {
  const html = readJudge();
  const embedded = embeddedJson(html, "rugcall-embedded-replay");
  assert.deepEqual(embedded, loadReplay());
});

test("judge page: deployed treatment follows program_id", () => {
  const html = readJudge();
  const replay = loadReplay();
  const programId = (replay.config && replay.config.programId) || null;
  assert.equal(
    programId,
    DEPLOYED_PROGRAM_ID,
    "the replay binding carries the deployed devnet program id"
  );
  assert.ok(
    !programSection(html).includes(PLACEHOLDER),
    "not-yet-deployed placeholder text is gone from the program section"
  );
  assert.ok(
    html.includes("Program " + DEPLOYED_PROGRAM_ID + " · Devnet"),
    "program status chip carries the deployed treatment"
  );
  // War-room fix (X3): only the program links to the explorer. The
  // replay market and the resolver are fixture (replay record)
  // values, rendered as labelled plain text — linking them asserted
  // on-chain accounts that do not exist.
  const expectedLinks = 1;
  const hrefs = explorerHrefs(html);
  assert.equal(hrefs.length, expectedLinks, "exactly the expected devnet explorer links appear");
  assert.ok(
    programSection(html).includes("fixture (replay record) value, not an on-chain account"),
    "fixture market/resolver values are labelled as fixture, not linked"
  );
  for (const h of hrefs) {
    assert.ok(h.endsWith('?cluster=devnet"'), "explorer link is pinned to devnet: " + h);
  }
  assert.ok(
    hrefs.some((h) => h.includes("/address/" + DEPLOYED_PROGRAM_ID + "?cluster=devnet")),
    "the deployed program id has its own explorer link"
  );

  // Regenerate with a stub program id into a temp output: exactly
  // the program, market and resolver links appear, all devnet.
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "rugcall-judge-test-"));
  const tmpOut = path.join(tmpDir, "judge-stub.html");
  execFileSync(process.execPath, [GENERATOR, "--out", tmpOut, "--program-id", STUB_PROGRAM_ID], {
    stdio: "pipe",
  });
  const stub = fs.readFileSync(tmpOut, "utf8");
  const expected = 1;
  const stubHrefs = explorerHrefs(stub);
  assert.equal(stubHrefs.length, expected, "exactly the expected devnet explorer links appear");
  for (const h of stubHrefs) {
    assert.ok(h.endsWith('?cluster=devnet"'), "explorer link is pinned to devnet: " + h);
  }
  assert.ok(
    stubHrefs.some((h) => h.includes("/address/" + STUB_PROGRAM_ID + "?cluster=devnet")),
    "the stub program id has its own explorer link"
  );
  assert.ok(!programSection(stub).includes(PLACEHOLDER), "placeholder text is gone from the program section once deployed");
  assert.ok(
    stub.includes("Program " + STUB_PROGRAM_ID),
    "program status chip carries the stub program id once deployed"
  );
});

test("judge page: inlined evidence.js is byte-identical to its source", () => {
  const html = readJudge();
  const m = html.match(/<script data-inline="evidence\.js">([\s\S]*?)<\/script>/);
  assert.ok(m, "inlined evidence.js block is present");
  const source = fs.readFileSync(EVIDENCE_JS, "utf8");
  assert.equal(m[1], source, "inlined evidence.js matches the source byte for byte");
  const sourceHash = crypto.createHash("sha256").update(fs.readFileSync(EVIDENCE_JS)).digest("hex");
  const inlineHash = crypto.createHash("sha256").update(m[1], "utf8").digest("hex");
  assert.equal(inlineHash, sourceHash, "sha256 of the inlined module matches the source file");
});

test("judge page: frozen honesty copy is present, without overclaims", () => {
  const html = readJudge();
  const flat = html.replace(/\s+/g, " ");
  assert.ok(/hard triggers H1/.test(flat) && /only/.test(flat), "hard-triggers-only rule is stated");
  assert.ok(
    flat.includes("Soft scores never resolve a market"),
    "soft scores never resolve a market"
  );
  // War-room fix (X4): the standalone page serves recorded
  // snapshots; the frozen wording must say so, not claim live reads.
  assert.ok(
    flat.includes("The money is devnet. The evidence shown on this page is recorded snapshots, not live reads."),
    "devnet/recorded-snapshot split is stated in the frozen wording"
  );
  assert.ok(/Evidence shown: recorded snapshots/.test(flat) && /Markets: Devnet/.test(flat), "dual-cluster header copy is present");
  assert.ok(!/one code path/i.test(flat), "no shared-code-path claim");
  assert.ok(
    !/Solana OS (now )?(uses|consumes|loads) evidence\.js/i.test(flat),
    "no claim that the OS consumes the evidence module"
  );
  assert.ok(
    flat.includes("does not consume this module yet") || flat.includes("does not consume") ,
    "the page states the current non-consumption honestly"
  );
});
