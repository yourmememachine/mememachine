"use strict";

// RugCall dry-run resolver.
//
// Reads fixture markets and recorded evidence snapshots, evaluates
// each open market through the shared evidence module, writes the
// canonical evidence bundle for every market it would resolve, and
// logs the instruction it would post. It never signs, never submits,
// never reads a keypair file, and needs no chain RPC in fixture mode:
// every record carries signed:false and submitted:false.

const fs = require("fs");
const path = require("path");
const crypto = require("crypto");

const ev = require("../app/evidence.js");

const RESOLVE_ORDINAL = 0x03;
const CANCEL_ORDINAL = 0x05;
const OUTCOME_FLAGGED = 1;
const OUTCOME_SURVIVES = 2;
const STATE_OPEN = 1;
const STATE_RESOLVED = 2;
const STATE_CANCELLED = 3;
const GRACE_SEC = 86400;
const RETRY_SEC = 3600;
const MAINNET_RPC_URL = "https://api.mainnet-beta.solana.com";
// The resolver identity used by the command line when no explicit
// --resolver flag is given. It matches the resolver recorded in the
// fixture markets schema.
const DEFAULT_RESOLVER_PUBKEY =
  "Dm95oxQp8YghEsCads1SXWkGwy4BaYLjCYv4fieG7GtH";

let tempCounter = 0;

function writeFileAtomic(filePath, data) {
  const dir = path.dirname(filePath);
  fs.mkdirSync(dir, { recursive: true });
  tempCounter += 1;
  const tempPath = path.join(
    dir,
    `.${path.basename(filePath)}.tmp-${process.pid}-${tempCounter}`
  );
  fs.writeFileSync(tempPath, data);
  fs.renameSync(tempPath, filePath);
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

function isZeroHash(bytes) {
  return bytes.length > 0 && bytes.every((b) => b === 0);
}

function hashResultBytes(hashResult) {
  if (!hashResult) return Buffer.alloc(0);
  if (hashResult.bytes) return Buffer.from(hashResult.bytes);
  if (hashResult.hex) return Buffer.from(hashResult.hex, "hex");
  return Buffer.alloc(0);
}

function hashResultHex(hashResult, bytes) {
  if (hashResult && typeof hashResult.hex === "string") {
    return hashResult.hex.toLowerCase();
  }
  return bytes.toString("hex");
}

function compareAddress(a, b) {
  if (a.address < b.address) return -1;
  if (a.address > b.address) return 1;
  return 0;
}

function buildAccounts({ resolverPubkey, config, market }) {
  // Exact account order shared by ResolveMarket and CancelMarket.
  // In dry-run the CancelMarket caller is the resolver itself.
  return [
    { pubkey: resolverPubkey, signer: true, writable: false },
    { pubkey: config, signer: false, writable: false },
    { pubkey: market.address, signer: false, writable: true },
    { pubkey: market.live_market, signer: false, writable: true },
    { pubkey: market.creator, signer: false, writable: true },
  ];
}

function loadLedger(ledgerPath) {
  if (!ledgerPath) return {};
  if (!fs.existsSync(ledgerPath)) return {};
  const parsed = readJsonFile(ledgerPath, "processed ledger");
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`processed ledger at ${ledgerPath} is not a JSON object`);
  }
  return parsed;
}

function ledgerEntryIsFinal(entry) {
  return (
    entry &&
    typeof entry === "object" &&
    (entry.action === "WOULD_RESOLVE" || entry.action === "WOULD_CANCEL")
  );
}

function createResolver(options) {
  const opts = options || {};
  const resolverPubkey = opts.resolverPubkey;
  if (!resolverPubkey || typeof resolverPubkey !== "string") {
    throw new Error("createResolver requires a resolverPubkey string");
  }
  const hashFn = opts.hashFn || ev.hashBundle;
  const sink =
    opts.sink ||
    ((record) => {
      process.stdout.write(`${JSON.stringify(record)}\n`);
    });
  const passNowFixed = opts.now;
  const outDir = opts.outDir || path.join(process.cwd(), "rugcall-out");
  const ledgerPath = opts.ledgerPath || null;
  const evidenceDir = opts.evidenceDir || null;
  // Optional in-memory evidence, keyed by market address. The live
  // command line path fills this after fetching, so a live pass and a
  // fixture pass share the exact same decision code.
  const evidenceByMarket = opts.evidenceByMarket || null;
  const evidenceProvider = opts.evidenceProvider || null;

  let crewStore =
    opts.crewStore !== undefined ? opts.crewStore : ev.emptyCrewStore();

  function getCrewStore() {
    return crewStore;
  }

  function loadEvidence(market) {
    if (evidenceByMarket && evidenceByMarket[market.address]) {
      return evidenceByMarket[market.address];
    }
    if (evidenceProvider) {
      return evidenceProvider(market);
    }
    if (!evidenceDir) {
      throw new Error(
        `no evidence source configured for market ${market.address}`
      );
    }
    if (!market.evidence_file) {
      throw new Error(`market ${market.address} has no evidence_file`);
    }
    return readJsonFile(
      path.join(evidenceDir, market.evidence_file),
      `evidence snapshot for market ${market.address}`
    );
  }

  function runPass(marketsObj, nowOverride) {
    const passNow =
      nowOverride !== undefined ? nowOverride : passNowFixed;
    if (typeof passNow !== "number" || !Number.isFinite(passNow)) {
      throw new Error("runPass requires a fixed pass time (now, unix seconds)");
    }
    if (!marketsObj || typeof marketsObj !== "object") {
      throw new Error("runPass requires a markets object");
    }

    const records = [];
    const emit = (record) => {
      records.push(record);
      sink(record);
      return record;
    };

    const baseRecord = (market, action, decisionReason) => ({
      ts: passNow,
      action,
      market: market ? market.address : null,
      subject_mint: market ? market.subject_mint : null,
      creator: market ? market.creator : null,
      resolver: resolverPubkey,
      program_id:
        marketsObj.program_id !== undefined ? marketsObj.program_id : null,
      expiry: market ? market.expiry : null,
      decision_reason: decisionReason,
      qualifying_triggers: [],
      excluded_triggers: [],
      signed: false,
      submitted: false,
    });

    // Config / resolver guard. The program rejects posts from any key
    // other than its configured resolver, so a mismatch is a refusal
    // to evaluate anything at all, however strong the evidence looks.
    if (marketsObj.resolver !== resolverPubkey) {
      const record = baseRecord(null, "REFUSAL", "resolver-mismatch");
      record.reason = "resolver-mismatch";
      emit(record);
      return records;
    }

    const ledger = loadLedger(ledgerPath);
    let ledgerDirty = false;

    const markets = Array.isArray(marketsObj.markets)
      ? [...marketsObj.markets].sort(compareAddress)
      : [];

    for (const market of markets) {
      // Terminal fixture states need no evaluation.
      if (market.state === STATE_RESOLVED || market.state === STATE_CANCELLED) {
        emit(baseRecord(market, "SKIP_TERMINAL", "terminal-state"));
        continue;
      }

      // Processed ledger: a final decision is never revisited. An
      // entry that only carries a retry time is a deferral, and the
      // market is skipped only until that time is reached.
      const prior = ledger[market.address];
      if (prior && typeof prior === "object") {
        if (ledgerEntryIsFinal(prior)) {
          emit(baseRecord(market, "SKIP_LEDGER", "already-in-ledger"));
          continue;
        }
        if (
          typeof prior.next_retry_at === "number" &&
          passNow < prior.next_retry_at
        ) {
          const record = baseRecord(
            market,
            "SKIP_LEDGER",
            "deferred-retry-not-due"
          );
          record.next_retry_at = prior.next_retry_at;
          emit(record);
          continue;
        }
      }

      if (market.state !== STATE_OPEN) {
        const record = baseRecord(
          market,
          "WOULD_RESOLVE_INVALID",
          "preflight-failed"
        );
        record.reason = "preflight-failed";
        record.preflight_errors = [
          `market state ${market.state} is not Open`,
        ];
        emit(record);
        continue;
      }

      const evidence = loadEvidence(market);
      const crewStoreBefore = crewStore;
      const result = ev.evaluateEvidence(evidence, crewStoreBefore);
      // Thread the store across markets in address order, whatever
      // the decision for this market turns out to be.
      crewStore = result.crewStore || crewStoreBefore;

      const triggers = Array.isArray(result.hardTriggers)
        ? [...result.hardTriggers].sort((a, b) => a.id - b.id)
        : [];
      const qualifying = [];
      const excluded = [];
      for (const trigger of triggers) {
        if (!trigger || !trigger.fired) continue;
        if (
          typeof trigger.eventTimestamp === "number" &&
          trigger.eventTimestamp <= market.expiry
        ) {
          qualifying.push(trigger);
        } else if (
          typeof trigger.eventTimestamp === "number" &&
          trigger.eventTimestamp > market.expiry
        ) {
          excluded.push({
            id: trigger.id,
            eventTimestamp: trigger.eventTimestamp,
          });
        }
      }
      const primary = qualifying.length > 0 ? qualifying[0] : null;
      const complete = result.complete === true;

      const withTriggers = (record) => {
        record.qualifying_triggers = qualifying.map((t) => t.id);
        record.excluded_triggers = excluded;
        return record;
      };

      const deferOrCancel = () => {
        if (passNow <= market.expiry + GRACE_SEC) {
          const record = withTriggers(
            baseRecord(market, "DEFERRED_PARTIAL", "evidence-partial-within-grace")
          );
          record.next_retry_at = passNow + RETRY_SEC;
          ledger[market.address] = {
            ...(ledger[market.address] || {}),
            next_retry_at: passNow + RETRY_SEC,
          };
          ledgerDirty = true;
          emit(record);
        } else {
          emitCancel(market, "evidence-partial-after-grace", qualifying, excluded);
        }
      };

      const emitCancel = (mkt, reason, qual, excl) => {
        const data = Buffer.from([CANCEL_ORDINAL]);
        const record = baseRecord(mkt, "WOULD_CANCEL", reason);
        record.qualifying_triggers = qual.map((t) => t.id);
        record.excluded_triggers = excl;
        record.instruction = {
          name: "CancelMarket",
          ordinal: CANCEL_ORDINAL,
          data_hex: data.toString("hex"),
          data_len: data.length,
          accounts: buildAccounts({
            resolverPubkey,
            config: marketsObj.config,
            market: mkt,
          }),
        };
        record.outcome = null;
        record.hard_trigger = 0;
        record.evidence_hash = null;
        record.trigger_event_ts = null;
        record.bundle_path = null;
        record.bundle_bytes = 0;
        record.bundle_sha256 = null;
        ledger[mkt.address] = {
          action: "WOULD_CANCEL",
          outcome: null,
          hard_trigger: 0,
          evidence_hash: null,
          bundle_path: null,
          decided_at: passNow,
        };
        ledgerDirty = true;
        emit(record);
      };

      const emitResolve = (mkt, outcome, triggerId, triggerEventTs, reason) => {
        // Build the bundle from the pre-evaluation crew store so the
        // published bundle re-derives the same result on replay.
        const bundle = ev.buildBundle({
          market: mkt,
          subjectMint: mkt.subject_mint,
          resolver: resolverPubkey,
          expirySec: mkt.expiry,
          fetchedAtSec:
            evidence && typeof evidence.fetchedAtSec === "number"
              ? evidence.fetchedAtSec
              : passNow,
          evidence,
          crewStore: crewStoreBefore,
          result,
        });
        const hashResult = hashFn(bundle);
        const hashBytes = hashResultBytes(hashResult);
        const hashHex = hashResultHex(hashResult, hashBytes);

        // Zero-hash rail: the program refuses an all-zero evidence
        // hash, so a zero hash here blocks the post entirely and the
        // market is not marked resolved anywhere.
        if (isZeroHash(hashBytes) || /^0+$/.test(hashHex || "")) {
          const record = withTriggers(
            baseRecord(mkt, "BLOCKED_ZERO_HASH", "zero-evidence-hash")
          );
          record.reason = "zero-evidence-hash";
          record.evidence_hash = hashHex;
          emit(record);
          return;
        }

        // Local preflight mirroring the program checks. A failure
        // here is a defect in this resolver, not a market outcome.
        const errors = [];
        if (outcome !== OUTCOME_FLAGGED && outcome !== OUTCOME_SURVIVES) {
          errors.push(`outcome ${outcome} is not 1 or 2`);
        }
        if (
          outcome === OUTCOME_FLAGGED &&
          !(triggerId >= 1 && triggerId <= 5)
        ) {
          errors.push(`Flagged outcome requires a trigger in 1..5, got ${triggerId}`);
        }
        if (outcome === OUTCOME_SURVIVES && triggerId !== 0) {
          errors.push(`Survives outcome requires trigger 0, got ${triggerId}`);
        }
        if (hashBytes.length === 0 || isZeroHash(hashBytes)) {
          errors.push("evidence hash is all zero");
        }
        if (mkt.state !== STATE_OPEN) {
          errors.push(`market state ${mkt.state} is not Open`);
        }
        if (outcome === OUTCOME_SURVIVES && passNow < mkt.expiry) {
          errors.push("Survives outcome requires the market to be at or past expiry");
        }
        if (errors.length > 0) {
          const record = withTriggers(
            baseRecord(mkt, "WOULD_RESOLVE_INVALID", "preflight-failed")
          );
          record.reason = "preflight-failed";
          record.preflight_errors = errors;
          record.evidence_hash = hashHex;
          emit(record);
          return;
        }

        const canonical = ev.canonicalizeBundle(bundle);
        const canonicalBytes = Buffer.from(canonical, "utf8");
        const bundlePath = path.join(
          outDir,
          "evidence",
          `${mkt.address}.json`
        );
        // Write the canonical bytes exactly, with no trailing
        // newline, then read them back and verify the hash.
        writeFileAtomic(bundlePath, canonicalBytes);
        const rereadBytes = fs.readFileSync(bundlePath);
        let rereadHashHex;
        try {
          const rereadBundle = JSON.parse(rereadBytes.toString("utf8"));
          const rereadHash = hashFn(rereadBundle);
          rereadHashHex = hashResultHex(
            rereadHash,
            hashResultBytes(rereadHash)
          );
        } catch (err) {
          throw new Error(
            `bundle verification failed for market ${mkt.address}: ${err.message}`
          );
        }
        if (rereadHashHex !== hashHex) {
          throw new Error(
            `bundle hash mismatch for market ${mkt.address}: wrote ${hashHex}, re-read ${rereadHashHex}`
          );
        }
        const bundleSha256 = crypto
          .createHash("sha256")
          .update(rereadBytes)
          .digest("hex");

        const data = Buffer.concat([
          Buffer.from([RESOLVE_ORDINAL, outcome, triggerId]),
          hashBytes,
        ]);
        const record = withTriggers(baseRecord(mkt, "WOULD_RESOLVE", reason));
        record.instruction = {
          name: "ResolveMarket",
          ordinal: RESOLVE_ORDINAL,
          data_hex: data.toString("hex"),
          data_len: data.length,
          accounts: buildAccounts({
            resolverPubkey,
            config: marketsObj.config,
            market: mkt,
          }),
        };
        record.outcome = outcome;
        record.hard_trigger = triggerId;
        record.evidence_hash = hashHex;
        record.trigger_event_ts = triggerEventTs;
        record.bundle_path = bundlePath;
        record.bundle_bytes = rereadBytes.length;
        record.bundle_sha256 = bundleSha256;
        ledger[mkt.address] = {
          action: "WOULD_RESOLVE",
          outcome,
          hard_trigger: triggerId,
          evidence_hash: hashHex,
          bundle_path: bundlePath,
          decided_at: passNow,
        };
        ledgerDirty = true;
        emit(record);
      };

      // Decision table.
      if (qualifying.length > 0) {
        if (passNow < market.expiry || complete) {
          emitResolve(
            market,
            OUTCOME_FLAGGED,
            primary.id,
            primary.eventTimestamp,
            "qualifying-hard-trigger"
          );
        } else {
          deferOrCancel();
        }
        continue;
      }

      if (passNow < market.expiry) {
        if (complete) {
          emit(
            withTriggers(
              baseRecord(
                market,
                "WATCHING",
                "complete-no-hard-trigger-before-expiry"
              )
            )
          );
        } else {
          emit(
            withTriggers(
              baseRecord(
                market,
                "WATCHING",
                "evidence-partial-before-expiry"
              )
            )
          );
          emit(
            withTriggers(
              baseRecord(
                market,
                "EVIDENCE_PARTIAL_EARLY",
                "evidence-partial-before-expiry"
              )
            )
          );
        }
        continue;
      }

      // At or past expiry, with no qualifying trigger.
      if (complete) {
        emitResolve(
          market,
          OUTCOME_SURVIVES,
          0,
          null,
          "no-qualifying-hard-trigger-after-expiry"
        );
      } else {
        deferOrCancel();
      }
    }

    if (ledgerPath && ledgerDirty) {
      writeFileAtomic(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`);
    } else if (ledgerPath && !fs.existsSync(ledgerPath)) {
      // A completed pass still leaves a ledger file behind, even if
      // nothing in it was decidable this time.
      writeFileAtomic(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`);
    }

    return records;
  }

  return {
    runPass,
    getCrewStore,
    get crewStore() {
      return crewStore;
    },
  };
}

// ---------------------------------------------------------------------------
// Fixture validation (command line only; the factory stays permissive
// so unit tests can drive it with minimal market objects).
// ---------------------------------------------------------------------------

function validateMarkets(marketsObj) {
  const errors = [];
  if (!marketsObj || typeof marketsObj !== "object") {
    return ["markets file is not a JSON object"];
  }
  if (marketsObj.schema !== "rugcall-markets/v1") {
    errors.push(
      `schema must be "rugcall-markets/v1", got ${JSON.stringify(marketsObj.schema)}`
    );
  }
  if (!Array.isArray(marketsObj.markets)) {
    errors.push("markets must be an array");
    return errors;
  }
  const seen = new Set();
  for (const market of marketsObj.markets) {
    const label = market && market.address ? market.address : "<unknown>";
    if (!market || typeof market !== "object") {
      errors.push("market entry is not an object");
      continue;
    }
    if (!market.address || typeof market.address !== "string") {
      errors.push("market is missing an address");
    } else if (seen.has(market.address)) {
      errors.push(`duplicate market address ${market.address}`);
    } else {
      seen.add(market.address);
    }
    if (![STATE_OPEN, STATE_RESOLVED, STATE_CANCELLED].includes(market.state)) {
      errors.push(`market ${label} has unknown state code ${market.state}`);
    }
    if (
      market.outcome !== undefined &&
      ![0, 1, 2].includes(market.outcome)
    ) {
      errors.push(`market ${label} has unknown outcome code ${market.outcome}`);
    }
    if (
      market.hard_trigger !== undefined &&
      (!Number.isInteger(market.hard_trigger) ||
        market.hard_trigger < 0 ||
        market.hard_trigger > 5)
    ) {
      errors.push(
        `market ${label} has hard_trigger ${market.hard_trigger}, outside 0..5`
      );
    }
    if (
      market.evidence_hash !== undefined &&
      !/^[0-9a-fA-F]{64}$/.test(market.evidence_hash)
    ) {
      errors.push(`market ${label} has a malformed evidence_hash`);
    }
    if (
      typeof market.expiry === "number" &&
      typeof market.created_at === "number" &&
      market.expiry <= market.created_at
    ) {
      errors.push(`market ${label} has expiry at or before created_at`);
    }
  }
  return errors;
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const args = { _: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (token.startsWith("--")) {
      const eq = token.indexOf("=");
      if (eq !== -1) {
        args[token.slice(2, eq)] = token.slice(eq + 1);
      } else if (
        token === "--watch" ||
        token === "--live" ||
        token === "--dry-run"
      ) {
        args[token.slice(2)] = true;
      } else {
        i += 1;
        if (i >= argv.length) throw new Error(`missing value for ${token}`);
        args[token.slice(2)] = argv[i];
      }
    } else {
      args._.push(token);
    }
  }
  return args;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function buildLiveDeps() {
  const rpcUrl = MAINNET_RPC_URL;
  const fetchJson = async (url, init) => {
    const response = await fetch(url, init);
    if (!response.ok) {
      throw new Error(`fetch failed with HTTP ${response.status} for ${url}`);
    }
    return response.json();
  };
  const rpc = async (method, params) => {
    const body = await fetchJson(rpcUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method,
        params: params || [],
      }),
    });
    if (body && body.error) {
      throw new Error(
        `RPC ${method} failed: ${body.error.message || JSON.stringify(body.error)}`
      );
    }
    return body ? body.result : undefined;
  };
  return { rpc, fetchJson, nowMs: Date.now(), rpcUrl };
}

async function runDryRun(args) {
  if (!args.markets) {
    process.stderr.write("dry-run requires --markets <file>\n");
    return 1;
  }
  const watch = args.watch === true;
  if (!watch && (args.now === undefined || args.now === "")) {
    process.stderr.write(
      "dry-run requires --now <unix-seconds> (no wall-clock fallback)\n"
    );
    return 1;
  }
  const evidenceMode = args.evidence || "fixture";
  if (evidenceMode !== "fixture" && evidenceMode !== "live") {
    process.stderr.write(`unknown --evidence mode ${evidenceMode}\n`);
    return 1;
  }

  let marketsObj;
  try {
    marketsObj = readJsonFile(args.markets, "markets file");
  } catch (err) {
    process.stderr.write(`${err.message}\n`);
    return 1;
  }

  const validationErrors = validateMarkets(marketsObj);
  if (validationErrors.length > 0) {
    for (const err of validationErrors) {
      process.stderr.write(`invalid fixture: ${err}\n`);
    }
    return 1;
  }

  const outDir = args["out-dir"] || path.join(process.cwd(), "rugcall-out");
  const evidenceDir = args["evidence-dir"] || null;
  const ledgerPath = args.ledger || null;
  const resolverPubkey =
    args.resolver || args["resolver-pubkey"] || DEFAULT_RESOLVER_PUBKEY;

  let initialCrewStore = ev.emptyCrewStore();
  if (args["crew-store"]) {
    if (fs.existsSync(args["crew-store"])) {
      try {
        initialCrewStore = readJsonFile(args["crew-store"], "crew store");
      } catch (err) {
        process.stderr.write(`${err.message}\n`);
        return 1;
      }
    }
  }

  const writeCrewStore = (resolver) => {
    fs.mkdirSync(outDir, { recursive: true });
    writeFileAtomic(
      path.join(outDir, "crew-store.json"),
      `${JSON.stringify(resolver.getCrewStore(), null, 2)}\n`
    );
  };

  // One pass. Factored out so --watch can repeat it with a real clock.
  const doPass = async (passNow, crewStoreIn) => {
    let evidenceByMarket = null;
    if (evidenceMode === "live") {
      if (typeof ev.fetchEvidence !== "function") {
        throw new Error("evidence module does not provide fetchEvidence");
      }
      evidenceByMarket = {};
      const deps = buildLiveDeps();
      const openMarkets = (marketsObj.markets || [])
        .filter((m) => m.state === STATE_OPEN)
        .sort(compareAddress);
      for (const market of openMarkets) {
        const fetched = await ev.fetchEvidence(market.subject_mint, deps);
        // Record the fetched input before any evaluation, so this
        // live decision can be replayed later as a fixture run.
        const inputPath = path.join(
          outDir,
          "evidence-inputs",
          `${market.address}-${passNow}.json`
        );
        writeFileAtomic(inputPath, `${JSON.stringify(fetched, null, 2)}\n`);
        evidenceByMarket[market.address] = fetched;
      }
    }
    const resolver = createResolver({
      resolverPubkey,
      now: passNow,
      outDir,
      ledgerPath,
      evidenceDir,
      crewStore: crewStoreIn,
      evidenceByMarket,
    });
    const records = resolver.runPass(marketsObj);
    return { records, resolver };
  };

  if (!watch) {
    const passNow = Number(args.now);
    if (!Number.isFinite(passNow)) {
      process.stderr.write(`--now must be a number, got ${args.now}\n`);
      return 1;
    }
    let outcome;
    try {
      outcome = await doPass(passNow, initialCrewStore);
    } catch (err) {
      process.stderr.write(`resolver pass failed: ${err.message}\n`);
      return 2;
    }
    const { records, resolver } = outcome;
    try {
      writeCrewStore(resolver);
    } catch (err) {
      process.stderr.write(`cannot write crew store: ${err.message}\n`);
      return 2;
    }
    if (records.some((r) => r.action === "REFUSAL")) return 1;
    if (
      records.some(
        (r) =>
          r.action === "BLOCKED_ZERO_HASH" ||
          r.action === "WOULD_RESOLVE_INVALID"
      )
    ) {
      return 2;
    }
    const resolved = records.filter((r) => r.action === "WOULD_RESOLVE").length;
    const cancelled = records.filter((r) => r.action === "WOULD_CANCEL").length;
    const deferred = records.filter(
      (r) => r.action === "DEFERRED_PARTIAL"
    ).length;
    process.stderr.write(
      `dry-run pass at ${passNow}: ${records.length} record(s), ` +
        `${resolved} would-resolve, ${cancelled} would-cancel, ${deferred} deferred\n`
    );
    return 0;
  }

  // Watch mode: repeat passes on a real clock. Deferral gating lives
  // in the ledger handling inside runPass, so a deferred market is
  // only re-evaluated once its retry time has been reached.
  const intervalSecs = Number(args["interval-secs"] || 60);
  const intervalMs =
    Number.isFinite(intervalSecs) && intervalSecs > 0
      ? intervalSecs * 1000
      : 60000;
  let currentCrewStore = initialCrewStore;
  for (;;) {
    const passNow = Math.floor(Date.now() / 1000);
    try {
      const { records, resolver } = await doPass(passNow, currentCrewStore);
      currentCrewStore = resolver.getCrewStore();
      writeCrewStore(resolver);
      if (records.some((r) => r.action === "REFUSAL")) return 1;
    } catch (err) {
      process.stderr.write(`resolver pass failed: ${err.message}\n`);
      return 2;
    }
    await sleep(intervalMs);
  }
}

async function main(argv) {
  const mode = argv[0];
  if (mode === "live") {
    process.stderr.write(
      "live mode is not available in this build: the resolver runs dry-run only\n"
    );
    return 3;
  }
  if (mode !== "dry-run") {
    process.stderr.write(
      "usage: node resolver.js dry-run --markets <file> --evidence-dir <dir> " +
        "--crew-store <file> --out-dir <dir> --ledger <file> --now <unix-seconds> " +
        "[--evidence=fixture|live] [--watch --interval-secs N]\n"
    );
    return 1;
  }
  let args;
  try {
    args = parseArgs(argv.slice(1));
  } catch (err) {
    process.stderr.write(`${err.message}\n`);
    return 1;
  }
  return runDryRun(args);
}

if (require.main === module) {
  main(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((err) => {
      process.stderr.write(`resolver failed: ${err.message}\n`);
      process.exitCode = 2;
    });
}

module.exports = {
  createResolver,
  validateMarkets,
  DEFAULT_RESOLVER_PUBKEY,
};
