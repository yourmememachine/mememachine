# Machine — Audit Readiness Package

Status: **No independent audit has been performed.** This document
exists so that when an auditor is engaged, everything they need is
in one place, and so the scope is fixed in public before the
engagement — an audit with a quietly narrowed scope is worse than
none, because it looks like assurance.

Last verified commit: `266f6c5` (2026-11-16; `make verify` green —
bridge workspace 108 passed, LaunchGuard 23 passed, fork suite
23 OK). The pin is refreshed after every full verification run;
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
| LaunchGuard launch standard | `programs/crates/launch-guard/` | Added 2026-11-14. Gates LaunchGuard's own mainnet use (it custodies locked creator tokens), not the bridge. Verify: registration truly requires revoked mint+freeze authorities, the ≥10% / ≥30-day (6,480,000-slot) lock floor, no early-release or lock-shortening path, PDA authority over the vault. Spec: `programs/LAUNCH-GUARD.md`. |
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
cd bridge && cargo test --release # bridge workspace: programs + relayer
cd programs && cargo test --release # LaunchGuard: 23 tests (13 integration + 9 adversarial + 1 demo launch)
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
