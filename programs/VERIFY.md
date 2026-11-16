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

### Addendum — localnet checklist round (2026-11-14, later)

Two adversarial tests added (suite now **10 passed, 0 failed**):
`register_rejected_if_freeze_authority_kept` (mint authority
revoked, freeze authority still live → `FreezeAuthorityNotRevoked`)
and `register_rejected_at_9_99_percent_lock` (99,900 of 1,000,000 →
`LockTooSmall`). SBF rebuild re-confirmed: launch_guard.so
`68cba8aa…`, unchanged.

### Addendum — reputation PDA (2026-11-15, later)

LaunchGuard gained a per-creator **reputation PDA**
(seeds `["reputation", creator]`): `launches_registered` counts
RegisterAndLock completions, `locks_completed` counts releases —
reachable only after a lock's full period elapsed. No instruction
decrements or edits the counters; there is no admin key. Suite now
**13 passed, 0 failed**, including
`creator_reputation_counts_registrations_and_completions` (two
launches by one creator share one PDA: 2 registered; an extend
moves nothing; a release after the full period records 1
completion; a stranger has no reputation account).
RegisterAndLock CU (program-test, native execution): 9,508 →
**9,775**. SBF rebuilt: launch_guard.so
`405a0075790224ee2e00c34762d2e561d5ac401bb29c0420030afce1f51dd367`
(115,208 B). Not deployed.

### Addendum — "rug attempt" adversarial harness (2026-11-16)

A second test file, `crates/launch-guard/tests/adversarial.rs`
(9 tests), written from an attacker's checklist. Each test is one
way a creator, a stranger, or a compromised account tries to get
locked tokens out early, to shorten a lock, to forge a reputation,
or to bypass the program entirely. Suite now **22 passed,
0 failed** (13 integration + 9 adversarial). Tests only — no
program change, so no SBF rebuild and no audit-scope change.

- `attack_release_one_slot_early_fails_at_exact_expiry_succeeds`:
  the expiry boundary is exact — one slot before `lock_until_slot`
  the vault is sealed (`StillLocked`); at the slot itself it opens.
- `attack_double_register_same_mint_fails`: a second registration
  of the same mint cannot create the launch PDA again; one record,
  one vault, reputation still counts 1.
- `attack_stranger_drains_vault_directly_fails`: a stranger calling
  the SPL token program to move vault funds fails — the vault is
  owned by the authority PDA, which only the program can sign for.
- `attack_release_to_attacker_token_account_fails`: a release naming
  an attacker's token account as destination fails (`MintMismatch`);
  the honest release still works afterwards.
- `attack_release_without_reputation_account_fails`: release with a
  nonexistent reputation account fails (`InvalidPda`) and, the
  transaction rolling back, moves no funds.
- `attack_register_with_forged_reputation_fails`: registering with
  another creator's reputation PDA fails (`InvalidPda`); nothing is
  created for the attacker and the victim's counters are untouched.
- `attack_lock_can_only_move_later`: extend by zero fails; extend
  after release fails (`AlreadyReleased`); extend after expiry is
  permitted — it only re-locks the creator's own funds for longer,
  and the old expiry stays sealed until the new date. This is the
  designed direction (locks extend, never shorten), pinned by test
  rather than assumed.
- `attack_two_creators_interleaved_reputations_stay_separate`: two
  creators' interleaved launches keep separate counters; neither
  can release the other's launch or borrow the other's reputation
  PDA.
- `attack_failed_registration_does_not_create_reputation`: a
  registration rejected for a sub-10% lock leaves no reputation
  account and no record; the later successful registration counts
  exactly once.

Two harness notes for future test authors: an identical transaction
resubmitted in the same slot returns the first transaction's cached
result instead of executing, so a repeated attempt needs a slot
warp first; and `warp_to_slot` rejects a target at or behind the
current slot, so expiries that coincide must warp to
`max(target, current + 1)`.

### Addendum — demo launch (2026-11-16)

A third test file, `crates/launch-guard/tests/demo_launch.rs`
(1 test, `demo_launch_full_story`), performs the LaunchGuard demo
launch recorded in `programs/DEMO-LAUNCH.md`: a fresh demo token
(mint `AjTP23T2HhMryZVDPpzVYrTsWHgH6jKkMt9b8cLdn7hc`, supply
1,000,000,000, fixed demo-only keypairs so every run reproduces
the same addresses), both authorities revoked, registered with
100,000,000 (10%) locked from slot 3 until slot 6,480,003
(LaunchRecord PDA `E6CttVSzy3Le4eHaxVWRyeR68HRqHnEkV6GSgUbBqRQi`,
vault PDA `7AQtQMVajVMezY8xUutptYPqyyxo27rFcXLzpVMFim7f`,
reputation PDA `4dnv3qqvVCwBibkpkTLrJr6AN5V6SekECNRofdiZNoH5`).
The rug paths are then attempted live and all fail: mint more
(SPL Token `FixedSupply`), release early (`StillLocked`), shorten
the lock via a zero-slot extend (`LockPeriodTooShort`), release
as a stranger (`NotCreator`); the final state is asserted
unchanged. Suite now **23 passed, 0 failed** (13 integration +
9 adversarial + 1 demo launch). Tests only — no program change,
so no SBF rebuild and no audit-scope change. Localnet
program-test only; the demo token is worthless by construction.

### Addendum — LaunchGuard registry export (2026-11-16)

A new off-chain crate, `crates/registry-export/`, turns the
program's accounts into a public, machine-readable JSON registry:
`launchguard-registry --url <RPC> --program-id <ID> [--verify]`
reads every `LaunchRecord` + `CreatorReputation` via
`getProgramAccounts` and emits mint, creator, locked amount/share,
lock and expiry slots, status (`locked` / `expired_unreleased` /
`released`), and creator reputation per launch. `--verify` also
fetches each vault and mint account and re-checks the registration
invariants (vault covers the locked amount; both authorities
revoked) instead of trusting the record. Parsing is strict (owner
+ discriminator + exact length); foreign accounts are ignored,
never misread. Suite now **37 passed, 0 failed** in `programs/`
(LaunchGuard 26 + registry export 11: 9 lib unit tests covering
the strict parser, statuses, reputation joining, and verify logic,
plus 2 program-test integration tests — two real launches, 10% /
30 days and 25% / 60 days, built from the actual account bytes the
program wrote, then register → warp → release → re-export proving
the `released` status and the empty-vault check). Off-chain
tooling only — no program change, so no SBF rebuild and no
audit-scope change. `make verify` green against `d276c3f`;
repo total **169** (109 bridge workspace + 37 programs + 23 fork).
