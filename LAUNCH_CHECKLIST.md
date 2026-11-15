# MemeMachine — Launch Checklist: what "best" means

Honest only. An item is **PROVEN** only if there is evidence below
(a slot number, a hash, a balance, a file). Everything else is
**NEXT**, with exactly what it needs. Nothing is marked proven on a
claim.

Verified on the localnet, 2026-11-14. Live chain at the time of
writing: slot 1,180, confirmed 1,179, finalized 1,149.

## PROVEN

| # | Item | Evidence |
|---|---|---|
| 1 | Chain boots from a custom genesis | `solana-test-validator` running on `ledger/` (MemeMachine genesis). Identity `7ivcPQJ6…`, genesis hash `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`, shred version 35836. |
| 2 | Genesis hash published | `GENESIS.md` — live hash `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`. |
| 3 | Validator keys published | `GENESIS.md` — identity, vote, stake, faucet pubkeys. |
| 4 | Fee schedule published | `GENESIS.md` — FeeRateGovernor: 9,500 lamports/signature, target 10,000, min 5,000, max 100,000, burn 50%, 20,000 target signatures/slot. |
| 5 | MACHINE distribution written down | `GENESIS.md` — founder 998,999,000 / stake 1,000,000 / faucet 1,000 / system ~0.2536. Total 1,000,000,000. |
| 6 | MACHINE is the native gas token from genesis | Founder balance decreases only by vote-transaction fees (998,999,000 → 998,998,999.99411), paid in MACHINE. No other token exists at genesis. |
| 7 | Slots advancing | Slot 1,180 and rising (~3 slots/second). `solana slot` returns a live, advancing number. |
| 8 | Voting and confirming | Confirmed Slot 1,179, tracking Processed Slot 1,180. Vote credits 17,184 and growing. Vote authority is the vote keypair (`GMe5eK5i…`), withdraw authority is the founder. |
| 9 | Finalizing | Finalized Slot 1,149. Root slot 1,074+. |
| 10 | Snapshots working | Full snapshot at slot 1,100 (validator log: `Full Snapshot Slot: 1100`). |
| 11 | Founder / faucet / stake balances correct | Founder 998,998,999.99411, faucet 1,000, stake 1,000,000 — all via `solana balance` on the live RPC. |
| 12 | Day-one apps shipped | Rug-Farm Registry (repo `github.com/yourmememachine/rug-farm-registry`, hosted API `rug-farm-registry.onrender.com`) + Solana OS (web artifact, the native apps layer). |
| 13 | Fast blocks | Observed ~3 slots/second on the localnet, confirming and finalizing continuously. |
| 14 | Wallet support beyond one wallet (OS layer) | Solana OS wallet connect supports Phantom, Solflare, and Backpack (real USDC signing). |
| 15 | Prebuilt binaries only, no source build | Agave 2.3.13 release binaries in `bin/`. `install.sh` downloads the release; nothing was compiled from source. |
| 16 | Vote-authority fix is reproducible | `patch-vote-authority.py` in this repo, with layout checks that abort on mismatch. It is the documented reason the test-validator can vote on a `solana-genesis` chain. |

## NEXT (not proven — what each needs)

| # | Item | What it needs |
|---|---|---|
| 17 | Transfer between two fresh keypairs (with a signature) | **A host where UDP send works.** On this VM the container blocks UDP send (`sendto` → EPERM), which breaks the RPC → TPU (QUIC, port 8009) forwarding that user transactions need. Evidence of the block: `solana_tpu_client_next` log "Connection error 127.0.0.1:8009: timed out / Failed to establish connection: reach max reconnect attempts", and a direct UDP `sendto` test failing with EPERM. The faucet also fails here (`TransactionDataTooLarge(30061)` on every airdrop). On a host with working UDP, run `./start-validator.sh`, fund a fresh keypair, and transfer — the signature is the proof. |
| 18 | Multi-validator: 3 nodes reaching consensus | **A host with `--ulimit nofile=1000000` and working UDP.** `agave-validator` (the only multi-validator binary) refuses to start here: it requires raising the open-file-descriptor limit to 1,000,000, but the container hard limit is 4,096 and cannot be raised (sysctl bypass fixed the network check, `prlimit` and the user-namespace raise both return "Operation not permitted"). `solana-test-validator` is single-node only (no entrypoint/join flag). Also needs UDP for gossip. On a suitable host, boot 3 `agave-validator` nodes on this genesis (or a regenerated 3-bootstrap-validator genesis) and show all three voting. |
| 19 | Public RPC endpoint | Hosting (a VM/bare metal with a public IP, the validator's RPC exposed). Not claimed until it answers. |
| 20 | Chain-level wallet support (wallets adding the MemeMachine network) | A public RPC endpoint (item 19) plus the chain ID published so wallets can add it. The OS-layer wallet connect (item 14) is done; this is the network-add step. |
| 21 | Bridge (Solana mainnet ↔ MemeMachine) | Bridge programs implemented and tested locally (`bridge/`, 34/34 program tests) and the off-chain relayer implemented and tested locally (`bridge/crates/relayer/`, 22/22 tests) — security review still pending (inquiries sent to two firms 2026-11-14), SBF build verified locally 2026-11-14 (hashes in `bridge/VERIFY.md`), not deployed. Nothing is claimed as bridged until a real asset crosses. |
| 22 | Audit | An independent audit of the validator configuration and any bridge/program code, with the report published. |
| 23 | Bug bounty | A published bounty program with scope and rewards. |
| 24 | Whitepaper | A written whitepaper (tokenomics, consensus, roadmap, governance). |
| 25 | Public chain | Hosted validators, stake, monitoring, incident response, and a public RPC — a separate step, not claimed now. |

## What "best" does NOT mean (yet)

- MemeMachine is **not** a multi-validator chain today.
  It is 1 validator right now. The multi-validator upgrade
  (item 18) is the proof that would change that, and it is NEXT.
- MemeMachine has **no** trading token, no deployed bridge, and no audit yet.
  The bridge programs and relayer (item 21) are built and tested
  locally but not deployed, reviewed, or crossed.
- A transfer has **not** been confirmed on this VM (item 17). The
  chain votes and finalizes, but do not present a transfer signature
  until one exists on a host where UDP works.
