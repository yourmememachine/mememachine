# MemeMachine — Audit Readiness Package

Status: **No independent audit has been performed.** This document
exists so that when an auditor is engaged, everything they need is
in one place, and so the scope is fixed in public before the
engagement — an audit with a quietly narrowed scope is worse than
none, because it looks like assurance.

## What must be audited before real value moves

Per BRIDGE.md section 5 item 5, the audit gates the bridge. The
chain configuration gates any public chain launch. Both scopes:

### Scope A — Bridge (gating: no real asset crosses without it)
| Component | Where | Notes for the auditor |
|---|---|---|
| Solana lock program | `bridge/crates/solana-lock/` | Deposit / unlock, vault ceiling, caps, pause. |
| Machine bridge program | `bridge/crates/machine-bridge/` | Mint/burn, kill-switch, no admin mint path exists — verify that claim. |
| Shared logic | `bridge/crates/bridge-common/` | Message format (195-byte canonical encoding), hash, 3-of-5 attestation verification (in-program ed25519-dalek, not the precompile — see bridge/README.md honest limits), nonce replay protection, caps, authority checks. |
| Off-chain relayer | `bridge/crates/relayer/` | Finalized-only signing rule, deterministic message construction (expiry derived from source slot), quorum collection with disagreement blocking, submitter account ordering. A relayer bug that signs a non-finalized or mis-built message is in scope even though quorum + on-chain checks should contain it. |
| Key management | `bridge/deploy/DEPLOY-BRIDGE.md` step 2 | Fresh keys on deployment hosts, role separation, multisig + 48h timelock for upgrades. Process review, not code. |
| Deployment + drills | `bridge/deploy/`, `bridge/drills/` | Caps translation (`caps.py`), initialization parameters, test-crossing / pause / outage drill results. |

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
lockfile and confirming ed25519-dalek's behavior in the SBF artifacts
remain explicit audit check items, not assumed facts.

## Known limitations (stated up front, from the project's own docs)
- Single-validator localnet today; transfers unverified on the
  current VM class (UDP blocked, fd limit) — LAUNCH_CHECKLIST.md
  items 17–18.
- Bridge programs are local-only: not deployed, no asset has
  crossed. SBF build verified locally 2026-11-14 (see above).
- Attestation verification uses ed25519-dalek in-program rather
  than the ed25519 precompile + instructions sysvar.
- Caps are absolute values set at initialization, not percentages
  of live vault balance.
- Daily-cap window uses the Clock sysvar's unix timestamp.
- The 3-of-5 relayer set is a trust assumption (BRIDGE.md honest
  limits), weaker than validator-set attestation.
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
