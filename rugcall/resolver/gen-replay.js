"use strict";

// RugCall replay generator.
//
// Builds the deterministic replay artifact the app ships with: the
// H2 fixture market, cloned with the standard pools from the
// program's program-test vector, is run through the resolver's
// decision path at a fixed clock; the evidence bundle it produces
// is verified, published next to the other bundles, and embedded in
// rugcall/app/replay.json together with the claim amounts the
// program's parimutuel rule pays out and per-market evidence
// summaries computed by the same evidence module the app runs.
// Two runs over the same inputs produce byte-identical output.

const fs = require("fs");
const os = require("os");
const path = require("path");
const crypto = require("crypto");

const ev = require("../app/evidence.js");
const { createResolver, DEFAULT_RESOLVER_PUBKEY } = require("./resolver.js");

const H2_MARKET_ADDRESS = "5z1WF2nXLdx9UGJTnQy7VGZqAzLoeha5m2SNQUxXHG2B";
const FIXED_NOW = 1799920800;
const EXPECTED_TRIGGER_EVENT_TS = 1799913660;
const STD3_FLAGGED_POOL = "3000000";
const STD3_SURVIVES_POOL = "7000000";

const BUYS = [
  { wallet: "Alice", side: 1, stake: "1000000" },
  { wallet: "Bob", side: 1, stake: "2000000" },
  { wallet: "Carol", side: 2, stake: "7000000" },
];

// Parimutuel claim for a winning stake: floor(stake * totalPool /
// winningPool), in BigInt so the amounts are exact at any scale. A
// zero winning pool pays nothing.
function computeClaim(stake, totalPool, winningPool) {
  const stakeBn = BigInt(stake);
  const totalBn = BigInt(totalPool);
  const winningBn = BigInt(winningPool);
  if (winningBn === 0n) return 0n;
  return (stakeBn * totalBn) / winningBn;
}

function sha256Hex(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function readJsonFile(filePath, label) {
  let raw;
  try {
    raw = fs.readFileSync(filePath, "utf8");
  } catch (err) {
    throw new Error(`cannot read ${label} at ${filePath}: ${err.message}`);
  }
  try {
    return JSON.parse(raw);
  } catch (err) {
    throw new Error(`invalid JSON in ${label} at ${filePath}: ${err.message}`);
  }
}

function writeFileAtomic(filePath, data) {
  const dir = path.dirname(filePath);
  fs.mkdirSync(dir, { recursive: true });
  const tempPath = path.join(
    dir,
    `.${path.basename(filePath)}.tmp-${process.pid}`
  );
  fs.writeFileSync(tempPath, data);
  fs.renameSync(tempPath, filePath);
}

function generateReplay(options) {
  const opts = options || {};
  const marketsPath =
    opts.marketsPath || path.join(__dirname, "test", "fixtures", "markets.json");
  const evidenceDir =
    opts.evidenceDir ||
    path.join(__dirname, "test", "fixtures", "evidence");
  const outPath = opts.outPath || null;
  const bundleOutDir = opts.bundleOutDir || null;

  const marketsRaw = fs.readFileSync(marketsPath);
  const marketsObj = JSON.parse(marketsRaw.toString("utf8"));
  const template = (marketsObj.markets || []).find(
    (m) => m.address === H2_MARKET_ADDRESS
  );
  if (!template) {
    throw new Error(`markets file has no market ${H2_MARKET_ADDRESS}`);
  }
  // The golden fixture file is never edited: the standard pools are
  // set on this clone only.
  const market = structuredClone(template);
  market.flagged_pool = STD3_FLAGGED_POOL;
  market.survives_pool = STD3_SURVIVES_POOL;
  const singleMarketObj = { ...marketsObj, markets: [market] };

  const tempOutDir = fs.mkdtempSync(path.join(os.tmpdir(), "rugcall-replay-"));
  try {
    const collected = [];
    const resolver = createResolver({
      resolverPubkey: DEFAULT_RESOLVER_PUBKEY,
      now: FIXED_NOW,
      outDir: tempOutDir,
      evidenceDir,
      crewStore: ev.emptyCrewStore(),
      sink: (record) => collected.push(record),
    });
    const records = resolver.runPass(singleMarketObj);
    const resolves = records.filter((r) => r.action === "WOULD_RESOLVE");
    if (resolves.length !== 1) {
      throw new Error(
        `expected exactly one WOULD_RESOLVE record, got ${resolves.length}`
      );
    }
    const record = resolves[0];
    if (
      record.outcome !== 1 ||
      record.hard_trigger !== 2 ||
      record.trigger_event_ts !== EXPECTED_TRIGGER_EVENT_TS
    ) {
      throw new Error(
        `unexpected resolution: outcome ${record.outcome}, trigger ${record.hard_trigger}, trigger_event_ts ${record.trigger_event_ts}`
      );
    }

    const bundleBytes = fs.readFileSync(record.bundle_path);
    const bundle = JSON.parse(bundleBytes.toString("utf8"));
    const verify = ev.verifyBundle(bundle, record.evidence_hash);
    if (!verify.hashOk || !verify.verdictOk) {
      throw new Error(
        `bundle verification failed: hashOk ${verify.hashOk}, verdictOk ${verify.verdictOk}`
      );
    }

    const flaggedPool = BigInt(market.flagged_pool);
    const survivesPool = BigInt(market.survives_pool);
    const totalPool = flaggedPool + survivesPool;
    const winningSide = record.outcome;
    const winningPool = winningSide === 1 ? flaggedPool : survivesPool;
    let paidOut = 0n;
    const claims = BUYS.map((buy) => {
      if (buy.side === winningSide) {
        const payout = computeClaim(buy.stake, totalPool, winningPool);
        paidOut += payout;
        return {
          wallet: buy.wallet,
          side: buy.side,
          stake: buy.stake,
          payout: payout.toString(),
        };
      }
      return {
        wallet: buy.wallet,
        side: buy.side,
        stake: buy.stake,
        payout: "0",
        program_error: 19,
        program_error_name: "NoClaimableStake",
      };
    });
    const dust = (totalPool - paidOut).toString();

    const summaries = {};
    for (const fixtureMarket of marketsObj.markets || []) {
      const evidence = readJsonFile(
        path.join(evidenceDir, fixtureMarket.evidence_file),
        `evidence snapshot for market ${fixtureMarket.address}`
      );
      const result = ev.evaluateEvidence(evidence, ev.emptyCrewStore());
      summaries[fixtureMarket.address] = {
        verdict: result.displayVerdict,
        hardTriggers: result.hardTriggers
          .filter((t) => t.fired)
          .map((t) => t.id),
        partial: result.partial,
        resolutionBlockers: result.resolutionBlockers,
      };
    }

    const evidenceFixturePath = path.join(evidenceDir, template.evidence_file);
    const replayRecord = {
      ...record,
      bundle_path: `rugcall/evidence/${market.address}.json`,
    };
    const replay = {
      schema: "rugcall-replay/v1",
      generatedFrom: {
        evidenceJsSha256: sha256Hex(
          fs.readFileSync(path.join(__dirname, "..", "app", "evidence.js"))
        ),
        resolverJsSha256: sha256Hex(
          fs.readFileSync(path.join(__dirname, "resolver.js"))
        ),
        marketsSha256: sha256Hex(marketsRaw),
        evidenceFixtureSha256: sha256Hex(
          fs.readFileSync(evidenceFixturePath)
        ),
      },
      config: {
        programId:
          marketsObj.program_id !== undefined ? marketsObj.program_id : null,
        cluster: "devnet",
      },
      market,
      buys: BUYS.map((b) => ({ ...b })),
      record: replayRecord,
      bundle: {
        canonical: bundleBytes.toString("utf8"),
        sha256: sha256Hex(bundleBytes),
      },
      verify: { hashOk: verify.hashOk, verdictOk: verify.verdictOk },
      claims,
      dust,
      summaries,
      notes: [
        "Pools are claim state from the program's program-test vector (setup_std3); the resolver's decision path does not read pool fields.",
        "Carol's survives stake is on the losing side of a Flagged resolution: her claim returns program error 19 (NoClaimableStake) and her payout is 0.",
        "Floor rounding in the winning claims strands the dust amount in the vault; there is no sweep instruction in v1.",
        "record.bundle_path names the published bundle location under rugcall/evidence/ rather than the temporary build directory, so regeneration is byte-identical.",
      ],
    };

    // Nothing is written before the bundle has verified (above).
    if (bundleOutDir) {
      writeFileAtomic(path.join(bundleOutDir, `${market.address}.json`), bundleBytes);
    }
    if (outPath) {
      writeFileAtomic(outPath, `${JSON.stringify(replay, null, 2)}\n`);
    }
    return replay;
  } finally {
    fs.rmSync(tempOutDir, { recursive: true, force: true });
  }
}

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith("--")) {
      throw new Error(`unexpected argument ${token}`);
    }
    const eq = token.indexOf("=");
    if (eq !== -1) {
      args[token.slice(2, eq)] = token.slice(eq + 1);
    } else {
      i += 1;
      if (i >= argv.length) throw new Error(`missing value for ${token}`);
      args[token.slice(2)] = argv[i];
    }
  }
  return args;
}

function main(argv) {
  let args;
  try {
    args = parseArgs(argv);
  } catch (err) {
    process.stderr.write(`${err.message}\n`);
    return 1;
  }
  try {
    generateReplay({
      marketsPath: args.markets,
      evidenceDir: args["evidence-dir"],
      outPath: args.out || path.join(__dirname, "..", "app", "replay.json"),
      bundleOutDir: args["bundle-out"] || path.join(__dirname, "..", "evidence"),
    });
  } catch (err) {
    process.stderr.write(`replay generation failed: ${err.message}\n`);
    return 1;
  }
  return 0;
}

if (require.main === module) {
  process.exitCode = main(process.argv.slice(2));
}

module.exports = {
  generateReplay,
  computeClaim,
  parimutuelPayout: computeClaim,
  H2_MARKET_ADDRESS,
  FIXED_NOW,
  EXPECTED_TRIGGER_EVENT_TS,
  BUYS,
};
