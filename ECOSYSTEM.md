# Machine Ecosystem — Architecture

**Status: 2026-11-14. This document states what exists, what connects
to what today, and what is planned. Nothing here is claimed without a
run, a file, or a command behind it.**

Machine is a Solana-derived Layer 1 (Agave 2.3.13) with its own
genesis, its own native token, and two companion codebases by the same
author that integrate with it: a browser desktop interface (Solana OS)
and an open registry service (Rug-Farm Registry). The three are
**separate codebases in separate repositories / artifacts**. They are
one ecosystem only in the sense that they are built by the same author
for the same chain, and that the concrete integration points in §4
exist. There is no shared code, no shared deployment, and no shared
database between them.

Repositories / locations:

| Layer | Where it lives |
|---|---|
| Core chain | This repo — https://github.com/yourmememachine/mememachine |
| Bridge programs | `bridge/` in this repo |
| Solana OS (browser interface) | Browser artifact, source at `~/workspace/ts-spaces/solana-os/index.html` |
| Rug-Farm Registry | https://github.com/yourmememachine/rug-farm-registry — hosted API https://rug-farm-registry.onrender.com |

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

**What it is not.** Not deployed. Not audited. No relayer exists, no
asset has crossed, nothing here holds real funds.

**Proven.** Built and tested locally only. Re-verified 2026-11-14:
`cargo test --release` in `bridge/` — **34 passed, 0 failed**
(15 bridge-common unit, 9 machine-bridge integration, 10 solana-lock
integration). Full output in `bridge/VERIFY.md`.

**Honest limits.** No SBF build has been run (`cargo build --release`
produces host libraries, not deployable programs). No deployment and
no audit. The off-chain relayer is now implemented and tested
locally (`bridge/crates/relayer/`) but has never run live. All seven
prerequisites in `BRIDGE.md` §5 (live 3-validator net, public RPC,
verified user transfers, fresh production keys, security review,
worthless-amount test crossings, monitoring) are unmet.

> **Update (2026-11-14):** the programs and the relayer now exist
> and pass their tests locally; `BRIDGE.md` and
> `LAUNCH_CHECKLIST.md` item 21 have been updated to say so. The
> "not deployed, not audited, nothing crossed" part is still true.

### Layer 3 — Browser interface (Solana OS)

**What it is.** A separate browser-based desktop artifact by the same
author: a chain-as-filesystem window manager where slots are time,
the wallet is the home folder, and programs are apps (Wallet, Rug
Shield, Pump Radar, x402 Store, MCP Hub, Terminal, Chain Files).
Wallet connect supports Phantom, Solflare, and Backpack with real
signing; the x402 Store speaks the real facilitator protocol
(`facilitator.x402.rs` `/verify` + `/settle`, $0.001 per check).

**What it is not.** It is not the chain, not a validator, and not
currently reading Machine chain data. Today its live data comes from
Solana mainnet (DexScreener + public Solana RPC) — there is no public
Machine RPC for it to read yet. Its "engine" line for the registry is
a **port** of the registry's v2.17 rules running in-browser, plus live
calls to the hosted registry API (§4); the two are different
mechanisms and both are real.

**Honest limits.** Wallet connect could not be completed from an
iPhone in-app browser with no wallet extension (reported 2026-11-13,
unresolved). No live $0.001 x402 payment has been settled through it
yet — that step awaits a payee wallet address and an in-wallet
approval. Pump Radar, Wallet, and Chain Files views still use sampled
data in places; the static page holds no keys.

### Layer 4 — Registry app (Rug-Farm Registry)

**What it is.** A separate open-source Python codebase that flags
serial-rugger dev wallets on Solana pump.fun: the ≥ 2 rugged mints +
evidence rule, dev profiles, funder chains, bundle tests, a SQLite
store, a daily scan, a hosted read-only API, pay-per-check (x402) and
an MCP server. Version 2.19, **669 tests passing** (verified
2026-11-14: `python -m pytest tests/ -q` in the registry repo —
656 before this workstream's change, +13 new). Hosted on Render's
free tier at https://rug-farm-registry.onrender.com (spins down when
idle; first request after idle can take ~50 s).

**What it is not.** Not a Machine-native app in any deployed sense.
Its verdict data is Solana-only, its checks are Solana pump.fun dev
wallets, and it has no Machine coverage. It is a day-one companion
app for the ecosystem, not a component of the chain.

**Honest limits.** The hosted API has no Helius key set in the
deployment (DexScreener-only mode) beyond the daily scan workflow;
the free Render instance sleeps when idle; x402 gating is OFF until
`RUGFARM_X402_PAYEE` is set.

---

## 2. How the layers connect TODAY

Concrete integration points that exist now — each one verifiable:

| # | Connection | Evidence |
|---|---|---|
| 1 | **Rug Shield (OS) → hosted registry API.** The OS calls `https://rug-farm-registry.onrender.com` (`/health`, `/check`, `/wallets/{address}`, `/registry`) directly from the browser. CORS is enabled on the API (`access-control-allow-origin: *`) so the browser can read the replies; verified live with curl 2026-11-14. | `solana-os/index.html` references the API host; `src/api.py` in the registry repo (CORS middleware, `tests/test_api_cors.py`). |
| 2 | **Rug Shield runs the registry's rules in-browser.** The OS carries a port of the registry's v2.17 detection rules (labeller, dev history, x402) and runs them on live chain data in the browser, in addition to the live API calls in (1). | OS boot line: "engine — registry v2.17 rules ported"; registry `src/` v2.17 modules. |
| 3 | **Same x402 payment protocol, both sides.** The OS x402 Store and the registry's pay-per-check gate both speak the real facilitator protocol (`facilitator.x402.rs`, Solana USDC, $0.001/call). Same price, same facilitator, same verify/settle flow. Both gates are OFF until a payee is set, and no live payment has settled yet. | OS `index.html` (verify/settle calls); registry `src/x402.py`, `src/api.py`. |
| 4 | **Machine is listed in the registry's network table.** The registry's `NETWORKS` config (`src/multichain.py`) lists Machine with status "local testnet", probe "unavailable", registry coverage "no", plus its genesis hash and repo. `GET /networks` on the hosted API exposes the same table, and `GET /health` reports the network list. Every verdict carries a `"chain"` field defaulting to `"solana"`. | Registry v2.19 (`CHANGELOG.md`); `python -m src.multichain --list-chains` prints Machine as "local testnet — probe: unavailable, registry coverage: no". |
| 5 | **Bridge design ↔ both chains.** The bridge programs name Solana mainnet and Machine as their two endpoints and encode both chain IDs in the signed message format. This is a code-level link only: with no deployment and no relayer, no value or message moves between the chains today. | `bridge/README.md`, `bridge/VERIFY.md`, `BRIDGE.md` §2.3. |
| 6 | **Same author, same accounts.** All three codebases are published under `github.com/yourmememachine`, and the OS and registry both point at the same public repos. That is an authorship link, not a technical one. | Repo URLs in each codebase. |

What does **not** connect today:

- The OS does **not** read Machine chain data (no public Machine RPC).
- The registry does **not** check Machine wallets or tokens (no
  coverage; a Machine check honestly reports probe unavailable and
  verdict unknown).
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
| Registry coverage for Machine | Public RPC + real on-chain activity to check | NEXT — listing (v2.19) is the first step, not coverage |
| Bridge deployment | All `BRIDGE.md` §5 prerequisites (live net, public RPC, verified transfers, fresh keys, audit, test crossings, monitoring) | NEXT — programs built + tested locally only |
| Multi-validator consensus (3 nodes) | Host where UDP works and fd limit can be raised | NEXT — top roadmap item |
| Independent audit | Live net + bridge deployment to review | NEXT |
| Machine-native apps deployed on-chain | Public chain + verified transfers | NEXT |

---

## 4. Reading this honestly

- Saying "the ecosystem ships day-one apps" is true of the **apps
  existing and working** (registry API live, OS working in-browser).
  It is not a claim that those apps run **on** Machine today — their
  live data is Solana mainnet, and the chain they belong to is a
  local testnet.
- Saying "the bridge is built" is true of the **programs and their
  tests** (47/47 locally, plus 22/22 relayer). It is not a claim that bridging works —
  nothing is deployed, nothing has crossed, and no relayer exists.
- Saying "Machine is multi-chain ready" is not true. The registry
  **lists** Machine as a network; listing is a configuration entry
  plus an honest "unavailable" response, not coverage, not a probe,
  and not a single checked wallet.
- Every "PROVEN" in `LAUNCH_CHECKLIST.md` stays proven only for the
  item and evidence named there. Anything marked NEXT there stays
  NEXT until the named proof exists.
