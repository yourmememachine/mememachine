# MemeMachine — Status: PROVEN vs NEXT

Verified 2026-11-14 ~12:15 UTC against the running local validator and
the files in this repo. Nothing here is claimed without a run, a file,
or a command behind it. "PROVEN" means it exists and was verified.
"NEXT" means it is not done yet, with the plain blocker stated.

Chain: MemeMachine, Agave 2.3.13 fork. Token: $MACHINE, 1,000,000,000
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
# -> {"jsonrpc":"2.0","result":20569,"id":1}  (12:15 UTC; read it again — it advances)

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
| P3 | Slots advancing, confirming, finalizing | Processed slot 20,569; finalized slot 20,569; confirmed slot 20,600; block height 20,568 (12:15 UTC). Slot rate measured ~2.4 slots/sec (20,265 → 20,277 in 5 s; 20,252 → 20,569 over ~2.5 min). |
| P4 | Single validator voting | `getVoteAccounts` → exactly 1 current vote account: node `7ivcPQJ6…`, vote `GMe5eK5iefNXFN3NbFUx7ge27JQQDi1irsnCkYJdzcDA`, activated stake 999,999,997,717,120 lamports. |
| P5 | $MACHINE is the native gas token; distribution published | `GENESIS.md`: total 1,000,000,000; founder 998,999,000 / stake 1,000,000 / faucet 1,000 / system ~0.2536 (capitalization 1,000,000,000.2535667 in 241 accounts, from `agave-ledger-tool genesis`). Founder balance observed 998,998,999.897019982 — down only by vote-transaction fees since genesis. Faucet balance observed 1,000. |
| P6 | Fee schedule published and matches genesis | `GENESIS.md` + `agave-ledger-tool genesis`: FeeRateGovernor target 10,000 / min 5,000 / max 100,000 lamports per signature, target 20,000 signatures/slot, burn 50%, rent 3,480 lamports/byte-year, exemption 2.0 years. Timing: 64 ticks/slot, 6.25 ms tick, 8,192 slots/epoch, warmup enabled, inflation none. |
| P7 | Vote-authority patch is real and reproducible | `patch-vote-authority.py` in this repo; it rewrites the vote account's authorized voter (founder → vote keypair) after 9 layout checks. Tested 2026-11-14 against synthetic genesis files in `tests/` — it patches only the voter field and refuses (leaving the file byte-identical) on a bad layout, an already-patched voter, or a missing vote account. |
| P8 | Fork-script test suite passes | `tests/` — 23 tests, 23 passed, 0 failed (run 2026-11-14 ~12:15 UTC, `./tests/run.sh`). Covers shell syntax (`bash -n` on all 7 fork scripts), `start-validator.sh` / livenet argument handling (no validator ever started, `--fresh` never used), the patcher on synthetic files, `validators.conf` port uniqueness, livenet distribution arithmetic, and genesis config sanity. See `tests/README.md` for what it deliberately does not cover. |
| P9 | Bridge programs built and tested locally | `bridge/` — `cargo build --release` passes (5 upstream warnings per program, from the `entrypoint!` macro); `cargo test --release` re-run 2026-11-14 ~12:14 UTC: **34 passed, 0 failed** (15 bridge-common unit + 9 machine-bridge integration + 10 solana-lock integration, program-test with the real SPL token program). 3-of-5 quorum, replay protection, caps, pause and kill-switch all exercised. Local only — see N3. |
| P10 | Livenet deployment package exists (scripts + config) | `livenet/`: `generate-keys.sh`, `generate-genesis.sh`, `start-validator.sh`, `check-cluster.sh`, `validators.conf` (gossip 8001/8011/8021, RPC 8899/8900/8901, dynamic ranges non-overlapping), 3 systemd units, `DEPLOY.md`, `PORTS.md`. The balanced livenet distribution in `generate-genesis.sh` sums to exactly 1,000,000,000 MACHINE (verified by test: five primordial buckets 150M / 250M / 200M / 150M / 243,999,000 + 3×1,000,000 identity + 3×1,000,000 stake + 1,000 faucet). No livenet genesis has been generated yet — see N2. |
| P11 | Whitepaper draft exists in the repo | `WHITEPAPER.md`, v1.0, 2026-11-14, 262 lines: chain, architecture, fee model, the balanced livenet allocation (founder/team 15% with a 48-month vesting schedule, ecosystem 25%, community 20%, validator rewards 15%, treasury 24.3999%) and the localnet-vs-livenet distinction. A draft document — not a published paper, not a review of anything. |
| P12 | Prebuilt binaries only, no source build | `bin/` holds Agave 2.3.13 release binaries (`solana-cli 2.3.13, src:5466f459`); `install.sh` pins version 2.3.13 and that commit and downloads the prebuilt release. |
| P13 | Day-one apps exist | Rug-Farm Registry (repo `github.com/yourmememachine/rug-farm-registry`, hosted API) and Solana OS (browser desktop, wallet connect for Phantom/Solflare/Backpack). These are apps that run on/around the chain, not chain consensus — they do not change the single-validator fact. |
| P14 | Bridge relayer implemented and tested locally (added 2026-11-14) | `bridge/crates/relayer/` — nonce-cursor watcher with the finalized-only rule (+16 slots Solana side), deterministic message construction, ed25519 signing, 3-of-5 attestation file exchange + collector (disagreement blocks quorum), MintWrapped/Unlock submitters, JSON-RPC client, CLI (sign-once / collect / submit / run, dry-run unless `submit_enabled`). `cargo test --release -p relayer`: **22 passed, 0 failed** (20 unit + 2 integration). Never run live — see N3. |
| P15 | Repo operability package exists (added 2026-11-14) | All shell scripts executable in git (fork suite: 23 passed, 2 skipped, after the mode fix); `ops/` (host-readiness gate — verified to FAIL with both blockers named on a restricted VM, public-RPC plan, wallet network template, reconciliation job + systemd timer, incident response); `bridge/deploy/` (SBF build script, caps calculator, deploy runbook with the §5 gate); `bridge/drills/` (test-crossing, pause, relayer-outage); `AUDIT.md` audit-readiness package with scope fixed in public. None of these is a live deployment. |

## NEXT

| # | Item | Plain blocker |
|---|---|---|
| N1 | User transfer verified (a confirmed transfer between two fresh keypairs, with a signature) | **This host blocks UDP send** (container: `sendto` → EPERM), which breaks RPC → TPU forwarding. The faucet also fails here. Needs a host where UDP works; then fund a fresh keypair and transfer — the signature is the proof. |
| N2 | Multi-validator: 3 nodes reaching consensus | **This host cannot run `agave-validator`.** Its file-descriptor hard limit is 4,096 (`ulimit -Hn` = 4096, observed 2026-11-14); `agave-validator` requires 1,000,000 and it cannot be raised here. Also needs working UDP for gossip. The livenet package (P10) is ready to run on a suitable host; no livenet keys, genesis, or `genesis-info.env` exist yet. |
| N3 | Bridge deployed; an asset actually crossed | **No live net to deploy to, no audit.** The programs (P9) and the relayer (P14, added 2026-11-14) are local-only: no SBF build, no program deployed anywhere, no relayer has run live, no asset has crossed. Needs N2 + a security audit + fresh production keys + worthless-amount test crossings first (see `bridge/deploy/DEPLOY-BRIDGE.md`). |
| N4 | Public RPC endpoint | Needs hosted validators on a public host (N2 first) with the RPC exposed. Nothing public answers today. |
| N5 | Chain-level wallet support (wallets adding the MemeMachine network) | Needs N4 (a public RPC endpoint) plus the published chain identity. OS-layer wallet connect (P13) is done; this is the network-add step. |
| N6 | Independent audit | No auditor engaged, no report. Covers the validator configuration and the bridge programs before real value is involved. |
| N7 | Public chain (hosted validators, stake, monitoring) | Needs N2 + N4 + N6, plus monitoring and incident response. A separate step, not claimed now. |

## Honest limits (unchanged)

- **Local single-node testnet, not a public chain.** One validator, on
  this VM. Everything public-facing is NEXT.
- **Transfers unverified** (N1). The chain votes and finalizes, but no
  user-transfer signature exists.
- **Bridge code only, not a live bridge** (N3). 34/34 program tests and 22/22 relayer tests prove the
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
- **Repo state:** local only. Nothing in this file was pushed to
  GitHub. The public repo still serves the earlier state until a push
  is explicitly done.
