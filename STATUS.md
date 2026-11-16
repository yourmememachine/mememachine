# Machine — Status: PROVEN vs NEXT

Verified 2026-11-16 ~15:20 UTC by a full local `make verify` run
(bridge workspace 109 passed, LaunchGuard 26 passed, fork suite
23 OK — total 158, 0 failed) against the local working tree,
which includes the 2026-11-16 hardening changes (LaunchGuard forgery
patch, ed25519 compute-unit measurements) not yet pushed to GitHub.
Nothing here is claimed without a run, a file,
or a command behind it. "PROVEN" means it exists and was verified.
"NEXT" means it is not done yet, with the plain blocker stated.

**Live-chain state, 2026-11-16: the local validator is STALLED at slot
29,100** (processed / confirmed / finalized all 29,100, 29,098
transactions) since ~22:11 PST 2026-11-14. `getHealth` still returns
`ok` and the process is alive, but the slot does not advance; plain
restarts from the existing ledger re-stall at the same slot. The
earlier "slots advancing" evidence below (P3) is historical — it was
true on 2026-11-14 and is not true now. Fresh restart vs diagnosis
is an open decision; see P3.

Chain: Machine, Agave 2.3.13 fork. Token: $MACHINE, 1,000,000,000
fixed supply, native gas from genesis. Genesis hash:
`32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`.

## How to verify anything on this page

```sh
# Chain is up, and which chain it is (local validator RPC)
curl -s -m 5 -X POST http://127.0.0.1:8899 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}'
# -> {"jsonrpc":"2.0","result":"ok","id":1}

curl -s -m 5 -X POST http://127.0.0.1:8899 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getGenesisHash"}'
# -> {"jsonrpc":"2.0","result":"32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb","id":1}

curl -s -m 5 -X POST http://127.0.0.1:8899 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}'
# -> {"jsonrpc":"2.0","result":29100,"id":1}  (2026-11-16: STALLED at 29,100
#    since ~22:11 PST 2026-11-14 — read it again, it does not advance)

# Same slot at finalized and confirmed commitment (run all three)
curl -s -m 5 -X POST http://127.0.0.1:8899 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getSlot","params":[{"commitment":"finalized"}]}'
curl -s -m 5 -X POST http://127.0.0.1:8899 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getSlot","params":[{"commitment":"confirmed"}]}'

# Identity, version, balances
./bin/solana slot --url http://127.0.0.1:8899
./bin/solana balance 7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz --url http://127.0.0.1:8899
./bin/agave-ledger-tool --ledger ledger genesis-hash
./bin/agave-ledger-tool --ledger ledger genesis

# Bridge programs (local only)
cd bridge && cargo build --release && cargo test --release

# Fork-script tests (never starts a validator, never touches ledger/ or keys/)
./tests/run.sh
```

## PROVEN

| # | Item | Evidence (observed 2026-11-14 ~12:15 UTC unless noted) |
|---|---|---|
| P1 | Chain boots from a custom genesis | `solana-test-validator` (pid 2157) running on `ledger/` since 11:32 UTC. `getHealth` → `ok`. `getIdentity` → `7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz`. `getVersion` → `solana-core 2.3.13`. |
| P2 | Genesis hash published and matches the live chain | `getGenesisHash` → `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`; `agave-ledger-tool --ledger ledger genesis-hash` returns the same hash. Shred version 35836, created 2026-11-14T08:54:31Z. |
| P3 | Slots advancing, confirming, finalizing — STALLED, see header | **Not true now.** Processed / confirmed / finalized slot all frozen at 29,100 (29,098 transactions) since ~22:11 PST 2026-11-14; `getHealth` still returns `ok`. Plain restarts from the existing ledger re-stall at the same slot (tower-lockout pattern; a tower-file clear on 2026-11-14 bought only ~30 slots). Historical evidence, when it was advancing (2026-11-14 12:15 UTC): processed 20,569, finalized 20,569, confirmed 20,600, block height 20,568; slot rate ~2.4 slots/sec. Open decision: fresh restart from shipped genesis (same genesis hash) vs diagnosis of the stall. |
| P4 | Single validator voting | `getVoteAccounts` → exactly 1 current vote account: node `7ivcPQJ6…`, vote `GMe5eK5iefNXFN3NbFUx7ge27JQQDi1irsnCkYJdzcDA`, activated stake 999,999,997,717,120 lamports. |
| P5 | $MACHINE is the native gas token; distribution published | `GENESIS.md`: total 1,000,000,000; founder 998,999,000 / stake 1,000,000 / faucet 1,000 / system ~0.2536 (capitalization 1,000,000,000.2535667 in 241 accounts, from `agave-ledger-tool genesis`). Founder balance observed 998,998,999.897019982 — down only by vote-transaction fees since genesis. Faucet balance observed 1,000. |
| P6 | Fee schedule published and matches genesis | `GENESIS.md` + `agave-ledger-tool genesis`: FeeRateGovernor target 10,000 / min 5,000 / max 100,000 lamports per signature, target 20,000 signatures/slot, burn 50%, rent 3,480 lamports/byte-year, exemption 2.0 years. Timing: 64 ticks/slot, 6.25 ms tick, 8,192 slots/epoch, warmup enabled, inflation none. |
| P7 | Vote-authority patch is real and reproducible | `patch-vote-authority.py` in this repo; it rewrites the vote account's authorized voter (founder → vote keypair) after 9 layout checks. Tested 2026-11-14 against synthetic genesis files in `tests/` — it patches only the voter field and refuses (leaving the file byte-identical) on a bad layout, an already-patched voter, or a missing vote account. |
| P8 | Fork-script test suite passes | `tests/` — 23 tests, 23 passed, 0 failed (run 2026-11-14 ~12:15 UTC, `./tests/run.sh`). Covers shell syntax (`bash -n` on all 7 fork scripts), `start-validator.sh` / livenet argument handling (no validator ever started, `--fresh` never used), the patcher on synthetic files, `validators.conf` port uniqueness, livenet distribution arithmetic, and genesis config sanity. See `tests/README.md` for what it deliberately does not cover. |
| P9 | Bridge programs built and tested locally | `bridge/` — `cargo build --release` passes (5 upstream warnings per program, from the `entrypoint!` macro); `cargo test --release` 2026-11-16: **70 program tests passed, 0 failed** (33 bridge-common unit + 21 machine-bridge integration + 15 solana-lock integration + 1 precompile-cost test, program-test with the real SPL token program; 109 including the relayer's 39 — P14). 3-of-5 quorum via the ed25519 precompile, replay protection, absolute + dynamic caps, pause and kill-switch all exercised. SBF-measured compute units (real build, 2026-11-16): MintWrapped 84,952 execution CU + 7,200 precompile = 92,152 total; Unlock 79,582 + 7,200 = 86,782 — three ed25519 verifies fit the 200,000 CU budget comfortably. See `bridge/VERIFY.md`. Local only — see N3. |
| P10 | Livenet deployment package exists (scripts + config) | `livenet/`: `generate-keys.sh`, `generate-genesis.sh`, `start-validator.sh`, `check-cluster.sh`, `validators.conf` (gossip 8001/8011/8021, RPC 8899/8900/8901, dynamic ranges non-overlapping), 3 systemd units, `DEPLOY.md`, `PORTS.md`. The balanced livenet distribution in `generate-genesis.sh` sums to exactly 1,000,000,000 MACHINE (verified by test: five primordial buckets 150M / 250M / 200M / 150M / 243,999,000 + 3×1,000,000 identity + 3×1,000,000 stake + 1,000 faucet). No livenet genesis has been generated yet — see N2. |
| P11 | Whitepaper draft exists in the repo | `WHITEPAPER.md`, v1.0, 2026-11-14, 262 lines: chain, architecture, fee model, the balanced livenet allocation (founder/team 15% with a 48-month vesting schedule, ecosystem 25%, community 20%, validator rewards 15%, treasury 24.3999%) and the localnet-vs-livenet distinction. A draft document — not a published paper, not a review of anything. |
| P12 | Prebuilt binaries only, no source build | `bin/` holds Agave 2.3.13 release binaries (`solana-cli 2.3.13, src:5466f459`); `install.sh` pins version 2.3.13 and that commit and downloads the prebuilt release. |
| P13 | Day-one apps exist | Solana OS (browser desktop, wallet connect for Phantom/Solflare/Backpack). This is an apps layer that runs on/around the chain, not chain consensus — it does not change the single-validator fact. |
| P14 | Bridge relayer implemented and tested locally (added 2026-11-14) | `bridge/crates/relayer/` — nonce-cursor watcher with the finalized-only rule (+16 slots Solana side), deterministic message construction, ed25519 signing, 3-of-5 attestation file exchange + collector (disagreement blocks quorum), MintWrapped/Unlock submitters, JSON-RPC client, CLI (sign-once / collect / submit / run, dry-run unless `submit_enabled`). Since 2026-11-16 (P21) the watcher verifies the source transfer behind each event before signing (Coreum lesson, Aug 2026). `cargo test --release -p relayer`: **39 passed, 0 failed** (35 unit + 4 integration, including the full local end-to-end drill and the 45-crossing volume drill). Never run live — see N3. |
| P15 | Repo operability package exists (added 2026-11-14) | All shell scripts executable in git (fork suite: 23 passed, 2 skipped, after the mode fix); `ops/` (host-readiness gate — verified to FAIL with both blockers named on a restricted VM, public-RPC plan, wallet network template, reconciliation job + systemd timer, incident response); `bridge/deploy/` (SBF build script, caps calculator, deploy runbook with the §5 gate); `bridge/drills/` (test-crossing, pause, relayer-outage); `AUDIT.md` audit-readiness package with scope fixed in public. None of these is a live deployment. |
| P16 | LaunchGuard native launch standard implemented and tested locally (added 2026-11-14) | `programs/crates/launch-guard/` — Machine's original flagship program (spec: `programs/LAUNCH-GUARD.md`): a launch registers only with mint+freeze authorities revoked and ≥10% of supply locked ≥30 days (6,480,000 slots, slot-based by design) in a program-PDA vault; locks can be extended, never shortened; no admin key. `cargo test --release` 2026-11-16: **26 passed, 0 failed** (program-test, real SPL token program): 13 integration tests, including the creator reputation PDA (per-creator registered/completed counters, added 2026-11-15), the end-to-end demo launch (added 2026-11-16, P20), plus a 12-test "rug attempt" adversarial harness (added 2026-11-16, P19, extended with the forgery patch, P23). In the audit scope (AUDIT.md Scope A); not deployed — see N3. |
| P17 | External code-review changes implemented (added 2026-11-14) | Attestation verification switched to the ed25519 precompile + instructions sysvar (no in-program signature crypto; SBF binaries ~41% smaller); dynamic caps on unlocks (1% of locked total per tx, 5% per 216,000-slot window) on top of the absolute caps; adversarial tests (expiry boundary, reordered/duplicate signature sets, nonce gap 1→1,000,000, dynamic-cap boundaries); LaunchGuard explicitly rejects Token-2022 mints. Bridge workspace **74 passed, 0 failed**; LaunchGuard **10 passed, 0 failed**; fork suite 23 OK. Details: `bridge/VERIFY.md`, `programs/VERIFY.md`. Still local-only code — deployment gates unchanged (N3). |
| P18 | Hardening + features round (added 2026-11-15) | Deterministic sysvar-parser adversarial tests (6), message hash-binding and cap-overflow boundary tests, an end-to-end relayer-rotation test, a 45-crossing volume drill that reconciles exactly (`bridge/drills/last-drill.md`), plus three features: **crossing receipts** (a permanent receipt PDA written by every mint/unlock — BRIDGE.md §4.5), **automatic cap tightening** (dynamic caps at a quarter for one window after any unpause), and the **LaunchGuard creator reputation PDA** (registered/completed counters any program can read). Bridge workspace **93 passed, 0 failed**; LaunchGuard **13 passed, 0 failed**; fork suite 23 OK. Docs: THREAT-MODEL.md, LIMITATIONS.md, SECURITY-CHANGELOG.md. Still local-only — deployment gates unchanged (N3). |
| P19 | LaunchGuard "rug attempt" adversarial harness (added 2026-11-16) | `programs/crates/launch-guard/tests/adversarial.rs` — 12 tests written from an attacker's checklist, all against the real SPL token program in program-test: release one slot before expiry fails and at the exact expiry slot succeeds; double-registering the same mint fails; a stranger cannot drain the vault directly through the token program; release to an attacker's token account fails (MintMismatch); release with a missing or foreign reputation PDA fails (InvalidPda) and moves no funds; a failed registration creates no reputation account; two creators' interleaved launches keep separate reputation counters; plus 3 forgery tests added in the 2026-11-16 hardening pass (P23): release with a foreign-owned forged record fails (InvalidAccountData), release/extend with the record at the wrong address fails (InvalidPda). One design point pinned, not a bug: extending after expiry is permitted because it only re-locks the creator's own funds for longer — the old expiry stays sealed until the new date. Tests + the P23 program patch, no SBF rebuild, no audit-scope change. LaunchGuard suite **26 passed, 0 failed**; `make verify` green (total 158). |
| P20 | LaunchGuard demo launch performed on localnet (added 2026-11-16) | `programs/crates/launch-guard/tests/demo_launch.rs`, write-up in `programs/DEMO-LAUNCH.md`: a fresh demo token (mint `AjTP23T2HhMryZVDPpzVYrTsWHgH6jKkMt9b8cLdn7hc`, supply 1,000,000,000, fixed demo-only keypairs so the run is reproducible) with both authorities revoked, registered with 100,000,000 (10%) locked until slot 6,480,003 (30 days); the rug paths were then attempted live and all failed — mint more (SPL `FixedSupply`), release early (`StillLocked`), shorten the lock (`LockPeriodTooShort`), release as a stranger (`NotCreator`) — and the final state was unchanged. Localnet program-test only; the demo token is worthless by construction and this is not a public launch. Performed 2026-11-16, so for the Dev3pack submission (27–29 Nov 2026) it counts as pre-existing work. Tests only: no program change, no SBF rebuild, no audit-scope change. LaunchGuard suite **23 passed, 0 failed**; `make verify` green against `a6eefcc`. |
| P21 | Relayer source-transfer verification (added 2026-11-16) | Trust audit of the relayer watcher prompted by the Coreum bridge drain (Aug 2026: relayers attested to self-payments of the bridge's own wrapped token as deposits, ~200,000 XRP lost). Finding: the `EventRecord` itself is sound — the lock program transfers into the vault before writing it — but the watcher's sender extraction only scanned top-level instructions, while the deposit's token transfer is a CPI (inner instruction) of the Deposit ix, so on a real deposit transaction it found no transfer and refused to sign, and a batched transaction with an unrelated top-level transfer would have yielded the wrong sender. Fix (`bridge/crates/relayer/src/rpc.rs`, `main.rs`): the watcher now reads the source program's config (vault / wrapped mint) and requires the source transaction to contain the transfer behind the record — into the vault for a deposit, burning the wrapped mint for a burn — for exactly the record's amount, found in top-level or inner instructions; anything else fails closed with `SourceTransferNotVerified` and no signature is produced. 8 new tests (phantom deposit rejected, wrong amount/destination/mint rejected, decoy top-level transfer ignored, sender found in inner instructions for both directions). Off-chain only: no program change, no SBF rebuild, no audit-scope change (relayer already in Scope A; see AUDIT.md). Bridge workspace **99 passed, 0 failed**; relayer **30 passed, 0 failed**. |
| P22 | Relayer malformed-attestation fail-closed tests + receipt fuzz target (added 2026-11-16) | The attestation exchange treats files from other relayers as untrusted input; 9 new `Collector` tests (`bridge/crates/relayer/src/attestation.rs`) prove it fails closed — truncated, garbage-byte, and empty files error out; wrong-length and forged signature fields are rejected (the forged one only at the final bridge-common re-verification); a tampered stored message hash is rejected in `to_parts`; a valid attestation for another nonce wearing this nonce's file name never counts toward quorum; and a malformed file aborts collection even when a full valid quorum is present (strict fail-closed — a human investigates the exchange directory, nothing is submitted). No behavior change was needed: the collector already failed closed in every shape; the tests pin it. Also fuzz round 2: new `crossing_receipt` cargo-fuzz target (receipt PDA derivation + `CrossingReceipt` serialization round-trip), bounded session 2,405,767 executions, no findings (AUDIT.md). Tests/tooling only: no program change, no SBF rebuild, no audit-scope change. Bridge workspace **108 passed, 0 failed**; relayer **39 passed, 0 failed**. |
| P23 | Hardening pass: LaunchGuard forgery patch + ed25519 compute-unit measurement (added 2026-11-16) | **The patch (real hole, fixed):** `programs/crates/launch-guard/src/processor.rs` — `load_record` now rejects accounts the program does not own (`InvalidAccountData`) before parsing, and `process_release` / `process_extend` verify the record is the derived `["launch", mint]` PDA (`InvalidPda`). Without it, a program-owned account holding valid-looking record bytes for another address could stand in for a victim's record and Release would drain the victim's vault (the transfer executes before the reputation check). 3 new adversarial tests pin it (P19). **Mutation check done 2026-11-16:** removing the release PDA check makes the wrong-address release test fail; removing the owner check makes the forged-record test fail; the file was restored byte-identical (sha256 `79b1f2d9…`) after each mutation and the suite re-ran green. **Compute units, measured on the real SBF build** (solana_lock.so 204,272 B, sha256 `cda1b66f…`; machine_bridge.so 204,016 B, sha256 `ecd19d0f…`): MintWrapped (3-of-5) 84,952 execution CU + 7,200 precompile = **92,152 total**; Unlock (3-of-5) 79,582 + 7,200 = **86,782 total**; Burn 15,054; Deposit 16,719. `compute_units_consumed` excludes the precompile charge (proved: a 3-signature precompile-only transaction reports 0 CU) — the cost-model charge is 2,400 CU/signature (strict). Three ed25519 verifies fit the 200,000 CU budget comfortably (~46% / ~43%). The old `bridge/VERIFY.md` 409-CU figures were native-execution measurements and are corrected there. Also: SPL Token is now registered as a builtin in the bridge tests (no `spl_token.so` exists in SBF mode), and a new `precompile_cost` test pins the 0-CU precompile-only result. Full `make verify` 2026-11-16: bridge **109**, LaunchGuard **26**, fork suite **23** — total **158 passed, 0 failed**. Details: `HARDENING-2026-11-16.md`, `bridge/VERIFY.md`, `bridge/deploy/SBF-BUILD-NOTES.md`. |

## NEXT

| # | Item | Plain blocker |
|---|---|---|
| N1 | User transfer verified (a confirmed transfer between two fresh keypairs, with a signature) | **This host blocks UDP send** (container: `sendto` → EPERM), which breaks RPC → TPU forwarding. The faucet also fails here. Needs a host where UDP works; then fund a fresh keypair and transfer — the signature is the proof. |
| N2 | Multi-validator: 3 nodes reaching consensus | **This host cannot run `agave-validator`.** Its file-descriptor hard limit is 4,096 (`ulimit -Hn` = 4096, observed 2026-11-14); `agave-validator` requires 1,000,000 and it cannot be raised here. Also needs working UDP for gossip. The livenet package (P10) is ready to run on a suitable host; no livenet keys, genesis, or `genesis-info.env` exist yet. |
| N3 | Bridge deployed; an asset actually crossed | **No live net to deploy to, no audit.** The programs (P9) and the relayer (P14, added 2026-11-14) are local-only: SBF binaries build (2026-11-14, hashes in `bridge/VERIFY.md`), but no program is deployed anywhere, no relayer has run live, no asset has crossed. Needs N2 + a security audit + fresh production keys + worthless-amount test crossings first (see `bridge/deploy/DEPLOY-BRIDGE.md`). |
| N4 | Public RPC endpoint | Needs hosted validators on a public host (N2 first) with the RPC exposed. Nothing public answers today. |
| N5 | Chain-level wallet support (wallets adding the Machine network) | Needs N4 (a public RPC endpoint) plus the published chain identity. OS-layer wallet connect (P13) is done; this is the network-add step. |
| N6 | Independent audit | No auditor engaged, no report. Covers the validator configuration and the bridge programs before real value is involved. |
| N7 | Public chain (hosted validators, stake, monitoring) | Needs N2 + N4 + N6, plus monitoring and incident response. A separate step, not claimed now. |

## Honest limits (unchanged)

- **Local single-node testnet, not a public chain.** One validator, on
  this VM. Everything public-facing is NEXT. **The chain is currently
  stalled at slot 29,100 (see header / P3)** — it voted and finalized
  until ~22:11 PST 2026-11-14 and does not advance now.
- **Transfers unverified** (N1). The chain votes and finalizes, but no
  user-transfer signature exists.
- **Bridge code only, not a live bridge** (N3). 109/109 bridge-workspace tests (including 39/39 relayer) and 26/26 LaunchGuard tests prove the
  programs behave as designed in program-test; they prove nothing about
  a live deployment, which does not exist.
- **Dev keys only.** The keypairs in `keys/` are throwaway test keys
  for this localnet. They hold no mainnet value and must be replaced
  before any public or production use. The livenet keys do not exist
  yet and must be generated on the deployment host, never copied into
  this repo.
- **Localnet distribution is founder-weighted by design** (founder
  998,999,000 of 1,000,000,000). It is a throwaway dev genesis. The
  balanced livenet allocation (P10/P11) applies only to the
  not-yet-generated livenet genesis.
- **Repo state:** pushed to GitHub (github.com/yourmememachine/mememachine)
  under the owner's identity — but the public repo is currently BEHIND
  this working tree: the 2026-11-16 hardening changes (P23) are not
  yet pushed. Do not treat the repo as matching this file until the
  push is done and verified.
