# Bridge Verification

Real commands and their real output. Nothing here is claimed without a
run behind it. Verified 2026-11-14 ~11:07 UTC, in the bridge workspace
directory.

## Current state (2026-11-15, latest commits — read this first)

The sections below are the dated verification log; this summary is
the current state and supersedes their numbers.

- **Tests:** bridge workspace **84 passed, 0 failed** (28
  bridge-common unit + 19 machine-bridge + 14 solana-lock + 23
  relayer, including the full local end-to-end drill);
  LaunchGuard **10 passed** (`programs/VERIFY.md`); fork suite
  23 OK.
- **Verification path:** attestation verification uses the ed25519
  precompile + instructions sysvar (changed 2026-11-14; the original
  sections below describe the earlier in-program dalek path's runs).
- **Caps:** absolute caps on both programs plus dynamic caps on
  unlocks (1% of locked total per tx, 5% per 216,000-slot window).
- **Compute units (program-test, native execution):** MintWrapped
  409, Unlock 409, Burn 409, Deposit 409 — full transactions; see
  the CU section in the code-review update below. SBF-execution CU
  is a testnet-host measurement, still outstanding.
- **SBF:** solana_lock.so `f2bdc178…` (201,704 B), machine_bridge.so
  `9570f07d…` (202,016 B), launch_guard.so `68cba8aa…` (111,440 B).
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

### Compute units (program-test, native execution)

Measured with `process_transaction_with_metadata` (tests
`compute_units_mint_wrapped`, `compute_units_unlock`):

- Full MintWrapped transaction (ed25519 precompile + bridge
  instruction, 3-of-5): **409 CU**
- Full Unlock transaction (same shape): **409 CU**

Program-test executes the programs natively, so these figures are
not SBF-execution costs; what they show is that the design carries
almost no on-chain verification overhead of its own — signature
verification is the runtime precompile's job now. SBF-execution CU
will be measured on the Phase 1 testnet host and recorded here.

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
