# Machine Ecosystem — Architecture

**Status: 2026-11-16. This document states what exists, what connects
to what today, and what is planned. Nothing here is claimed without a
run, a file, or a command behind it.**

Machine is a Solana-derived Layer 1 (Agave 2.3.13) with its own
genesis, its own native token, and one companion codebase by the same
author that integrates with it: a browser desktop interface (Solana
OS). The chain and the OS are **separate codebases in separate
repositories / artifacts**. They are one ecosystem only in the sense
that they are built by the same author for the same chain, and that
the concrete integration points in §2 exist. There is no shared code,
no shared deployment, and no shared database between them.

Repositories / locations:

| Layer | Where it lives |
|---|---|
| Core chain | This repo — https://github.com/yourmememachine/mememachine |
| Bridge programs | `bridge/` in this repo |
| Solana OS (browser interface) | Browser artifact, source at `~/workspace/ts-spaces/solana-os/index.html` |

---

## 1. The layers

### Layer 1 — Core chain (Machine)

**What it is.** A real fork of Solana validator code (Agave 2.3.13,
prebuilt release binaries) running a custom genesis: its own chain
identity, its own fee schedule, its own validator, its own native gas
token **$MACHINE** (1,000,000,000 fixed supply, no inflation).
Genesis hash `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`, shred
version 35836. See `GENESIS.md`.

**What it is not.** Not a public chain. Not multi-validator. Not a
token on Solana mainnet — it is a separate chain that produces its own
blocks, votes, and finality, on one local node.

**Proven (2026-11-14, `LAUNCH_CHECKLIST.md`, `GENESIS.md`).** Boots
from the custom genesis; votes, confirms, and finalizes continuously
(slot 1,180, confirmed 1,179, finalized 1,149 at time of writing,
~3 slots/second); fee schedule and distribution published; snapshots
working. The vote-authority patch that makes `solana-test-validator`
vote is in this repo (`patch-vote-authority.py`) and reproducible.

**Honest limits.** Single validator, local testnet only. **User
transfers are not verified**: the build container blocks UDP send, so
RPC → TPU forwarding fails and no transfer between two fresh keypairs
has been confirmed here; the faucet also fails here
(`TransactionDataTooLarge`). No public RPC endpoint exists. The
keypairs in `keys/` are throwaway dev keys and must be replaced before
any public use. A 3-validator live-net package exists in `livenet/`
but is **not deployed**.

### Layer 2 — Bridge (Solana mainnet ↔ Machine)

**What it is.** Native Rust on-chain programs in `bridge/`
(`bridge-common` + `machine-bridge` mint/burn + `solana-lock`
deposit/unlock), implementing the lock/mint design in `BRIDGE.md`:
1:1 custody bridge, 3-of-5 relayer quorum, per-nonce replay
protection, on-chain caps, pause, and a one-way kill-switch on the
Machine side. No admin mint; $MACHINE itself is never bridged.

**What it is not.** Not deployed. Not audited. The off-chain
relayer exists (`bridge/crates/relayer/`) but has never run live;
no asset has crossed, nothing here holds real funds.

**Proven.** Built and tested locally only. Re-verified 2026-11-18:
`cargo test --release` in `bridge/` — **188 passed, 0 failed**
(37 bridge-common, 31 machine-bridge, 25 solana-lock, 1
precompile-cost, 60 relayer including the full end-to-end drill,
19 proof bundle including an end-to-end crossing export,
15 receipt indexer including an end-to-end receipt binding).
Full output in `bridge/VERIFY.md`.

**Honest limits.** SBF builds of all three programs are verified
locally (hashes in `bridge/VERIFY.md`); that is a build proof, not
a deployment. No deployment and no audit. All seven prerequisites
in `BRIDGE.md` §5 (live 3-validator net, public RPC, verified user
transfers, fresh production keys, security review,
worthless-amount test crossings, monitoring) are unmet.

### Layer 3 — Browser interface (Solana OS)

**What it is.** A separate browser-based desktop artifact by the same
author: a chain-as-filesystem window manager where slots are time,
the wallet is the home folder, and programs are apps (Wallet, Rug
Shield, Pump Radar, x402 Store, MCP Hub, Terminal, Chain Files).
Wallet connect supports Phantom, Solflare, and Backpack with real
signing; the x402 Store speaks the real facilitator protocol
(`facilitator.x402.rs` `/verify` + `/settle`, $0.001 per check).
Rug Shield runs its launch-detection rules engine in the browser on
live chain data.

**What it is not.** It is not the chain, not a validator, and not
currently reading Machine chain data. Today its live data comes from
Solana mainnet (DexScreener + public Solana RPC) — there is no public
Machine RPC for it to read yet.

**Honest limits.** Wallet connect could not be completed from an
iPhone in-app browser with no wallet extension (reported 2026-11-13,
unresolved). No live $0.001 x402 payment has been settled through it
yet — that step awaits a payee wallet address and an in-wallet
approval. Pump Radar, Wallet, and Chain Files views still use sampled
data in places; the static page holds no keys.

---

## 2. How the layers connect TODAY

Concrete integration points that exist now — each one verifiable:

| # | Connection | Evidence |
|---|---|---|
| 1 | **The OS x402 Store speaks the real x402 payment protocol** (`facilitator.x402.rs`, Solana USDC, $0.001/call): the same verify/settle flow used by x402 pay-per-call services. The gate is OFF until a payee is set, and no live payment has settled yet. | OS `index.html` (verify/settle calls). |
| 2 | **Bridge design ↔ both chains.** The bridge programs name Solana mainnet and Machine as their two endpoints and encode both chain IDs in the signed message format. This is a code-level link only: with no deployment and no relayer, no value or message moves between the chains today. | `bridge/README.md`, `bridge/VERIFY.md`, `BRIDGE.md` §2.3. |
| 3 | **Same author, same accounts.** The chain repo and the OS are published under `github.com/yourmememachine` and point at the same public repos. That is an authorship link, not a technical one. | Repo URLs in each codebase. |

What does **not** connect today:

- The OS does **not** read Machine chain data (no public Machine RPC).
- The bridge does **not** move anything (not deployed; no relayer).
- No Machine app is deployed on the chain itself — there is no
  on-chain program deployed to Machine other than the local test
  validator's system programs.

---

## 3. What is PLANNED (not claimed)

| Item | Depends on | Status |
|---|---|---|
| Public Machine RPC | 3-validator live net on a host with working UDP + fd limit 1,000,000 | NEXT — `livenet/` package exists, not deployed |
| OS reads Machine chain data | Public RPC above | NEXT |
| Bridge deployment | All `BRIDGE.md` §5 prerequisites (live net, public RPC, verified transfers, fresh keys, audit, test crossings, monitoring) | NEXT — programs built + tested locally only |
| Multi-validator consensus (3 nodes) | Host where UDP works and fd limit can be raised | NEXT — top roadmap item |
| Independent audit | Live net + bridge deployment to review | NEXT |
| Machine-native apps deployed on-chain | Public chain + verified transfers | NEXT |

---

## 4. Reading this honestly

- Saying "the ecosystem ships a day-one apps layer" is true of the
  **apps existing and working** (the OS working in-browser). It is
  not a claim that those apps run **on** Machine today — their live
  data is Solana mainnet, and the chain they belong to is a local
  testnet.
- Saying "the bridge is built" is true of the **programs and their
  tests** (188/188 locally, including 60/60 relayer, 19/19 proof
  bundle, and 15/15 receipt indexer). It is not a claim
  that bridging works — nothing is deployed, nothing has crossed,
  and no relayer has run live.
- Every "PROVEN" in `LAUNCH_CHECKLIST.md` stays proven only for the
  item and evidence named there. Anything marked NEXT there stays
  NEXT until the named proof exists.
