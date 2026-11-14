# Bridge Verification

Real commands and their real output. Nothing here is claimed without a
run behind it. Verified 2026-11-14 ~11:07 UTC, in the bridge workspace
directory.

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

- **No SBF build.** `cargo build --release` produces host libraries.
  The deployable `.so` files need `cargo build-sbf` (the toolchain is
  present in the Agave release directory, but the build was not run —
  deployment is out of scope until the BRIDGE.md section 5 prerequisites
  exist).
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
