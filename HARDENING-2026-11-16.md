# Machine — Code Hardening Pass, 2026-11-16

Two passes on 2026-11-16. The first did the code work (LaunchGuard
forgery patch, ed25519 compute-unit measurement on the real SBF
build, SPL Token builtin fix in the bridge tests) and stopped before
docs, the final full verification, and the mutation check. This
finish pass ran the final verification, the mutation check, updated
the docs, and wrote this file. Nothing was pushed to GitHub; all
changes are on disk in this repo. The validator, ledger, and tower
file were not touched.

## 1. Baseline test counts — `make verify`, 2026-11-16 ~14:00 UTC, exit 0

- Bridge workspace: **108 passed, 0 failed** — bridge-common unit 33,
  machine-bridge integration 21, solana-lock integration 15, relayer
  unit 35, relayer end_to_end 2, full_drill 2.
- LaunchGuard: **23 passed, 0 failed** — integration 13, adversarial 9,
  demo 1.
- Fork suite: **23 passed, 0 failed.**
- **Total: 154.**

## 2. ed25519 compute units — MEASURED on the real SBF build

SBF binaries rebuilt from current sources via the documented
scratch-workspace method (`cargo build-sbf`, platform-tools v1.48,
lockfile verified byte-identical to `bridge/deploy/sbf-Cargo.lock`):

- `solana_lock.so`: 204,272 B, sha256
  `cda1b66ffb32d2c6fe94e0ccbd96ffd95736a78845667a33ed3dc8a64cde8d35`
- `machine_bridge.so`: 204,016 B, sha256
  `ecd19d0f8765eb1e333323526cf11e1abd760f98a87bf42d6e13ec55f5eecb77`
- Rebuilding solana-lock in place reproduced the identical hash
  (deterministic). The hashes differ from the Nov 15 record
  (f2bdc178/9570f07d) because commits `8369f99` (receipts, cap
  tightening) and `b9d98d3` (precompile parser adversarial tests)
  changed program code after the Nov 15 SBF rebuild (`4a83a7a`).

Measured SBF execution CU (program-test with
`BPF_OUT_DIR=/tmp/sbf-scratch/target/deploy`, run twice —
identical both times):

- **MintWrapped (3-of-5): 84,952 CU**
- **Unlock (3-of-5): 79,582 CU**
- Burn: 15,054 CU; Deposit: 16,719 CU

**Critical correction:** `compute_units_consumed` does NOT include
the ed25519 precompile charge. Proved with a new test: a
transaction containing only a 3-signature ed25519 instruction
succeeds and reports **0 CU**. The charge lives in the cost model
only: 2,280 CU/signature non-strict, **2,400 CU/signature strict**
(solana-cost-model 2.3.13; strict applies — all features are active
by default). Full on-chain cost of a 3-of-5 crossing:

- **MintWrapped: 84,952 + 7,200 = 92,152 CU** (91,792 non-strict)
- **Unlock: 79,582 + 7,200 = 86,782 CU** (86,422 non-strict)

**Verdict: three ed25519 verifies in one unlock fit the compute
limit comfortably** — 46% / 43% of the 200,000 CU default
transaction budget, far under the 1,400,000 max. Even 5-of-5
(~96,952 / ~91,582 CU) fits. The old `bridge/VERIFY.md` figures
(409 CU) were native-execution measurements, which meter almost
nothing (program entry + syscalls only); the SBF numbers above are
the real ones.

Note: the SBF `.so` files lived in ephemeral
`/tmp/sbf-scratch/target/deploy/` and are gone now (the scratch
directory did not survive a VM replacement). They are reproducible
via the documented scratch method; the hashes above are the record,
matching existing practice of recording hashes only.

## 3. The LaunchGuard forgery patch (real hole, fixed)

`programs/crates/launch-guard/src/processor.rs`:

1. `load_record` now takes `program_id` and rejects accounts it
   does not own with `InvalidAccountData` before parsing.
2. `process_release` and `process_extend` both verify the record
   account is the derived `["launch", mint]` PDA, else `InvalidPda`.

The forgery was real: a program-owned account holding
valid-looking record bytes for another address would otherwise let
Release drain a victim's vault (the transfer executes before the
reputation check; the write-back would succeed because the
planted account is program-owned).

3 new tests in `programs/crates/launch-guard/tests/adversarial.rs`
(+ helpers: `start_with` genesis injection, `make_mint_keyed`,
`register_keyed`, `release_ix_record`, `extend_ix_record`,
`forged_record`, `expect_instruction_error`):

- `attack_release_with_forged_record_fails` (foreign owner →
  InvalidAccountData, vault untouched)
- `attack_release_with_record_at_wrong_address_fails`
  (program-owned, wrong address → InvalidPda; the attacker first
  registers their own launch so they hold a real reputation, making
  this a true discriminator)
- `attack_extend_with_record_at_wrong_address_fails` (→ InvalidPda,
  real record unchanged)

Adversarial 9 → 12; LaunchGuard total 23 → 26.

### Mutation check (finish pass, 2026-11-16 ~15:25 UTC)

`processor.rs` was backed up to `/tmp/processor.rs.bak` first
(`programs/` is untracked, so git cannot restore it).

- **Mutation A:** release PDA check removed → the wrong-address
  release test **FAILED** (as it must — the test catches the
  missing check). File restored from backup, verified
  byte-identical (sha256 `79b1f2d9…`, `diff` empty, no mutation
  marker left).
- **Mutation B:** owner check in `load_record` removed → the
  forged-record test **FAILED**. File restored from backup,
  verified byte-identical the same way.
- LaunchGuard suite re-run after restoring: **26 passed, 0 failed**
  (integration 13, adversarial 12, demo 1).

## 4. Final test counts — full `make verify`, 2026-11-16 ~15:32 UTC, exit 0

- Bridge workspace: **109 passed, 0 failed** — bridge-common 33,
  machine-bridge integration 21, solana-lock integration 15,
  precompile-cost 1 (new), relayer unit 35, relayer end_to_end 2,
  full_drill 2.
- LaunchGuard: **26 passed, 0 failed** — integration 13,
  adversarial 12, demo 1.
- Fork suite: **23 passed, 0 failed.**
- **Total: 158 passed, 0 failed.** (Baseline was 154; +1 bridge
  precompile test, +3 LaunchGuard adversarial tests.)

## 5. Every file changed

### Code / tests (first pass)

1. `bridge/crates/solana-lock/tests/lock_tests.rs` — SPL Token now
   registered via `add_builtin_program` with a new
   `spl_token_builtin` fn pointer (the `processor!` macro's closure
   can't coerce to the required fn-pointer type due to a
   higher-ranked-lifetime mismatch; the fn is the macro's exact
   body). Reason: token is a builtin program on-chain, and
   `add_program` panics in SBF mode because no `spl_token.so`
   exists. Native behaviour unchanged.
2. `bridge/crates/machine-bridge/tests/mint_tests.rs` — same change.
3. `bridge/crates/relayer/tests/full_drill.rs` — same change in both
   ProgramTests (Solana + Machine sides).
4. `bridge/crates/solana-lock/tests/precompile_cost.rs` (**new**) —
   `precompile_charge_excluded_from_execution_cu`: asserts a
   3-signature precompile-only tx succeeds with 0 CU. Pins the
   assumption the CU table relies on.
5. `programs/crates/launch-guard/src/processor.rs` — the forgery
   patch (§3 above). Incident: the first batch of three parallel
   edits to this file interfered — the owner-check `if` and both
   PDA-check blocks were silently lost (only the comments and
   call-site signature changes survived). All three checks were
   re-applied sequentially and verified with grep + the passing
   suite. Final file state confirmed correct (owner check line 49;
   PDA checks lines 336–337 and 388–389) and re-confirmed by the
   mutation check in the finish pass.
6. `programs/crates/launch-guard/tests/adversarial.rs` — 3 new
   forgery tests + helpers (§3 above).

### Docs + labels (finish pass)

7. `STATUS.md` — header rewritten: verification is now the
   2026-11-16 `make verify` run (158 total), and the live chain is
   honestly **stalled at slot 29,100** since ~22:11 PST 2026-11-14
   (the old header claimed verification "against the running local
   validator" with slot 20,569 from Nov 14). P3 slot evidence
   replaced with the stall + the 20,569 reading kept as history.
   P9 (bridge 70 program tests / 109 workspace, SBF CU numbers),
   P16 (LaunchGuard 26), P19 (12-test harness, forgery tests),
   new **P23** (this hardening pass), Honest limits (109/109 and
   26/26, stall noted), repo state (public repo is BEHIND this
   working tree — hardening changes not yet pushed).
8. `README.md` — status line (stalled at 29,100), block speed now
   historical, honest-limits wording past-tense, Bridge section
   (70 program tests / 109 workspace, SBF CU totals).
9. `bridge/VERIFY.md` — current-state summary (109 / 26 / 158,
   SBF CU totals, new SBF hashes, launch_guard.so rebuild
   outstanding); the CU section corrected with the real SBF table
   and the precompile-charge explanation; new hardening addendum.
10. `bridge/deploy/SBF-BUILD-NOTES.md` — 2026-11-16 rebuild record
    (hashes + sizes), launch_guard.so rebuild outstanding, stale
    "(56/56)" counts updated to 109/109. (One edit in this file
    briefly dropped the 2026-11-15 rebuild record; it was restored
    immediately and all three records verified present in order.)
11. `bridge/crates/machine-bridge/tests/mint_tests.rs` (again) —
    CU `println!` label fixed: "(incl. ed25519 precompile)" was
    wrong; now "(execution CU; +7,200 precompile charge on-chain)".
12. `bridge/crates/solana-lock/tests/lock_tests.rs` (again) — same
    label fix for the Unlock CU line.
13. `HARDENING-2026-11-16.md` (**new**, this file).

## 6. Still open / needs a decision

- **Nothing pushed to GitHub yet.** The hardening changes live
  only in this working tree; the public repo is behind. Push via
  the browser web-upload flow after review (handled by the parent
  agent, not here).
- **launch_guard.so not rebuilt after the forgery patch.** The
  2026-11-15 hash (`405a0075…`, 115,208 B) is stale for the
  current sources. Rebuild outstanding before any LaunchGuard
  deployment.
- **Local validator still stalled at slot 29,100** (since
  ~22:11 PST 2026-11-14). Awaiting the owner's decision: fresh
  restart vs diagnosis. The docs now say so honestly instead of
  claiming a live, advancing chain.
- **SBF binaries are ephemeral.** `/tmp/sbf-scratch/` is gone;
  the `.so` files are reproducible via the documented scratch
  method and the hashes above are the record.
- Out of scope, unchanged: Reddit ban/appeal, Dev3pack setup,
  bridge not deployed, no audit, no asset crossed.
