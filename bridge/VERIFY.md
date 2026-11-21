# Bridge Verification

Real commands and their real output. Nothing here is claimed without a
run behind it. Verified 2026-11-14 ~11:07 UTC, in the bridge workspace
directory.

## Current state (2026-11-21, latest commits — read this first)

The sections below are the dated verification log; this summary is
the current state and supersedes their numbers.

- **Tests:** bridge workspace **222 passed, 0 failed** (49
  bridge-common unit + 4 cap-accounting property tests + 35 machine-bridge
  + 30 solana-lock (29 + 1 precompile-cost) + 66 relayer + 23 proof bundle + 15 receipt
  indexer, including the
  full local end-to-end drill and the 45-crossing volume drill);
  LaunchGuard **68 passed**, PQ vault **17 passed** and registry
  export **11 passed** (`programs/VERIFY.md`,
  `programs/PQ-VERIFY.md`); fork suite 49 OK.
  Full `make verify` 2026-11-21: **777 passed, 0 failed**.
  This run verifies commit `bd02459` (the RugCall judge-page
  fixture fix; the bridge side also carries the cap-accounting
  property tests from `e9a5dca` and the
  A-F1 external-seat positivity guard from `68fa4b6`). The SBF rebuild is
  byte-identical to the previous pin (solana_lock.so
  `42bbfadc…` 212,840 B, machine_bridge.so `e8b85254…`
  215,832 B) — the on-chain programs did not change.
- **PQ Vault (2026-11-19, commit `8ca345c`):** new native
  program in the `programs/` workspace (not a bridge component):
  a cold-storage vault authorized by WOTS+ one-time signatures
  instead of Ed25519, with program-enforced one-time discipline
  and chunked signature delivery through a digest-bound buffer.
  17 tests; measured verification cost against the SBF build is
  337k–395k CU (ExecuteWithdrawal) — verifying transactions must
  request a raised compute budget. SBF built: pq_vault.so
  `b6a52b93…` (148,944 B, deterministic). See
  `programs/PQ-VAULT.md` and `programs/PQ-VERIFY.md`.
- **Debate round 4 (2026-11-18, commit `236257b`):** bridge-common
  + proof-bundle scope. `MachineConfig` gained `source_mint`
  (LEN 334 → 366) and `MintWrapped` binds `message.source_mint`;
  `validate_message` bounds attestation lifetime at 1,000,000
  slots on the source basis; zero/impossible quorum and admin
  thresholds are rejected by the shared verifiers; the ed25519
  instruction builder refuses oversized sets; proof-bundle's
  verifier rejects non-ASCII hashes without panicking and
  zero-amount bundles, stamps the protocol quorum on export,
  carries the verified relayer set in its report, and can pin
  that set (`verify --expected-relayers`); the CLI caps input
  files at 1 MiB. SBF rebuilt: solana_lock.so `a22bccd8…`
  (211,744 B), machine_bridge.so `7d0ab41f…` (215,312 B).
- **Debate round 7 (2026-11-20, commit `8c53c16`):** scope 1
  second rotation. Machine `Initialize` rejects a wrapped mint
  with non-zero supply and a default `source_mint`; Lock
  `Initialize` rejects a vault with a personal close authority
  or a non-zero balance; `Deposit`/`Burn` reject a default
  destination. 6 new regression tests, all reproduced failing
  pre-fix. SBF rebuilt: solana_lock.so `d1ab3d00…`
  (212,136 B), machine_bridge.so `ea5e5cf9…` (215,512 B).
- **A-F1 fix (2026-11-20, commit `1b0f96c`):** the round-7
  surviving High — attestation expiry compared the destination
  chain's slot against a source-chain expiry. Expiry is now
  timestamp-based: `BridgeMessage` format version 2 (203-byte
  canonical encoding, `MACHINE-BRIDGE-V2` separator),
  `expiry_timestamp` + signed `source_timestamp` derived from
  the source transaction's block time, destination programs
  validating against their own Clock `unix_timestamp`;
  lifetime bounded to (0, 400,000] s with far-future and
  post-dating rejection. The defect trace was reproduced
  before the fix (honest attestation rejected at a diverged
  destination slot) and is pinned by 2 new regression tests.
  SBF rebuilt: solana_lock.so `42bbfadc…` (212,840 B),
  machine_bridge.so `e8b85254…` (215,832 B).
- **Receipt indexer (2026-11-18, commit `e0d0a24`):** new
  off-chain crate `crates/receipt-indexer/` (binary
  `receipt-index`) performing the binding BRIDGE.md 4.5 leaves to
  indexers: it derives a crossing receipt's PDA, parses the
  receipt account strictly, resolves the PDA's creating
  transaction as the oldest successful signature for the address,
  and verifies that transaction executed in exactly the slot the
  receipt records with a destination-program instruction writing
  the PDA. 15 tests: 14 library (the tamper matrix over the
  account, signature-list, and transaction views) and 1
  end-to-end (a real program-test MintWrapped crossing plus a
  failed on-chain replay of the same nonce, bound in-process and
  through the shipped binary). See `RECEIPT-INDEXER.md`.
  Off-chain only — no program change, no SBF rebuild.
- **Tamper-evident relayer audit log (2026-11-17, commit
  `1a76c91`):** new `audit_log` module in the relayer crate plus a
  `relayer-log` inspector binary. Every sign, quorum collection,
  non-routine collection failure, dry run, and submission is
  appended to a hash-chained JSON Lines log; the relayer fails
  closed if the existing log does not verify. 13 new tests,
  including the pinned honest limit: truncation and a wholesale
  recomputed rewrite pass internal verification and are caught
  only against a published checkpoint (`relayer-log head` /
  `verify --checkpoint`). The wire format was cross-checked
  against an independent Python implementation of the hash
  construction, whose output the shipped binary verifies. See
  `RELAYER-LOG.md`. Off-chain only — no program change, no SBF
  rebuild.
- **LaunchGuard CloseVault (2026-11-17, commit `8e84d18`):** new
  creator-only instruction, gated on the record's Released state,
  that sweeps residual tokens (sent into the vault by anyone —
  Release moves exactly `locked_amount`) back to the creator and
  closes the vault account. No path to the lock: locked or
  expired-but-unreleased launches always fail it. 5 new
  adversarial tests; the external review's missing-PDA-check
  claim was verified already fixed since `93b5cd9`. Details in
  `programs/VERIFY.md` and the AUDIT.md scope update.
- **Crossing proof bundles (2026-11-17, commit `c90ade0`):** new
  off-chain crate `crates/proof-bundle/` (binary `crossing-proof`)
  exports one self-contained JSON bundle per crossing — receipt,
  canonical message, attestation set, stated relayer set and
  quorum — and verifies it fully offline: the message hash is
  recomputed from the canonical encoding, the receipt is compared
  field-by-field against the message, and the signatures are
  re-verified with bridge-common's own `verify_attestation`, with
  the claimed quorum pinned to the protocol constant so a bundle
  cannot weaken the rules it claims to satisfy. 19 tests: 18
  library (valid bundles, JSON round-trip, the tamper matrix) and
  1 end-to-end (a real crossing through the drill harness,
  verified in-process and through the shipped binary). See
  `PROOF-BUNDLE.md`. Off-chain only — no program change, no SBF
  rebuild.
- **Self-audit fixes (2026-11-16, commit `7cd6f5b`):** Initialize
  in both bridge programs is bound to the program's upgrade
  authority (ProgramData account required; machine-bridge also
  validates the wrapped mint account); PDA creation is
  create-or-adopt in all three programs; one shared member-set
  validator serves Initialize and SetRelayers; the token program
  is pinned in the four value handlers; the relayer's
  source-transfer check is bound to the record's creating
  instruction. See STATUS.md P25 and SECURITY-CHANGELOG.md.
- **Verification path:** attestation verification uses the ed25519
  precompile + instructions sysvar (changed 2026-11-14; the original
  sections below describe the earlier in-program dalek path's runs).
- **Caps:** absolute caps on both programs plus dynamic caps on
  unlocks (1% of locked total per tx, 5% per 216,000-slot window),
  tightened to a quarter of those limits for one window after any
  unpause (added 2026-11-15).
- **Crossing receipts:** every successful MintWrapped / Unlock now
  also writes a permanent receipt PDA (message hash, source tx,
  amount, recipient, slot, timestamp), derived from
  (direction, nonce) — see the 2026-11-15 addendum below.
- **Compute units (measured on the real SBF build, 2026-11-16):**
  MintWrapped (3-of-5) 84,952 execution CU + 7,200 precompile =
  **92,152 total**; Unlock (3-of-5) 79,582 + 7,200 = **86,782
  total**; Burn 15,054 execution; Deposit 16,719 execution.
  `compute_units_consumed` does NOT include the ed25519 precompile
  charge (proved: a 3-signature precompile-only transaction reports
  0 CU); the cost-model charge is 2,400 CU/signature (strict).
  Three ed25519 verifies fit the 200,000 CU budget comfortably
  (~46% / ~43%). The old 409/676 figures in the log below were
  native-execution measurements, which meter almost nothing — see
  the corrected CU section.
- **SBF (rebuilt 2026-11-16, self-audit fixes):** solana_lock.so
  `46c58613…` (210,256 B), machine_bridge.so `8e4501e2…`
  (211,856 B), launch_guard.so `cda71121…` (126,512 B — the
  LaunchGuard rebuild left outstanding by the forgery patch is
  now done). **launch_guard.so rebuilt again 2026-11-17
  (CloseVault, `8e84d18`): `915dc769…` (135,488 B). Both bridge
  binaries rebuilt 2026-11-17 (debate round 1, `0efccbc`):
  solana_lock.so `e0586b19…` (211,648 B), machine_bridge.so
  `56d23895…` (213,880 B).** Full hashes in
  `bridge/deploy/SBF-BUILD-NOTES.md`.
  Not deployed anywhere; no asset has crossed; no audit.

Toolchain: cargo 1.100.0 (3acc79c0c 2026-11-05), rustc 1.100.0
(0ef1bc386 2026-11-09). SDK: `solana-program = "2.3"` (matches Agave
2.3.13), `spl-token = "4"`, `solana-program-test = "2.3"`.

## Build

```
$ cargo build --release
warning: `machine-bridge` (lib) generated 5 warnings
warning: `solana-lock` (lib) generated 5 warnings
    Finished `release` profile [optimized] target(s) in 0.34s
```

The 5 warnings per program are upstream, not code issues: `unexpected
cfg` values from the solana-program `entrypoint!` macro
(`custom-heap`, `custom-panic`, `solana`) and a deprecation notice for
`solana_program::system_instruction` (the SDK suggests
`solana_system_interface` instead).

## Tests

```
$ cargo test --release
cargo exit: 0
```

Per-suite results (from the full run):

```
test result: ok. 15 passed; 0 failed  — bridge-common unit tests
test result: ok. 9 passed; 0 failed   — machine-bridge integration tests
test result: ok. 10 passed; 0 failed  — solana-lock integration tests
```

Total: **34 passed, 0 failed.**

### bridge-common unit tests (15)

```
test tests::admin_threshold_enforced ... ok
test tests::canonical_encoding_is_stable_and_195_bytes ... ok
test tests::daily_cap_window_resets ... ok
test tests::config_serializes_to_declared_len ... ok
test tests::expired_attestation_rejected ... ok
test tests::duplicate_relayer_signature_rejected ... ok
test tests::pause_authority_two_relayers_or_two_admins ... ok
test tests::kill_switch_blocks_mint_but_not_burn ... ok
test tests::unknown_relayer_rejected ... ok
test tests::tampered_signature_rejected ... ok
test tests::two_of_5_fails_quorum ... ok
test tests::wrong_direction_rejected ... ok
test tests::valid_3_of_5_attestation_passes ... ok
test tests::valid_5_of_5_attestation_passes ... ok
test tests::wrong_version_rejected ... ok
```

### machine-bridge integration tests (9, program-test + real SPL token)

```
test valid_3_of_5_claim_mints ... ok
test replay_of_used_nonce_fails ... ok
test two_of_5_claim_fails ... ok
test over_per_tx_cap_fails ... ok
test over_daily_cap_fails ... ok
test paused_bridge_rejects_mints ... ok
test kill_switch_is_permanent_and_burns_stay_open ... ok
test admin_signatures_cannot_mint ... ok
test burn_records_event_and_rejects_replay ... ok
```

### solana-lock integration tests (10, program-test + real SPL token)

```
test deposit_locks_tokens_in_vault ... ok
test deposit_same_nonce_twice_fails ... ok
test deposit_over_vault_ceiling_fails ... ok
test unlock_with_valid_3_of_5_burn_attestation ... ok
test unlock_replay_fails ... ok
test unlock_2_of_5_fails ... ok
test unlock_over_per_tx_cap_fails ... ok
test unlock_over_daily_cap_fails ... ok
test admin_signatures_cannot_unlock ... ok
test pause_blocks_deposit_and_unlock ... ok
```

## What was NOT verified

- **SBF build: done 2026-11-14** (after the original verification).
  Both programs compile to deployable sBPF binaries with the Agave
  2.3.13 toolchain: `solana_lock.so` sha256
  `a229cb6ff0d3b7992a576891be375429474167c8d11ccca9b17b5ae8ad554090`,
  `machine_bridge.so` sha256
  `672b42a8ca9562ecd6c535833b4a65143f6485f25c12f9fb6dd5e8b417402095`.
  The build needs a program-only lockfile (the platform-tools cargo
  1.84 cannot parse the edition2024 crates in the unified workspace
  lock) — full method, pins and caveats in
  `bridge/deploy/SBF-BUILD-NOTES.md`, lockfile saved as
  `bridge/deploy/sbf-Cargo.lock`. The binaries are not deployed
  anywhere; deployment remains gated on BRIDGE.md section 5.
- **No deployment.** Neither program is deployed anywhere; no relayer
  exists; no asset has crossed.
- **No audit.** See README.md honest limits.

## Issues found and fixed during the build

1. `spl-token` ships its own entrypoint; linking it into the program
   `cdylib` collided on the `entrypoint` symbol. Fixed with the
   `no-entrypoint` feature on the spl-token dependency.
2. borsh 1.8 removed `try_to_vec` from the `BorshSerialize` trait;
   switched to the free function `borsh::to_vec`.
3. `solana-program-test` 2.3 dropped the native-processor API;
   integration tests use the `solana_program_test::processor!` macro to
   wrap Rust processors as builtin functions.
4. program-test reuses the blockhash within a slot, so a replayed
   transaction with an identical signature returns the cached success
   instead of re-executing. Replay tests warp the slot before the
   second attempt (see `warp()` in the test files). The on-chain nonce
   check was verified to exist independently: after a first unlock the
   nonce PDA is present, program-owned, 89 bytes, discriminator 2.
5. `rand` was a transitive-only dependency; added as a dev-dependency
   for the unit tests.

---

## Update — 2026-11-14: relayer

New crate `crates/relayer/` (off-chain relayer, see its README).
Verified in the bridge workspace directory:

```
$ cargo test --release -p relayer
test result: ok. 20 passed; 0 failed  — relayer unit tests
test result: ok. 2 passed; 0 failed   — relayer integration tests
                                       (deposit -> 3-of-5 quorum ->
                                        MintWrapped instruction;
                                        expired attestation rejected)
```

Total for the relayer: **22 passed, 0 failed.**

One compile error was found and fixed during this build (ureq 2
`send_json` returns a `Response`; the JSON body is read with
`.into_json()`), recorded here the same way as the program issues
above.

### What was NOT verified (relayer)
- **No live run.** No relayer has watched a real deposit or burn:
  there is no deployed bridge and no public Machine RPC yet.
- **SBF build** — done for the programs 2026-11-14, see above; the
  relayer is an off-chain binary and has no SBF target.
- **No deployment, no audit** (unchanged from above).
Full bridge workspace, same day:

```
$ cargo test --release        # whole bridge workspace
test result: ok. 15 passed  — bridge-common unit
test result: ok. 9 passed   — machine-bridge integration
test result: ok. 10 passed  — solana-lock integration
test result: ok. 20 passed  — relayer unit
test result: ok. 2 passed   — relayer integration
Total: 56 passed, 0 failed. Exit code 0.
```

---

## Update — 2026-11-14 (evening): external code-review changes

Implemented from an external review of the bridge code:

1. **Attestation verification now uses the ed25519 precompile +
   instructions sysvar** (the Solana-standard path) instead of
   in-program ed25519-dalek verification. The submitting transaction
   carries an ed25519-program instruction with the quorum signatures
   over the message hash; the programs read it back from the sysvar
   and require >= quorum *distinct registered* relayers on exactly
   this hash. Signatures no longer travel inside the program
   instruction payloads. (The relayer still uses dalek off-chain to
   re-verify peers' attestations before aggregating.)
2. **Dynamic percentage caps** added on top of the absolute caps, per
   BRIDGE.md §4.4: on the Solana unlock path each transaction is
   limited to 1% of the vault's locked total and each window to 5%.
   The daily window is now **slot-based** (216,000 slots) instead of
   wall-clock. The mint side keeps absolute caps only — the wrapped
   supply is created by the mints themselves, so a percentage rule
   has no independent reference there; the Machine program now tracks
   total wrapped supply outstanding on-chain for reconciliation.
3. **Adversarial tests added:** mint at the exact expiry slot passes,
   one slot later fails; reordered signature sets mint/unlock;
   a duplicated relayer signature counts once (quorum not reached);
   a nonce jump 1 -> 1,000,000 mints; unlocks at exactly the dynamic
   per-tx (1%) and daily (5%) boundaries pass, one unit over fails.

```
$ cargo test --release        # whole bridge workspace
test result: ok. 17 passed  — bridge-common unit
test result: ok. 14 passed  — machine-bridge integration
test result: ok. 13 passed  — solana-lock integration
test result: ok. 20 passed  — relayer unit
test result: ok. 2 passed   — relayer integration
Total: 66 passed, 0 failed. Exit code 0.
```

### Compute units — CORRECTED 2026-11-16 (measured on the real SBF build)

**The 409 CU figures below this section were wrong for SBF.**
They were measured with `process_transaction_with_metadata` in
program-test, which executes the programs natively: native
execution meters almost nothing (program entry + syscalls only),
so a full MintWrapped transaction reported 409 CU. Those numbers
say nothing about on-chain cost and are kept below only as a
dated log. The real numbers, measured 2026-11-16 on the SBF
build (`cargo build-sbf`, platform-tools v1.48, lockfile verified
byte-identical to `bridge/deploy/sbf-Cargo.lock`), are:

| Instruction | Execution CU (`compute_units_consumed`) | ed25519 precompile (cost model) | Full on-chain total |
|---|---|---|---|
| MintWrapped (3-of-5) | 84,952 | 3 × 2,400 = 7,200 | **92,152** |
| Unlock (3-of-5) | 79,582 | 3 × 2,400 = 7,200 | **86,782** |
| Burn | 15,054 | — | 15,054 |
| Deposit | 16,719 | — | 16,719 |

**Key finding: `compute_units_consumed` does NOT include the
ed25519 precompile charge.** Proved with a new test
(`bridge/crates/solana-lock/tests/precompile_cost.rs`,
`precompile_charge_excluded_from_execution_cu`): a transaction
containing only a 3-signature ed25519 instruction succeeds and
reports **0 CU**. The precompile charge lives in the cost model
only: 2,280 CU/signature non-strict, **2,400 CU/signature
strict** (solana-cost-model 2.3.13; strict applies — all features
are active by default). The totals above use the strict figure.

**Verdict: three ed25519 verifies in one crossing fit the compute
limit comfortably** — 92,152 / 86,782 CU is ~46% / ~43% of the
200,000 CU default transaction budget, far under the 1,400,000
maximum. Even a 5-of-5 crossing (~96,952 / ~91,582 CU) fits.
The CU test `println!` labels used to say "(incl. ed25519
precompile)" — that was wrong and is fixed: execution CU
excludes the precompile charge, and the labels now say so.

### SBF rebuild (same day, new sources)

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `solana_lock.so` | 205,736 | `05ab551aa74d916b886cd77d9d1cbc977437917f6e24b30361d022a67b9be6a6` |
| `machine_bridge.so` | 206,016 | `eacc1a86a10095e34688487eec019a206ce975ce57bc08610bc019191478da21` |

Both ~41% smaller than the previous build (349,952 / 350,192 bytes)
— the in-program signature-verification code is gone. Same
program-only lockfile method as `bridge/deploy/SBF-BUILD-NOTES.md`;
the scratch tree's lockfile was verified byte-identical to the
saved `bridge/deploy/sbf-Cargo.lock` (pins: blake3 1.5.5, zeroize
1.8.1, base64ct 1.6.0, indexmap 2.13.1, proc-macro-crate 3.3.0).
Still not deployed anywhere.

### Addendum — same evening, follow-up review items

- **Burn CU measured:** full Burn transaction reports **409 CU**
  under the same program-test native execution (test
  `compute_units_burn`) — the mint/unlock/burn figures are now all
  recorded above; SBF-execution measurement remains a testnet-host
  task.
- **Wrong-destination-account tests** (machine-bridge suite now 17):
  `mint_to_wrong_owner_account_fails_and_nonce_survives` and
  `mint_to_wrong_mint_account_fails` prove the on-chain checks the
  relayer's operator-supplied destination account relies on.
- **Bridge workspace total is now 69 passed, 0 failed** (17
  bridge-common + 17 machine-bridge + 13 solana-lock + 22 relayer).
- Top-level docs (BRIDGE.md status line, README.md, ECOSYSTEM.md,
  LAUNCH_CHECKLIST.md, STATUS.md) were synced to the current counts
  and the precompile verification path.

### Addendum — localnet checklist round (2026-11-14, later)

- **Adversarial tests added:** attestations signed over a hash with
  the wrong domain separator, and over a hash of a truncated
  (100-of-195-byte) canonical encoding, both rejected with
  `MissingAttestation` (machine-bridge suite now 19). Dynamic-cap
  boundary coverage confirmed: exactly 1% of locked total unlocks,
  1% + 1 unit fails. LaunchGuard gained freeze-authority-kept and
  9.99%-lock rejections (suite now 10, `programs/VERIFY.md`).
- **Deposit CU measured:** 409 in program-test native execution —
  all four bridge instructions now have recorded figures above.
- **Full local end-to-end drill:** `cargo test --release -p relayer
  --test full_drill` passes all four phases (deposit→mint,
  pause-mid-flow, burn→unlock, disagreement blocks quorum). See
  `bridge/drills/LOCAL-DRILL.md`. Linking both programs into one
  test binary required gating each program's `entrypoint!` behind
  a `no-entrypoint` feature (off by default; never used for SBF).
- **Clippy:** `--release --all-targets` on both workspaces; the
  easy own-code warnings fixed (needless `mut` in two CU tests,
  an `ok_or_else` closure, a test helper's name, a deprecated
  keypair constructor in the drill). Remaining warnings are
  upstream SDK deprecations and macro cfg notes.
- **SBF re-confirmed:** all three programs rebuilt with the current
  lockfiles; hashes **unchanged** — solana_lock.so `05ab551a…`,
  machine_bridge.so `eacc1a86…`, launch_guard.so `68cba8aa…`.
- **Bridge workspace total is now 73 passed, 0 failed.**

### Addendum — fuzzing round (2026-11-14, later still)

- **cargo-fuzz targets added** (`bridge/fuzz/`): `precompile_parser`
  (the AUDIT.md Scope A priority parser), `message_decode`,
  `daily_usage`. First runs: message_decode 7.35M executions clean;
  daily_usage 19.1M clean; **precompile_parser found a crash
  immediately** — empty instructions-sysvar data panicked inside
  the upstream sysvar helper `load_current_index`
  (`data[len-2..len]` on an empty slice).
- **Fix:** bridge-common no longer uses the upstream
  load_current_index / load_instruction_at helpers in
  `verify_precompile_attestation`; the sysvar is parsed in-crate
  with every read bounds-checked, and malformed shapes return
  `MissingAttestation`. Regression test:
  `malformed_sysvar_is_missing_attestation_never_a_panic`
  (bridge-common suite now 18). Post-fix fuzz run: 7.77M
  executions, no crash. On-chain reachability note: a real
  runtime always supplies a well-formed sysvar, so this was a
  robustness/audit-surface bug, not a live exploit path — exactly
  the class fuzzing exists to catch before an auditor does.
- **SBF rebuilt** with the parser fix: solana_lock.so
  `2b2beeb6…` (201,744 B), machine_bridge.so `42cae7f0…`
  (202,024 B); launch_guard.so unchanged (`68cba8aa…`).
- **Bridge workspace total is now 74 passed, 0 failed.** A root
  `Makefile` (`make verify`) and a GitHub Actions CI workflow
  (`.github/workflows/ci.yml`, both Rust workspaces on push/PR)
  were added in the same round.

## Update 2026-11-15 — nonce-gate hardening (external review follow-up)

- An external review (recorded in AUDIT.md) noted that replay
  protection depended on each processor remembering to check the
  nonce PDA, because `verify_precompile_attestation` verifies
  signatures only. The check now lives in one shared helper,
  `bridge_common::check_nonce_unprocessed`, and all four
  value-moving paths (deposit, burn, mint, unlock) route through
  it. Behavior is unchanged — same errors
  (`InvalidAccountData` / `NonceAlreadyProcessed`), same order of
  checks — but the gate can no longer be half-applied by a future
  caller.
- Four unit tests added for the helper (fresh account passes and
  returns the bump; program-owned account is
  `NonceAlreadyProcessed`; wrong account is `InvalidAccountData`;
  the two directions keep separate nonce registries). The existing
  end-to-end replay tests for mint, unlock, burn, and deposit are
  unchanged and still pass.
- **Bridge workspace total is now 78 passed, 0 failed**
  (bridge-common 22, machine-bridge 19, solana-lock 14,
  relayer 23).
- **SBF rebuilt:** solana_lock.so
  `f2bdc178495af0db34298178865fd9c9408f07f152169973c68a5ebd82b8aa41`
  (201,704 B), machine_bridge.so
  `9570f07d503092fe586f8212987996274807fbe6d5de6f73558f8ed8cb73abb6`
  (202,016 B); launch_guard.so unchanged (`68cba8aa…`,
  111,440 B).

### Addendum — hardening + features round (2026-11-15, later)

Two external improvement lists and a feature request were actioned
in one round. Bridge workspace **93 passed, 0 failed**
(bridge-common 33, machine-bridge 21, solana-lock 15, relayer 24);
LaunchGuard **13 passed**; fork suite 23 OK; `make verify` green.

- **Tests added:** hash-binding (every BridgeMessage field is
  covered by the attested hash), cap-overflow boundaries, relayer
  rotation end-to-end (below-threshold rejection, duplicate-key
  rejection, old set fails / new set mints after rotation),
  LaunchGuard stranger-cannot-extend and cannot-regain-authorities,
  and a 45-crossing volume drill that reconciles exactly
  (`bridge/drills/last-drill.md`, written by the drill itself).
- **Crossing receipts:** MintWrapped and Unlock each create a
  receipt PDA (BRIDGE.md §4.5) in the same transaction;
  integration tests read the receipt back and verify every field.
- **Automatic cap tightening:** unpause sets
  `tightened_until_slot = slot + 216,000` in the lock config;
  unlocks inside the window are held to 0.25% / 1.25% dynamic
  caps. Tested end to end: a 0.5% unlock is rejected while
  tightened, exactly-0.25% passes, and after warping past the
  deadline the same 0.5% unlock passes.
- **Compute units changed** (program-test, native execution):
  MintWrapped 409 → **676**, Unlock 409 → **676** (the receipt
  account creation); Deposit and Burn unchanged at 409.
- **SBF rebuilt:** solana_lock.so
  `1bc7c3a904d73ee83ea021f209677f130135fe7c62058a9049047e7ad90cfd9b`
  (204,288 B), machine_bridge.so
  `eccd1686af3fda6a07d98f9be9b385401396289e79a6243624c5ea3f530276a8`
  (204,024 B), launch_guard.so
  `405a0075790224ee2e00c34762d2e561d5ac401bb29c0420030afce1f51dd367`
  (115,208 B). Not deployed.

### Addendum — relayer source-transfer verification (2026-11-16)

Trust audit of the relayer watcher, prompted by the Coreum bridge
drain (Aug 2026). Bridge workspace **99 passed, 0 failed**
(bridge-common 33, machine-bridge 21, solana-lock 15, relayer 30);
LaunchGuard **23 passed**; fork suite 23 OK; `make verify` green.

- **Finding:** the `EventRecord` is sound — the lock program
  transfers into the vault before writing it — but the watcher's
  sender extraction (`sender_from_parsed_transaction`) only
  scanned top-level instructions. The deposit's token transfer is
  a CPI of the Deposit instruction, so it lives in
  `meta.innerInstructions`: on a real deposit transaction the old
  code found no transfer and refused to sign, and a batched
  transaction with an unrelated top-level transfer would have
  yielded the wrong sender in the attested message (and the
  permanent receipt).
- **Fix (off-chain only):** `sender_from_verified_source_transfer`
  scans top-level and inner instructions and requires the transfer
  behind the record — into the vault for a deposit, burning the
  wrapped mint for a burn — for exactly the record's amount.
  Anything else fails closed (`SourceTransferNotVerified`) and no
  signature is produced. The watcher reads the vault / wrapped
  mint from the source program's config account at startup.
  No program change, no SBF rebuild, no audit-scope change.
- **Tests added (8, replacing the 2 old sender-extraction
  tests):** sender found in inner instructions (deposit and burn),
  Coreum-style phantom deposit rejected, inner transfer to the
  wrong destination rejected, wrong amount rejected, decoy
  top-level transfer ignored, burn with the wrong mint rejected,
  missing instructions still an error.

### Addendum — malformed-attestation fail-closed tests + fuzz round 2 (2026-11-16)

Bridge workspace **108 passed, 0 failed** (bridge-common 33,
machine-bridge 21, solana-lock 15, relayer 39); LaunchGuard
**23 passed**; fork suite 23 OK; `make verify` green against
`266f6c5`.

- **Relayer exchange robustness (9 new tests, tests only):** the
  collector's inputs are files written by *other* relayers into a
  shared directory — untrusted input. Tests prove collection fails
  closed on a truncated file, garbage bytes, an empty file, a
  wrong-length signature field, a forged signature (caught at the
  final bridge-common re-verification), a tampered stored message
  hash, and a valid attestation for another nonce wearing this
  nonce's file name (skipped, never counted). A malformed file
  aborts collection even when a full valid quorum is also present:
  strict fail-closed, no partial attestation is ever produced.
  No behavior change was needed — the collector already failed
  closed in every shape; the tests pin it.
- **Fuzz round 2:** new `crossing_receipt` target in
  `bridge/fuzz/` — `receipt_pda` derivation (total, deterministic)
  and the `CrossingReceipt` borsh round-trip at its declared
  `LEN`, plus canonicality of arbitrary-byte decodes. Bounded
  session (`cargo fuzz run crossing_receipt -- -max_total_time=300
  -runs=5000000`): **2,405,767 executions in 301 s, no findings**.

### Addendum — hardening pass (2026-11-16)

Bridge workspace **109 passed, 0 failed** (bridge-common 33,
machine-bridge 21, solana-lock 15, precompile-cost 1, relayer
39); LaunchGuard **26 passed**; fork suite 23 OK; full
`make verify` green — **158 passed, 0 failed**.

- **ed25519 compute units measured on the real SBF build** (see
  the corrected CU section above): MintWrapped 92,152 CU total,
  Unlock 86,782 CU total — three verifies fit the 200,000 CU
  budget comfortably. The old 409/676 figures were
  native-execution measurements, not SBF costs.
- **SBF rebuilt** (same program-only lockfile method as
  `bridge/deploy/SBF-BUILD-NOTES.md`; lockfile verified
  byte-identical to `bridge/deploy/sbf-Cargo.lock`):
  solana_lock.so `cda1b66ffb32d2c6fe94e0ccbd96ffd95736a78845667a33ed3dc8a64cde8d35`
  (204,272 B), machine_bridge.so
  `ecd19d0f8765eb1e333323526cf11e1abd760f98a87bf42d6e13ec55f5eecb77`
  (204,016 B). Hashes differ from the 2026-11-15 record
  (f2bdc178/9570f07d) because commits `8369f99` (receipts, cap
  tightening) and `b9d98d3` (precompile-parser adversarial tests)
  changed program code after that rebuild. Rebuilding solana-lock
  in place reproduced the identical hash (deterministic).
  launch_guard.so was NOT rebuilt in this pass (see SBF-BUILD-NOTES.md).
- **SPL Token as a builtin in tests:** the bridge tests now
  register SPL Token via `add_builtin_program` (lock_tests.rs,
  mint_tests.rs, relayer full_drill.rs) instead of `add_program` —
  token is a builtin program on-chain, and `add_program` panics
  in SBF mode because no `spl_token.so` exists. Native behaviour
  unchanged.
- **New test:** `bridge/crates/solana-lock/tests/precompile_cost.rs`
  pins the finding that a precompile-only transaction reports
  0 execution CU.
- **LaunchGuard forgery patch** (in `programs/`, see
  `STATUS.md` P23 and `HARDENING-2026-11-16.md`): `load_record`
  rejects foreign-owned accounts; release/extend verify the record
  is the derived PDA. 3 new adversarial tests; mutation check
  done (removing either check makes its test fail).
