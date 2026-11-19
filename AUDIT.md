# Machine — Audit Readiness Package

Status: **No independent audit has been performed.** This document
exists so that when an auditor is engaged, everything they need is
in one place, and so the scope is fixed in public before the
engagement — an audit with a quietly narrowed scope is worse than
none, because it looks like assurance.

Last verified commit: `5b37d7b` (2026-11-19; `make verify` green —
bridge workspace 197 passed, LaunchGuard 62 passed, PQ vault 17
passed, registry export 11 passed, fork suite 23 OK). The pin is refreshed after every full verification run;
STATUS.md and BRIDGE.md carry the same pin.

## What must be audited before real value moves

Per BRIDGE.md section 5 item 5, the audit gates the bridge. The
chain configuration gates any public chain launch. Both scopes:

### Scope A — Bridge (gating: no real asset crosses without it)
| Component | Where | Notes for the auditor |
|---|---|---|
| Solana lock program | `bridge/crates/solana-lock/` | Deposit / unlock, vault ceiling, caps, pause. |
| Machine bridge program | `bridge/crates/machine-bridge/` | Mint/burn, kill-switch, no admin mint path exists — verify that claim. |
| Shared logic | `bridge/crates/bridge-common/` | Message format (195-byte canonical encoding), hash, 3-of-5 attestation verification via the ed25519 precompile + instructions sysvar (switched from in-program ed25519-dalek in the 2026-11-14 code-review changes; the precompile-instruction parser in `verify_precompile_attestation` is a priority review target), nonce replay protection, absolute + dynamic percentage caps, authority checks. Scope size at those changes: 4,706 lines of Rust across the five in-scope crates (bridge-common 1,168, solana-lock 547, machine-bridge 518, relayer 2,010, LaunchGuard 463; tests excluded). |
| Off-chain relayer | `bridge/crates/relayer/` | Finalized-only signing rule, deterministic message construction (expiry derived from source slot), quorum collection with disagreement blocking, submitter account ordering. A relayer bug that signs a non-finalized or mis-built message is in scope even though quorum + on-chain checks should contain it. |
| LaunchGuard launch standard | `programs/crates/launch-guard/` | Added 2026-11-14. Gates LaunchGuard's own mainnet use (it custodies locked creator tokens), not the bridge. Verify: registration truly requires revoked mint+freeze authorities, the ≥10% / ≥30-day (6,480,000-slot) lock floor, no early-release or lock-shortening path, PDA authority over the vault. Dust floor added 2026-11-19: a one-time `Initialize` (upgrade authority proven against ProgramData — a hand-mirrored copy of bridge-common's `verify_upgrade_authority` lives in this crate's processor and is a priority review target) creates a config PDA carrying a minimum lock size in raw units that registration enforces on top of the 10% share; verify the floor cannot be bypassed or changed by any later instruction, and that registration fails closed before Initialize. Spec: `programs/LAUNCH-GUARD.md`. |
| PQ Vault | `programs/crates/pq-vault/` | Added 2026-11-19. Gates PQ Vault's own mainnet use (it custodies stored funds), not the bridge. Withdrawals and rotation are authorized only by RFC 8391 WOTS+ one-time signatures; there is no Ed25519 path to the funds. Verify: one-time discipline is program-enforced (strict in-order spend, the index increments before the transfer), rotation cannot be authorized by anything but a one-time signature, exhaustion freezes fail-closed, the chunked signature buffer is bound to the exact withdrawal digest. Spec: `programs/PQ-VAULT.md`; verification record: `programs/PQ-VERIFY.md`. |
| Key management | `bridge/deploy/DEPLOY-BRIDGE.md` step 2 | Fresh keys on deployment hosts, role separation, multisig + 48h timelock for upgrades. Process review, not code. |
| Deployment + drills | `bridge/deploy/`, `bridge/drills/` | Caps translation (`caps.py`), initialization parameters, test-crossing / pause / outage drill results. |

> **Scope update (2026-11-15, after the first inquiries were sent):**
> three features were added to the in-scope programs — crossing
> receipts (a receipt PDA written by MintWrapped and Unlock),
> automatic cap tightening (tightened dynamic caps for one window
> after any unpause of the lock program), and the LaunchGuard
> creator reputation PDA. All are part of Scope A. Auditors should
> quote and review against the current `main` HEAD (pinned at the
> top of this file), not the tree as of the first inquiry.
>
> **Scope update (2026-11-16):** the off-chain relayer's watcher
> was fixed after a trust audit prompted by the Coreum bridge drain
> (Aug 2026). The watcher now verifies the source transfer behind
> each event before signing — into the vault for a deposit, burning
> the wrapped mint for a burn, for exactly the record's amount —
> and refuses to sign otherwise. Off-chain code only, no program
> change, no SBF rebuild; the relayer was already in Scope A.
> Auditors should still quote against the current `main` HEAD.
>
> **Scope update (2026-11-16, later):** tests-only hardening of
> the in-scope relayer — nine new collector tests proving the
> file-based attestation exchange fails closed on malformed files
> (truncated / garbage / empty files, bad or forged signatures,
> tampered message hash, wrong-nonce file; no behavior change was
> needed) — plus a fourth cargo-fuzz target (`crossing_receipt`)
> over bridge-common's receipt PDA derivation and `CrossingReceipt`
> serialization. No program change, no SBF rebuild, no scope
> change; the relayer and bridge-common were already in Scope A.
> Auditors should still quote against the current `main` HEAD.
>
> **Scope update (2026-11-16, later still):** a new off-chain
> registry export (`programs/crates/registry-export/`) that reads
> LaunchGuard's `LaunchRecord` and `CreatorReputation` accounts
> via RPC and emits a public JSON registry of protected launches,
> with an optional `--verify` re-check of vault balances and
> revoked mint authorities. Off-chain tooling only — it reads
> Scope A accounts, it does not touch the program: no program
> change, no SBF rebuild, no scope change. Auditors should still
> quote against the current `main` HEAD.
>
> **Scope update (2026-11-16, self-audit fixes — PROGRAM CHANGES,
> SBF REBUILT):** a self-audit of the in-scope code (REVIEW-1)
> found four issues, all fixed in commit `7cd6f5b` with regression
> tests: (F-1, High) `Initialize` in both bridge programs was
> permissionless — it is now bound to the program's upgrade
> authority (a new ProgramData account in both Initialize account
> lists; new shared `verify_upgrade_authority` in bridge-common),
> and machine-bridge validates the wrapped mint as a real mint
> account; (F-2, Medium) PDA creation in all three programs is
> create-or-adopt, so a pre-funded PDA shell no longer permanently
> blocks a crossing, registration, or deployment; (F-3, Low) one
> shared member-set validator in bridge-common now serves
> `Initialize` and `SetRelayers` in both programs; (F-4, Low)
> LaunchGuard's minimum lock uses ceiling division. Two
> defense-in-depth hardenings landed in the same commit: the token
> program is pinned explicitly in Deposit / Unlock / MintWrapped /
> Burn, and the relayer's source-transfer verification is bound to
> the record's creating instruction. All three SBF binaries were
> rebuilt (hashes in `bridge/deploy/SBF-BUILD-NOTES.md`).
> **Auditors must quote and review against the current `main`
> HEAD (pinned at the top of this file), not any earlier tree** —
> in particular, quotes issued against pre-`7cd6f5b` trees do not
> cover the changed Initialize paths.

> **Scope update (2026-11-17):** a new off-chain crossing proof
> bundle tool (`bridge/crates/proof-bundle/`, binary
> `crossing-proof`; doc `bridge/PROOF-BUNDLE.md`) packages a
> completed crossing's receipt, canonical message, and attestation
> set into one self-contained JSON file and verifies it fully
> offline, re-running bridge-common's own `verify_attestation`
> against the bundle's stated relayer set with the claimed quorum
> pinned to the protocol constant. Off-chain tooling only — it
> reads Scope A data (crossing receipts, relayer attestation
> files); it does not touch any program: **no program change, no
> SBF rebuild, no scope change.** Auditors should still quote
> against the current `main` HEAD.

> **Scope update (2026-11-17, later — PROGRAM CHANGE, SBF
> REBUILT):** LaunchGuard gained a fourth instruction,
> `CloseVault` (commit `8e84d18`, from an external review's
> stuck-funds point). The vault is a normal SPL token account, so
> anyone can transfer extra tokens into it, and `Release` moves
> exactly `locked_amount` — the excess was permanently stuck.
> `CloseVault` is creator-only and gated on the record's
> Released state, which `Release` sets only after the full
> locked amount has left the vault; it sweeps the residual to
> the creator and closes the vault account (rent to the
> creator). It has no path to any part of the lock: while a
> launch is locked or expired-but-unreleased it always fails
> (new error `NotReleased`). No LaunchGuard economics changed
> (the 10% floor, lock durations, and all parameters are as
> before). The same review's claim of a missing record-PDA
> re-derivation check in `ExtendLock`/`Release` was verified
> **not present in the code**: the assertion has been in both
> instructions since `93b5cd9` (2026-11-16), the reputation PDA
> is bound the same way in `RegisterAndLock` and `Release`, and
> dedicated wrong-address regression tests cover all of it;
> `CloseVault` carries the same check. 5 new tests (LaunchGuard
> suite 29 → 34); `launch_guard.so` rebuilt (135,488 B, sha256
> in `bridge/deploy/SBF-BUILD-NOTES.md`); the bridge programs
> were not touched and their binaries were not rebuilt.
> **Auditors must quote and review against the current `main`
> HEAD (pinned at the top of this file), not any earlier tree** —
> quotes issued against pre-`8e84d18` trees do not cover
> `CloseVault`.

> **Scope update (2026-11-17, later still):** the off-chain
> relayer gained a tamper-evident audit log
> (`bridge/crates/relayer/src/audit_log.rs`, inspector binary
> `relayer-log`; doc `bridge/RELAYER-LOG.md`; commit `1a76c91`).
> Every sign, quorum collection, non-routine collection failure,
> dry run, and submission is appended to a hash-chained JSON
> Lines log; the relayer fails closed if the existing log does
> not verify. Verification against a previously published
> checkpoint (`seq:hash` of the head) detects truncation and
> wholesale rewrites, which internal chain verification alone
> cannot — both attacks are covered by tests that pin the limit
> honestly. Off-chain only: it records Scope A relayer actions;
> it does not touch any program: **no program change, no SBF
> rebuild, no scope change.** Relayer tests 41 → 54; bridge
> workspace 146 → 159. Auditors should still quote against the
> current `main` HEAD.
>
> **Scope update (2026-11-17, debate round 1):** both bridge
> programs changed under a two-reviewer debate process (commit
> `0efccbc`). Unlock now binds the attested `source_mint` to the
> config; Machine Initialize requires the wrapped mint's mint
> authority to be the bridge-authority PDA (freeze authority that
> PDA or none); Lock Initialize requires a token-program-owned,
> initialized vault; Unlock/MintWrapped reject the authority PDA
> and the vault as value destinations; Burn pins its mint account;
> every handler re-derives the config PDA. 8 new tests; bridge
> workspace 159 → 167. Both bridge SBF binaries were rebuilt
> (hashes in `bridge/deploy/SBF-BUILD-NOTES.md`); LaunchGuard was
> not touched. **Program scope changed — auditors must quote
> against the current `main` HEAD, not any earlier tree.**
>
> **Scope update (2026-11-17, debate round 2):** the off-chain
> relayer changed under the same two-reviewer debate process
> (commit `9cf00cf`). Quorum collection now counts only
> individually verified signatures — junk files in the shared
> exchange directory are skipped instead of blocking a present
> quorum, while a verified signature over a different message
> still fails collection as equivocation; attestation files can
> no longer be silently overwritten with a different message.
> The source transaction is bound to the event record's own slot,
> and the source-transfer check requires the SPL Token program
> and a successful transaction. The audit log is verified before
> an attestation is published or a transaction is sent (it was
> only consulted afterwards); its default path moved out of the
> shared attestation directory, next to the local state file.
> The `run` loop validates its destination account at startup,
> submits every signed-but-unprocessed nonce (not only the
> latest), skips nonces whose destination receipt already
> exists, and verifies the destination token account's mint and
> owner against the message before submitting. Collection uses
> the on-chain quorum and refuses a paused/killed destination;
> config validation rejects a zero poll interval, a Solana
> safety margin below 16, an expiry window above 1,000,000
> slots, and keypair/state paths inside the shared directory;
> signing-key files with group/other permissions are refused.
> Writes are fsynced. 6 new tests; relayer 54 → 60, bridge
> workspace 167 → 173. **Off-chain only: no program change, no
> SBF rebuild, no scope change.** Auditors should still quote
> against the current `main` HEAD.
>
> **Scope update (2026-11-18):** a new off-chain receipt indexer
> (`bridge/crates/receipt-indexer/`, binary `receipt-index`; doc
> `bridge/RECEIPT-INDEXER.md`) performs the binding BRIDGE.md 4.5
> leaves to indexers: it derives a crossing receipt's PDA,
> parses the receipt account strictly (owner, exact length,
> discriminator, self-derivation), resolves the PDA's creating
> transaction as the oldest successful signature for the address
> (failed replay attempts are skipped), and verifies that
> transaction executed in exactly the slot the receipt records
> with a destination-program instruction writing the PDA. More
> than one successful signature for a receipt PDA is flagged in
> the record as an anomaly rather than hidden. 15 new tests (14
> unit + 1 end-to-end against a real program-test crossing and a
> failed on-chain replay); bridge workspace 173 → 188.
> **Off-chain tooling only: no program change, no SBF rebuild,
> no scope change.** Auditors should still quote against the
> current `main` HEAD.
>
> **Scope update (2026-11-18, debate round 3 — PROGRAM CHANGE,
> SBF REBUILT):** two independent reviews of LaunchGuard
> (`programs/crates/launch-guard/`), debated finding-by-finding.
> One code fix: `ExtendLock`'s cap was `clock.slot +
> MAX_LOCK_SLOTS` — a sliding window that let repeated
> extensions push a lock's total duration past the documented
> 5-year maximum without bound (reproduced before the fix: a
> max-length lock extended by 1 slot reached MAX + 1 total).
> The cap is now `registered_slot + MAX_LOCK_SLOTS`, checked
> arithmetic, failing closed. 1 new adversarial regression test;
> LaunchGuard 34 → 35. Three further agreed findings are
> documented limits in `programs/LAUNCH-GUARD.md`, not code
> changes, because no on-chain check can decide them:
> registration is permissionless and first-come among ≥10%
> holders (the record's creator is the registrant, not a
> verified issuer — creators should register before
> distributing supply); a holder account frozen before the
> freeze authority was revoked stays frozen forever and is
> invisible to the program; and the reputation counters are
> unweighted (a dust-supply completion counts the same as a
> large launch). Killed in debate: two unchecked slot additions
> are unreachable at any real slot height (the extension site
> was replaced by checked arithmetic as part of the cap fix).
> SBF rebuilt: launch_guard.so `9aceb092…` (135,416 B);
> bridge programs untouched. **Program scope changed —
> auditors must quote against the new HEAD.**
>
> **Scope update (2026-11-18, debate round 4 — PROGRAM CHANGE,
> SBF REBUILT):** two independent reviews of bridge-common +
> proof-bundle (`bridge/crates/bridge-common/`,
> `bridge/crates/proof-bundle/`), debated finding-by-finding;
> every surviving finding was reproduced before fixing.
> `MachineConfig` gains a `source_mint` field (account layout
> LEN 334 → 366; `Initialize` carries it) and `MintWrapped` now
> binds `message.source_mint` to it, mirroring the Unlock-side
> binding from round 1 — this closes the round-1 leftover B2.
> `validate_message` bounds the attestation lifetime on the
> source basis (`expiry_slot - source_slot` ≤ 1,000,000 slots;
> expiry before the source slot rejected) — the leftover B8.
> The shared attestation/admin verifiers reject quorum and
> threshold values of 0 (and above the set size), and
> `build_ed25519_instruction` refuses more than `MAX_RELAYERS`
> signatures instead of silently truncating. Proof-bundle:
> non-ASCII message hashes are rejected rather than panicking
> the verifier; zero-amount bundles are rejected; export stamps
> the protocol quorum; the verification report carries the
> relayer set and a new `verify_bundle_against_set` /
> `--expected-relayers` pins it; the CLI caps input files at
> 1 MiB; and the receipt/relayer-set claims in the crate docs
> and `bridge/PROOF-BUNDLE.md` are downgraded to what offline
> verification actually establishes (the receipt is
> unauthenticated stated data — its on-chain existence is a
> separate lookup, as is the set's identity when unpinned).
> 9 new regression tests; bridge workspace 188 → 197. SBF
> rebuilt: solana_lock.so `a22bccd8…` (211,744 B),
> machine_bridge.so `7d0ab41f…` (215,312 B); LaunchGuard
> untouched. **Program scope changed — auditors must quote
> against the new HEAD.**
>
> **Scope update (2026-11-19 — NEW PROGRAM, SBF BUILT):** a
> third native program joins Scope A: PQ Vault
> (`programs/crates/pq-vault/`), a cold-storage vault whose
> withdrawals are authorized by WOTS+ one-time signatures
> (RFC 8391, SHA-256, w=16, len=67) instead of Ed25519. Only
> 32-byte commitments are stored on-chain; the 2,144-byte
> signature is delivered in chunks through a digest-bound
> buffer account. One-time discipline is enforced by the
> program: strict in-order spend, the key index increments
> before funds move, rotation only on a one-time signature,
> exhaustion without a chained rotation freezes the vault.
> 17 tests (4 unit + 13 integration), all twelve of the
> design note's required tests; the suite also passes
> against the SBF build. Measured verification cost (SBF,
> program-test): ExecuteWithdrawal 337k–395k CU, RotateKeys
> 380k–393k CU — above the 200,000 default budget, so
> verifying transactions must request a raised compute
> budget; the design note's 50k–120k estimate is superseded
> in `programs/PQ-VAULT.md`. SBF built: pq_vault.so
> `b6a52b93…` (148,944 B, deterministic). Programs
> workspace 46 → 63; repo total 283. **Program scope
> changed — auditors must quote against the new HEAD.**
>
> **Scope update (2026-11-19, later — LaunchGuard dust
> floor, SBF REBUILT):** LaunchGuard gained a fifth
> instruction, `Initialize { min_lock_amount }`, and a
> config PDA (seeds `["config"]`) carrying a dust floor —
> a minimum lock size in raw units enforced by
> `RegisterAndLock` on top of the 10% share (effective
> minimum: max(ceiling-divided share, floor)). Initialize
> is one-time and bound to the program's upgrade authority
> via a ProgramData check mirrored from bridge-common
> (self-audit F-1 pattern); registration before Initialize
> fails closed. **Breaking instruction-ABI change:**
> `RegisterAndLock` now takes the config PDA as a tenth
> account (appended last); the pre-floor 9-account form
> fails with `NotEnoughAccountKeys`. The program is not
> deployed and has no live clients. The change went
> through a six-seat review (program security, floor-math
> correctness, adversarial, tests/edge cases, honesty/docs,
> build/integration) before shipping; the floor's honest
> limits (raw units, no decimals awareness, zero is a
> legitimate value, an oversized value can make a mint
> unregistrable) are documented in
> `programs/LAUNCH-GUARD.md`. SBF rebuilt: launch_guard.so
> `8153dd4f…` (139,352 B, deterministic). **Program scope
> changed — auditors must quote against the new HEAD.**

### Scope B — Chain configuration (gating: public chain claims)
| Component | Where |
|---|---|
| Genesis + vote-authority patch | `GENESIS.md`, `patch-vote-authority.py`, `ledger/` |
| Livenet genesis + keys tooling | `livenet/generate-genesis.sh`, `livenet/generate-keys.sh`, `livenet/validators.conf` (balanced allocation sums to 1B — there is a test) |
| Validator start / systemd units | `start-validator.sh`, `livenet/start-validator.sh`, `livenet/*.service` |
| Fork changes vs. upstream Agave 2.3.13 | `FORK.md`, `patches/` |

## How to build and test (auditor quick start)

```bash
./tests/run.sh                    # fork-script suite: 23 tests
cd bridge && cargo test --release # bridge workspace: programs + relayer + proof bundle + receipt indexer (197)
cd programs && cargo test --release # LaunchGuard 62 tests (16 integration + 18 adversarial + 1 demo launch + 22 dust floor + 5 unit) + PQ vault 17 + registry export 11
./ops/check-host.sh               # explains why validators cannot run on a restricted VM
```

Expected results are recorded in `bridge/VERIFY.md` for the
programs; the relayer's results join it after its first full run.
Known build caveat: `cargo build` produces host libraries; the
deployable SBF artifacts come from `cargo build-sbf`. The SBF build
was run on 2026-11-14 and both programs compiled (hashes in
`bridge/VERIFY.md`) — against a program-only lockfile, because the
platform-tools cargo cannot parse the edition2024 crates in the
unified workspace lock. Method, dependency pins and caveats:
`bridge/deploy/SBF-BUILD-NOTES.md`, lockfile at
`bridge/deploy/sbf-Cargo.lock`. Reproducing the build from that
lockfile remains an explicit audit check item, not an assumed fact.
(On the 2026-11-14 rebuild, the scratch tree's lockfile was verified
byte-identical to the saved one before building.)

## Free security checks run (2026-11-14, pre-audit)

Not an audit — automated passes, recorded so the engaged auditor
starts from what is already known:

- **`cargo fuzz` (libFuzzer) over bridge-common's parsers**
  (`bridge/fuzz/`): three targets — the Scope A priority
  precompile/sysvar parser, BridgeMessage decoding, and the
  DailyUsage cap arithmetic. First session found a real bug: the
  precompile verifier delegated sysvar reading to upstream
  helpers that slice without length checks, and empty sysvar
  data panicked inside `load_current_index`. Fixed the same day:
  the sysvar is now parsed in-crate with every read
  bounds-checked (malformed → `MissingAttestation`), with a
  regression test; post-fix runs are clean (7.77M executions on
  the parser target; 7.35M and 19.1M on the other two). Details
  in `bridge/VERIFY.md`.
  **Round 2 (2026-11-16):** a fourth target, `crossing_receipt`,
  covers the receipt PDA derivation (`receipt_pda` — total and
  deterministic for any direction byte and nonce) and the
  `CrossingReceipt` borsh round-trip at its declared length, plus
  canonicality of arbitrary-byte decodes. Bounded session
  (`-max_total_time=300`): 2,405,767 executions in 301 s, no
  findings.

- **`cargo audit` (RustSec DB) on both workspaces: 10 known
  vulnerabilities, all in transitive dependencies; none in
  Machine code.** Disposition by group:
  - `rustls` / `rustls-webpki` / `webpki-roots` (5 advisories, low–
    medium): TLS stack used by the **relayer's** RPC client (host
    side only, never on-chain). Must be resolved or explicitly
    accepted before the relayer runs live against mainnet RPCs.
  - `curve25519-dalek` timing variability (RUSTSEC-2024-0344) and
    `ed25519-dalek` signing-oracle note (RUSTSEC-2022-0093): the
    signature libraries, also present throughout the upstream
    Solana stack. Our usage (single-message sign/verify, no batch
    API) is outside the oracle attack's shape; the timing note is
    accepted pending upstream movement, and is exactly the kind of
    item the paid audit should confirm.
  - Remainder (`atty`, `lru`, `memmap2`, `rand`, `sized-chunks`):
    reachable only via `solana-program-test` / Agave host tooling —
    test harness and build machines, never in the SBF program
    graphs or the relayer's release binary path.
  - 16 further warnings are "unmaintained crate" notices in the
    same transitive set, not vulnerabilities.
- **Clippy** (2026-11-14): `programs/` workspace — no errors; two
  unused-import warnings in LaunchGuard, fixed on the spot; the
  remaining warnings are upstream Solana macro/dependency noise
  (deprecated `system_instruction` path, entrypoint cfg notes,
  `proc-macro-error2` future-incompatibility in a transitive
  proc-macro). `bridge/` workspace — no errors, 34 warnings: two
  unused imports in program tests (fixed), five needless-deref
  notes and one too-many-arguments note (initialize handler) —
  style only, in code paths covered by the 56 passing tests;
  accepted as-is rather than refactoring tested code for lint
  cosmetics, and flagged here for the auditor.
- **Sec3 X-Ray**: attempted 2026-11-15 — there is no free tier.
  A Sec3 Pro account's plan page lists only paid plans (Basic
  $99/mo and up) and the scanner is paywalled, so no scan was run;
  the project does not pay for tooling pre-audit.
- **Soteria** (Sec3's original scanner, free, run locally
  2026-11-15): the tool itself works — verified against a bundled
  example program, where it correctly flagged a missing signer
  check — but it cannot analyze this codebase. Its frozen 2022
  toolchain (rustc/cargo 1.56, Solana 1.9.5-era) cannot parse
  Cargo.lock v4 or modern crate manifests, and solana-program
  2.3.0 requires rustc ≥ 1.79. No release since Nov 2022. Dead
  end for Agave 2.3 code; the free pre-audit pass is the cargo
  audit + clippy + cargo-fuzz work above.
- **External code review (2026-11-15), dispositions** — an
  independent reviewer's findings, each re-verified against the
  code before being recorded here:
  1. *Ed25519 precompile parser is the highest-risk component.*
     Agreed — it is Scope A's priority target (above) and is
     fuzz-covered; a real audit should start there.
  2. *3-of-5 relayer quorum is the largest systemic risk.*
     Agreed — a documented design choice (BRIDGE.md §3); caps and
     the kill switch bound the damage, they do not remove the
     trust assumption.
  3. *Kill-switch enforcement.* Verified: `check_mint_allowed`
     rejects mints once `killed` is set and is called before
     minting; burns remain open as the intentional exit path.
  4. *Caps bind on unlock, deposits only hit the vault ceiling.*
     Verified and intentional (BRIDGE.md §4.4): dynamic caps need
     the locked-total reference that only the unlock side has; a
     large deposit is constrained on the way out.
  5. *Nonce replay protection depends on the caller checking.*
     Accurate about the split — `verify_precompile_attestation`
     checks signatures only — but both processors checked the
     nonce PDA (address + program ownership) before any value
     moved, on all four paths, and end-to-end replay tests pass
     for mint, unlock, burn, and deposit
     (`NonceAlreadyProcessed`). The residual risk was a future
     caller forgetting the check, not a present gap. Hardened
     anyway on 2026-11-15 at the project owner's direction: the
     check now lives in one shared helper
     (`bridge_common::check_nonce_unprocessed`) used by all four
     paths, with its own unit tests; behavior unchanged.
  6. *LaunchGuard.* Verified: revoked mint + freeze authorities,
     classic SPL only, ≥10% / ≥30-day lock, creator-only release
     after slot expiry, no early-release path.
  7. *Pause is low-threshold; unpause is admins-only.* Verified:
     pause takes 2-of-5 relayers or 2-of-3 admins
     (`PAUSE_RELAYER_THRESHOLD = 2`); unpause takes the admin
     multisig only. Matches BRIDGE.md §4.3.
  8. *No path to mint without a valid attestation.* Verified:
     the mint authority is the program PDA and MintWrapped runs
     the precompile attestation check before any mint.
  9. *Some status/docs lag the code (older test counts).*
     Confirmed and fixed 2026-11-15: ECOSYSTEM.md still claimed
     34 tests, no relayer, and no SBF build; README, STATUS.md,
     bridge/README.md, the relayer README, and STAGED-LAUNCH.md
     carried 22-relayer / 6-and-8-LaunchGuard / 69-workspace
     counts. All now state the current figures (78 bridge
     workspace incl. 23 relayer, after the nonce-gate tests in
     item 5; 10 LaunchGuard). Dated logs in the VERIFY.md files
     are historical records and were left as written.
  10. *Relayer finality and disagreement handling are off-chain.*
      Agreed: a relayer bug can stall the bridge or attempt a bad
      signature; the on-chain attestation, nonce, and cap checks
      are the last line of defense, and the collector's
      disagreement-blocks-quorum behavior is covered by the
      end-to-end drill test.
  *Reviewer's bottom line:* the implementation is careful and
  the design conservative; no obvious critical (drain /
  infinite-mint) bug was found. The two areas most needing
  professional review are (1) the ed25519 precompile +
  instructions-sysvar parser and (2) the full end-to-end
  instruction flows under adversarial conditions (caps, nonces,
  kill-switch) — both already named in Scope A above — and a
  real audit remains required before any real asset crosses,
  which is this project's standing deployment gate (BRIDGE.md
  §5).

## Known limitations (stated up front, from the project's own docs)
- Single-validator localnet today; transfers unverified on the
  current VM class (UDP blocked, fd limit) — LAUNCH_CHECKLIST.md
  items 17–18.
- Bridge programs are local-only: not deployed, no asset has
  crossed. SBF build verified locally 2026-11-14 (see above).
- ~~Attestation verification uses ed25519-dalek in-program rather
  than the ed25519 precompile + instructions sysvar.~~ Fixed
  2026-11-14: precompile + sysvar verification.
- ~~Caps are absolute values set at initialization, not percentages
  of live vault balance.~~ Fixed 2026-11-14: dynamic 1% per-tx / 5%
  daily caps on unlocks, enforced against the live locked total, on
  top of the absolute caps.
- ~~Daily-cap window uses the Clock sysvar's unix timestamp.~~ Fixed
  2026-11-14: slot-based window (216,000 slots).
- The 3-of-5 relayer set is a trust assumption (BRIDGE.md honest
  limits), weaker than validator-set attestation.
- Highest-risk component, in plain language: the ed25519-precompile
  + instructions-sysvar parser in bridge-common. It interprets
  transaction-supplied data to decide whether a valid 3-of-5
  attestation exists, so a parsing mistake there is the most direct
  path to a forged mint or unlock. That is why it is Scope A's
  priority review target, why it is fuzzed, and why it carries
  deterministic adversarial tests (extra instructions around the
  ed25519 instruction, malformed offsets, cross-instruction
  references, duplicate signers across instructions, wrong-hash
  entries — added 2026-11-15).
- Dev keys in `keys/` are throwaway and must never hold value.

## Engagement rules the project commits to
- The report is published in full, in this repo, whatever it says.
- Critical and high findings are fixed and re-tested before the
  gated step proceeds; mediums are fixed or accepted in writing,
  in public.
- The audit is of a specific commit. The commit hash audited is
  recorded here when the engagement starts:

  **Audited commit:** _(none — no engagement yet)_
  **Auditor:** _(none)_
  **Report:** _(none)_
