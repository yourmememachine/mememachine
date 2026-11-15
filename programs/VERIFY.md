# LaunchGuard — verification record

Design: `programs/LAUNCH-GUARD.md`. Status: **local only — not
deployed, not audited** (in AUDIT.md Scope A; gates its own mainnet
use, since it custodies locked creator tokens).

## Tests (2026-11-14)

```
$ cd programs && cargo test --release
test register_locks_ten_percent_for_thirty_days ... ok
test register_rejected_if_mint_authority_kept ... ok
test register_rejected_if_lock_too_small_or_too_short ... ok
test release_before_expiry_fails_after_expiry_succeeds ... ok
test extend_lock_pushes_release_later ... ok
test stranger_cannot_release ... ok
Total: 6 passed, 0 failed.
```

Program-test against the real SPL token program: actual mint
creation, authority revocation via `set_authority`, PDA vault,
token movements, and slot warps are all exercised end to end.

## Design change made during verification: slot-based locks

v1 as first written measured locks in wall-clock seconds via the
Clock sysvar's `unix_timestamp`. Testing exposed two problems:

1. In program-test, `warp_to_slot` advances the slot but the
   timestamp does not move (measured: +6,800,000 slots → +0 s), and
   a `set_sysvar` clock override does not survive into the bank
   that processes the next transaction. Time-based release could
   not be exercised honestly in the local harness.
2. More importantly, validator timestamps are the weaker primitive
   on a real chain too: they can drift within protocol tolerance.

Locks are therefore counted in **slots** (30 days = 6,480,000 slots
at the 400 ms target), the same determinism rule the bridge uses
for message expiry. Expiry is exactly computable from the
registration slot, and the release path is fully test-covered.

Two smaller build fixes are recorded for the auditor's benefit:
the shared-lifetime annotation on the PDA-account helper (same
shape as the bridge programs), and borsh's free `to_vec` function
in tests (borsh 1.8 removed the trait method — same fix as the
bridge workspace, see bridge/VERIFY.md).

## SBF build (2026-11-14)

Same method as the bridge programs (program-only lockfile, see
`bridge/deploy/SBF-BUILD-NOTES.md`):

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `launch_guard.so` | 111,056 | `bfe87a60b2f80cbb71e380e0468d3c4afd8e59161e53ae05ca44fa6525346457` |

ELF 64-bit sBPF, stripped. Lockfile saved as `programs/sbf-Cargo.lock`;
pins beyond the workspace lock: `cpufeatures 0.3.1 → 0.2.17`,
`crypto-common 0.2.2 → 0.1.7`, `indexmap 2.14.2 → 2.13.1`,
`zeroize 1.9.1 → 1.8.2`, `proc-macro-crate 3.5.0 → 3.3.0`
(drops the edition2024 `toml_edit 0.25` chain). Not deployed.

---

## Update — 2026-11-14 (evening): Token-2022 stance + rebuild

From the same external review: registration now **explicitly rejects
Token-2022 mints** (`LaunchError::UnsupportedTokenProgram`). v1 of
the standard is classic SPL Token only — Token-2022 extensions
(permanent delegate, transfer hooks, mint-close authority) can
reintroduce exactly the supply/control properties LaunchGuard
exists to remove, and they get their own analysis before any
support. The check covers the token-program account, the mint's
owning program, and the creator token account's owning program.

```
$ cd programs && cargo test --release
test register_rejected_for_token2022_mint ... ok   (new)
test compute_units_register ... ok                 (new, measurement)
Total: 8 passed, 0 failed.
```

RegisterAndLock measures **9,508 CU** in program-test (native
execution; SBF-execution figure to be measured on the testnet host).

### SBF rebuild (same day, new sources)

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `launch_guard.so` | 111,440 | `68cba8aab86f1001c1774a37897e931db1e7de68c72e93533bf2703f4e70094a` |

Not deployed.
