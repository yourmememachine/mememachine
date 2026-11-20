"use strict";

// RugCall resolver.
//
// Dry-run mode reads fixture markets and recorded evidence snapshots,
// evaluates each open market through the shared evidence module,
// writes the canonical evidence bundle for every market it would
// resolve, and logs the instruction it would post. Dry-run never
// signs, never submits, never reads a keypair file, and needs no
// chain RPC in fixture mode: every record carries signed:false and
// submitted:false.
//
// Live mode (createLiveRunner, below) shares the exact same decision
// code through decidePass, then signs and submits the decided
// instructions on devnet after a preflight pass; its records and
// ledger entries only claim signed/submitted once that has actually
// happened for them.

const fs = require("fs");
const path = require("path");
const crypto = require("crypto");

const ev = require("../app/evidence.js");
const submitLib = require("./submit.js");

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
const DEVNET_RPC_URL = "https://api.devnet.solana.com";
const BPF_LOADER_UPGRADEABLE = "BPFLoaderUpgradeab1e11111111111111111111111";
const DEFAULT_MIN_RESOLVER_LAMPORTS = 50000000;
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

  function decidePassInternal(marketsObj, nowOverride) {
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
      return { records, ledger: {}, ledgerDirty: false, skipPersist: true };
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

    return { records, ledger, ledgerDirty };
  }

  function decidePass(marketsObj, nowOverride) {
    return decidePassInternal(marketsObj, nowOverride).records;
  }

  function runPass(marketsObj, nowOverride) {
    const outcome = decidePassInternal(marketsObj, nowOverride);
    if (outcome.skipPersist) {
      return outcome.records;
    }
    if (ledgerPath && outcome.ledgerDirty) {
      writeFileAtomic(
        ledgerPath,
        `${JSON.stringify(outcome.ledger, null, 2)}\n`
      );
    } else if (ledgerPath && !fs.existsSync(ledgerPath)) {
      // A completed pass still leaves a ledger file behind, even if
      // nothing in it was decidable this time.
      writeFileAtomic(
        ledgerPath,
        `${JSON.stringify(outcome.ledger, null, 2)}\n`
      );
    }
    return outcome.records;
  }

  return {
    runPass,
    decidePass,
    getCrewStore,
    get crewStore() {
      return crewStore;
    },
  };
}

// ---------------------------------------------------------------------------
// Live mode: chain seam, preflight, and the submitting runner.
// ---------------------------------------------------------------------------

function withTimeout(promise, ms, label) {
  let timer = null;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`${label} timed out after ${ms}ms`)),
      ms
    );
    if (timer.unref) timer.unref();
  });
  return Promise.race([promise, timeout]).finally(() => {
    if (timer) clearTimeout(timer);
  });
}

function signatureStatusConfirmed(status) {
  if (!status || typeof status !== "object" || status.err) return false;
  if (status.confirmed === true) return true;
  return (
    status.confirmationStatus === "confirmed" ||
    status.confirmationStatus === "finalized"
  );
}

// Production chain seam for live mode, bound to the caller-supplied
// (devnet) RPC URL. This is a separate construction from
// buildLiveDeps, which serves mainnet evidence fetching and is never
// used for chain writes.
function buildChainDeps(rpcUrl) {
  const url = rpcUrl || DEVNET_RPC_URL;
  const rpc = async (method, params) => {
    const response = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method,
        params: params || [],
      }),
      signal: AbortSignal.timeout ? AbortSignal.timeout(10000) : undefined,
    });
    if (!response.ok) {
      throw new Error(`fetch failed with HTTP ${response.status} for ${url}`);
    }
    const body = await response.json();
    if (body && body.error) {
      throw new Error(
        `RPC ${method} failed: ${body.error.message || JSON.stringify(body.error)}`
      );
    }
    return body ? body.result : undefined;
  };
  return {
    rpcUrl: url,
    async getHealth() {
      return rpc("getHealth", []);
    },
    async getAccountInfo(pubkey) {
      const result = await rpc("getAccountInfo", [
        pubkey,
        { encoding: "base64" },
      ]);
      const value = result && result.value;
      if (!value) return null;
      const dataField = Array.isArray(value.data) ? value.data[0] : "";
      return {
        executable: !!value.executable,
        owner: value.owner,
        lamports: value.lamports,
        data: Buffer.from(dataField || "", "base64"),
      };
    },
    async getBalance(pubkey) {
      const result = await rpc("getBalance", [pubkey]);
      return result && typeof result.value === "number" ? result.value : 0;
    },
    async getLatestBlockhash() {
      const result = await rpc("getLatestBlockhash", []);
      return result && result.value ? result.value.blockhash : null;
    },
    async sendRawTransaction(bytes) {
      // The canonical RPC method name is sendTransaction; the
      // sendRawTransaction alias is not served by current RPC
      // builds (it answers -32601 Method not found, verified
      // against the public devnet endpoint on 2026-11-20). The
      // seam keeps its name; only the wire method changed.
      return rpc("sendTransaction", [
        Buffer.from(bytes).toString("base64"),
        { encoding: "base64" },
      ]);
    },
    async getSignatureStatus(signature) {
      const result = await rpc("getSignatureStatuses", [[signature]]);
      const values = result && Array.isArray(result.value) ? result.value : [];
      return values.length > 0 ? values[0] : null;
    },
  };
}

function createLiveRunner(options) {
  const opts = options || {};
  const chain = opts.chain;
  if (!chain) throw new Error("createLiveRunner requires a chain seam");
  const sink =
    opts.sink ||
    ((record) => {
      process.stdout.write(`${JSON.stringify(record)}\n`);
    });
  const rpcUrl = opts.rpcUrl || DEVNET_RPC_URL;
  const outDir = opts.outDir || path.join(process.cwd(), "rugcall-out");
  const ledgerPath = opts.ledgerPath || null;
  const evidenceDir = opts.evidenceDir || null;
  const evidenceByMarket = opts.evidenceByMarket || null;
  const evidenceProvider = opts.evidenceProvider || null;
  const hashFn = opts.hashFn || undefined;
  const minResolverLamports =
    opts.minResolverLamports !== undefined
      ? opts.minResolverLamports
      : DEFAULT_MIN_RESOLVER_LAMPORTS;
  const keypairPath = opts.keypairPath || null;
  let signer = opts.signer || null;
  let submitter = opts.submitter || null;
  let crewStore =
    opts.crewStore !== undefined ? opts.crewStore : ev.emptyCrewStore();

  function getCrewStore() {
    return crewStore;
  }

  function resolveNow(nowOverride) {
    const value =
      nowOverride !== undefined
        ? nowOverride
        : typeof opts.now === "function"
          ? opts.now()
          : opts.now;
    if (typeof value !== "number" || !Number.isFinite(value)) {
      throw new Error("live pass requires a pass time (now, unix seconds)");
    }
    return value;
  }

  async function runLivePass(marketsObj, nowOverride) {
    const passNow = resolveNow(nowOverride);
    if (!marketsObj || typeof marketsObj !== "object") {
      throw new Error("runLivePass requires a markets object");
    }
    const records = [];
    const emit = (record) => {
      records.push(record);
      sink(record);
      return record;
    };
    const programIdOverride = opts.programId || opts.program_id || null;
    const marketsProgramId =
      marketsObj.program_id !== undefined ? marketsObj.program_id : null;
    const effectiveProgramId = programIdOverride || marketsProgramId;

    const refusal = (code, detail, extra) => {
      const record = {
        ts: passNow,
        action: "REFUSAL",
        market: null,
        subject_mint: null,
        creator: null,
        resolver: signer ? signer.pubkey : null,
        program_id: effectiveProgramId || null,
        expiry: null,
        decision_reason: "live-preflight-failed",
        qualifying_triggers: [],
        excluded_triggers: [],
        signed: false,
        submitted: false,
        signature: null,
        reason: code,
        live_preflight: {
          check: code,
          detail,
          rpc_url: rpcUrl,
          program_id: effectiveProgramId || null,
          ...(extra || {}),
        },
      };
      emit(record);
      return { records, exitCode: 1 };
    };

    // Preflight, in order. The first failure wins, emits exactly one
    // refusal record, and no signing is attempted on any refusal path.
    // (1) Program id.
    if (!programIdOverride && !marketsProgramId) {
      return refusal(
        "program-id-unset",
        "no program id from --program-id or the markets file"
      );
    }
    if (
      programIdOverride &&
      marketsProgramId &&
      programIdOverride !== marketsProgramId
    ) {
      return refusal(
        "program-id-mismatch",
        `--program-id ${programIdOverride} does not match the markets file program_id ${marketsProgramId}`
      );
    }

    // (2) Keypair file.
    if (keypairPath) {
      if (!fs.existsSync(keypairPath)) {
        return refusal(
          "keypair-missing",
          `keypair file not found at ${keypairPath}`
        );
      }
      try {
        submitLib.parseKeypairBytes(
          fs.readFileSync(keypairPath, "utf8"),
          keypairPath
        );
      } catch (err) {
        return refusal("keypair-malformed", err.message);
      }
      if (!signer) {
        try {
          signer = submitLib.loadResolverSigner(keypairPath);
        } catch (err) {
          return refusal("keypair-malformed", err.message);
        }
      }
    } else if (!signer) {
      return refusal("keypair-missing", "no keypair path supplied");
    }

    // (3) RPC reachability.
    try {
      await withTimeout(
        Promise.resolve().then(() => chain.getHealth()),
        10000,
        "getHealth"
      );
    } catch (err) {
      return refusal("rpc-unreachable", err.message);
    }

    // (4)-(6) Program account.
    let programInfo;
    try {
      programInfo = await chain.getAccountInfo(effectiveProgramId);
    } catch (err) {
      return refusal("rpc-unreachable", err.message);
    }
    if (!programInfo) {
      return refusal(
        "program-missing",
        `no account found for program ${effectiveProgramId}`
      );
    }
    if (!programInfo.executable) {
      return refusal(
        "program-not-executable",
        `program account ${effectiveProgramId} is not executable`
      );
    }
    if (programInfo.owner !== BPF_LOADER_UPGRADEABLE) {
      return refusal(
        "program-wrong-owner",
        `program account is owned by ${programInfo.owner}, not ${BPF_LOADER_UPGRADEABLE}`
      );
    }

    // (7)-(8) Config account.
    let configInfo;
    try {
      configInfo = await chain.getAccountInfo(marketsObj.config);
    } catch (err) {
      return refusal("rpc-unreachable", err.message);
    }
    if (!configInfo) {
      return refusal(
        "config-missing",
        `no account found for config ${marketsObj.config}`
      );
    }
    const configData = Buffer.from(configInfo.data || []);
    if (
      configInfo.owner !== effectiveProgramId ||
      configData.length !== 122 ||
      configData[0] !== 1
    ) {
      return refusal(
        "config-malformed",
        `config owner ${configInfo.owner}, data length ${configData.length}, discriminator ${configData.length > 0 ? configData[0] : "none"}; expected owner ${effectiveProgramId}, length 122, discriminator 1`
      );
    }

    // (9) Signer must be the resolver recorded in the Config account
    // (bytes 33..65: discriminator 1 + admin 32, then resolver 32).
    const configResolver = submitLib.base58Encode(configData.subarray(33, 65));
    if (configResolver !== signer.pubkey) {
      return refusal(
        "keypair-config-mismatch",
        `signer ${signer.pubkey} is not the config resolver ${configResolver}`
      );
    }

    // (10) Resolver balance.
    let balance;
    try {
      balance = await chain.getBalance(signer.pubkey);
    } catch (err) {
      return refusal("rpc-unreachable", err.message);
    }
    if (typeof balance !== "number" || balance < minResolverLamports) {
      return refusal(
        "resolver-underfunded",
        `resolver balance ${balance} lamports is below the required ${minResolverLamports} lamports`,
        { actual_lamports: balance, required_lamports: minResolverLamports }
      );
    }

    if (!submitter) {
      submitter = submitLib.createSubmitter({ chain, signer });
    }

    const persistLiveEntry = (address, entry) => {
      if (!ledgerPath) return;
      const ledger = loadLedger(ledgerPath);
      ledger[address] = entry;
      writeFileAtomic(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`);
    };

    // Poll previously submitted-but-unconfirmed entries before any
    // new decision: a confirmed entry becomes final (so the decision
    // pass skips the market), an unconfirmed one is deferred again
    // instead of being resubmitted.
    if (ledgerPath && typeof chain.getSignatureStatus === "function") {
      const ledger = loadLedger(ledgerPath);
      let changed = false;
      for (const [address, entry] of Object.entries(ledger)) {
        if (
          !entry ||
          entry.action !== "SUBMITTED_UNCONFIRMED" ||
          !entry.signature
        ) {
          continue;
        }
        let status = null;
        try {
          status = await chain.getSignatureStatus(entry.signature);
        } catch (err) {
          status = null;
        }
        if (signatureStatusConfirmed(status)) {
          ledger[address] = {
            action: entry.pending_action || "WOULD_RESOLVE",
            outcome: entry.outcome !== undefined ? entry.outcome : null,
            hard_trigger:
              entry.hard_trigger !== undefined ? entry.hard_trigger : 0,
            evidence_hash:
              entry.evidence_hash !== undefined ? entry.evidence_hash : null,
            bundle_path:
              entry.bundle_path !== undefined ? entry.bundle_path : null,
            submitted: true,
            signature: entry.signature,
            decided_at: entry.decided_at || passNow,
          };
          changed = true;
          emit({
            ts: passNow,
            action: "SUBMIT_CONFIRMED",
            market: address,
            subject_mint: null,
            creator: null,
            resolver: signer.pubkey,
            program_id: effectiveProgramId,
            expiry: null,
            decision_reason: "submitted-confirmed-on-recheck",
            qualifying_triggers: [],
            excluded_triggers: [],
            signed: true,
            submitted: true,
            signature: entry.signature,
          });
        } else {
          ledger[address] = { ...entry, next_retry_at: passNow + RETRY_SEC };
          changed = true;
        }
      }
      if (changed) {
        writeFileAtomic(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`);
      }
    }

    // Decision pass: the shared dry-run decision code, without its
    // ledger persistence (live entries are written below, only for
    // submissions that actually happen).
    const resolver = createResolver({
      resolverPubkey: signer.pubkey,
      now: passNow,
      outDir,
      ledgerPath,
      evidenceDir,
      crewStore,
      evidenceByMarket,
      evidenceProvider,
      hashFn,
      sink: () => {},
    });
    const decided = resolver.decidePass(marketsObj);
    crewStore = resolver.getCrewStore();
    for (const record of decided) emit(record);

    if (decided.some((r) => r.action === "REFUSAL")) {
      return { records, exitCode: 1 };
    }
    if (
      decided.some(
        (r) =>
          r.action === "BLOCKED_ZERO_HASH" ||
          r.action === "WOULD_RESOLVE_INVALID"
      )
    ) {
      return { records, exitCode: 2 };
    }

    // Deferrals from the decision pass are durable in live mode too.
    if (ledgerPath) {
      const ledger = loadLedger(ledgerPath);
      let deferralChanged = false;
      for (const record of decided) {
        if (record.action === "DEFERRED_PARTIAL") {
          ledger[record.market] = {
            ...(ledger[record.market] || {}),
            next_retry_at: record.next_retry_at,
          };
          deferralChanged = true;
        }
      }
      if (deferralChanged) {
        writeFileAtomic(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`);
      }
    }

    let exitCode = 0;
    for (const record of decided) {
      if (record.action !== "WOULD_RESOLVE" && record.action !== "WOULD_CANCEL") {
        continue;
      }
      let result;
      try {
        result = await submitter.submitInstruction({
          programId: effectiveProgramId,
          instruction: {
            dataHex: record.instruction.data_hex,
            accounts: record.instruction.accounts,
          },
        });
      } catch (err) {
        emit({
          ts: passNow,
          action: "SUBMIT_FAILED",
          market: record.market,
          subject_mint: record.subject_mint,
          creator: record.creator,
          resolver: signer.pubkey,
          program_id: effectiveProgramId,
          expiry: record.expiry,
          decision_reason: "submit-failed",
          qualifying_triggers: record.qualifying_triggers,
          excluded_triggers: record.excluded_triggers,
          signed: true,
          submitted: false,
          signature: null,
          reason: err.message,
        });
        exitCode = 4;
        continue;
      }
      const signature = result && result.signature;
      if (result && result.confirmed) {
        record.signed = true;
        record.submitted = true;
        record.signature = signature;
        persistLiveEntry(record.market, {
          action: record.action,
          outcome: record.outcome !== undefined ? record.outcome : null,
          hard_trigger:
            record.hard_trigger !== undefined ? record.hard_trigger : 0,
          evidence_hash:
            record.evidence_hash !== undefined ? record.evidence_hash : null,
          bundle_path:
            record.bundle_path !== undefined ? record.bundle_path : null,
          submitted: true,
          signature,
          decided_at: passNow,
        });
      } else {
        const nextRetryAt = passNow + RETRY_SEC;
        emit({
          ts: passNow,
          action: "SUBMITTED_UNCONFIRMED",
          market: record.market,
          subject_mint: record.subject_mint,
          creator: record.creator,
          resolver: signer.pubkey,
          program_id: effectiveProgramId,
          expiry: record.expiry,
          decision_reason: "submitted-unconfirmed",
          qualifying_triggers: record.qualifying_triggers,
          excluded_triggers: record.excluded_triggers,
          signed: true,
          submitted: true,
          signature: signature || null,
          next_retry_at: nextRetryAt,
        });
        persistLiveEntry(record.market, {
          action: "SUBMITTED_UNCONFIRMED",
          pending_action: record.action,
          outcome: record.outcome !== undefined ? record.outcome : null,
          hard_trigger:
            record.hard_trigger !== undefined ? record.hard_trigger : 0,
          evidence_hash:
            record.evidence_hash !== undefined ? record.evidence_hash : null,
          bundle_path:
            record.bundle_path !== undefined ? record.bundle_path : null,
          submitted: true,
          signature: signature || null,
          decided_at: passNow,
          next_retry_at: nextRetryAt,
        });
        exitCode = 4;
      }
    }

    return { records, exitCode };
  }

  return { runLivePass, getCrewStore };
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
  // evidence.js's documented transport contract is
  // rpc(url, method, params) -> Promise of the FULL parsed JSON-RPC
  // reply: its rpcPort inspects reply.error itself and its callers
  // read reply.result. An earlier shape of this adapter took
  // (method, params) and returned reply.result only, which made
  // every live evidence fetch fail before the first family could
  // load (found on the first real-network exercise, 2026-11-20;
  // pinned by test/live-deps.test.js).
  const rpc = async (url, method, params) => {
    const body = await fetchJson(url || rpcUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method,
        params: params || [],
      }),
    });
    return body;
  };
  return { rpc, fetchJson, nowMs: () => Date.now(), rpcUrl };
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

async function runLive(args) {
  if (args.now !== undefined) {
    process.stderr.write(
      "live mode does not accept --now (it runs on the real clock)\n"
    );
    return 1;
  }
  if (!args.markets) {
    process.stderr.write("live requires --markets <file>\n");
    return 1;
  }
  if (!args.keypair) {
    process.stderr.write("live requires --keypair <path>\n");
    return 1;
  }
  const rpcUrl = args["rpc-url"] || DEVNET_RPC_URL;
  if (rpcUrl === MAINNET_RPC_URL) {
    process.stderr.write(
      `live mode refuses the mainnet RPC URL ${MAINNET_RPC_URL} for chain writes\n`
    );
    return 1;
  }
  const evidenceMode = args.evidence || "fixture";
  if (evidenceMode !== "fixture" && evidenceMode !== "live") {
    process.stderr.write(`unknown --evidence mode ${evidenceMode}\n`);
    return 1;
  }
  const minResolverLamports =
    args["min-resolver-lamports"] !== undefined
      ? Number(args["min-resolver-lamports"])
      : DEFAULT_MIN_RESOLVER_LAMPORTS;
  if (!Number.isFinite(minResolverLamports) || minResolverLamports < 0) {
    process.stderr.write(
      `--min-resolver-lamports must be a non-negative number, got ${args["min-resolver-lamports"]}\n`
    );
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
  const writeCrewStore = (store) => {
    fs.mkdirSync(outDir, { recursive: true });
    writeFileAtomic(
      path.join(outDir, "crew-store.json"),
      `${JSON.stringify(store, null, 2)}\n`
    );
  };

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
        const inputPath = path.join(
          outDir,
          "evidence-inputs",
          `${market.address}-${passNow}.json`
        );
        writeFileAtomic(inputPath, `${JSON.stringify(fetched, null, 2)}\n`);
        evidenceByMarket[market.address] = fetched;
      }
    }
    const runner = createLiveRunner({
      chain: buildChainDeps(rpcUrl),
      keypairPath: args.keypair,
      programId: args["program-id"] || null,
      now: passNow,
      outDir,
      ledgerPath,
      evidenceDir,
      crewStore: crewStoreIn,
      evidenceByMarket,
      minResolverLamports,
      rpcUrl,
    });
    const outcome = await runner.runLivePass(marketsObj);
    return { ...outcome, crewStore: runner.getCrewStore() };
  };

  const watch = args.watch === true;
  if (!watch) {
    const passNow = Math.floor(Date.now() / 1000);
    let outcome;
    try {
      outcome = await doPass(passNow, initialCrewStore);
    } catch (err) {
      process.stderr.write(`live pass failed: ${err.message}\n`);
      return 2;
    }
    try {
      writeCrewStore(outcome.crewStore);
    } catch (err) {
      process.stderr.write(`cannot write crew store: ${err.message}\n`);
      return 2;
    }
    return outcome.exitCode;
  }

  const intervalSecs = Number(args["interval-secs"] || 60);
  const intervalMs =
    Number.isFinite(intervalSecs) && intervalSecs > 0
      ? intervalSecs * 1000
      : 60000;
  let currentCrewStore = initialCrewStore;
  for (;;) {
    const passNow = Math.floor(Date.now() / 1000);
    try {
      const outcome = await doPass(passNow, currentCrewStore);
      currentCrewStore = outcome.crewStore;
      writeCrewStore(outcome.crewStore);
      if (outcome.exitCode === 1) return 1;
    } catch (err) {
      process.stderr.write(`live pass failed: ${err.message}\n`);
      return 2;
    }
    await sleep(intervalMs);
  }
}

async function main(argv) {
  const mode = argv[0];
  if (mode !== "dry-run" && mode !== "live") {
    process.stderr.write(
      "usage: node resolver.js dry-run --markets <file> --evidence-dir <dir> " +
        "--crew-store <file> --out-dir <dir> --ledger <file> --now <unix-seconds> " +
        "[--evidence=fixture|live] [--watch --interval-secs N]\n" +
        "       node resolver.js live --markets <file> --keypair <path> " +
        "[--evidence=fixture|live] [--evidence-dir <dir>] [--out-dir <dir>] " +
        "[--ledger <file>] [--crew-store <file>] [--rpc-url <url>] " +
        "[--program-id <base58>] [--min-resolver-lamports <n>] " +
        "[--watch --interval-secs N]\n"
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
  if (mode === "live") {
    return runLive(args);
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
  createLiveRunner,
  buildChainDeps,
  buildLiveDeps,
  validateMarkets,
  DEFAULT_RESOLVER_PUBKEY,
  DEVNET_RPC_URL,
  MAINNET_RPC_URL,
};
