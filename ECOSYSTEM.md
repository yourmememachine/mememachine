# Machine Ecosystem — Architecture

**Status: 2026-11-20. This document states what exists, what connects
to what today, and what is planned. Nothing here is claimed without a
run, a file, or a command behind it.**

Machine is a Solana-derived Layer 1 (Agave 2.3.13) with its own
genesis, its own native token, a bridge, a set of native programs
built in this repo, and a browser desktop interface (Solana OS).
The chain, the programs, and the OS are separate codebases. They are
one ecosystem in the sense that they are built by the same author
for the same chain, and that the concrete integration points in §2
exist. With one exception (§2, the RugCall evidence kernel), there
is no shared code, no shared deployment, and no shared database
between them.

Repositories / locations:

| Layer | Where it lives |
|---|---|
| Core chain | This repo — https://github.com/yourmememachine/mememachine |
| Bridge programs | `bridge/` in this repo |
| Native programs | `programs/` in this repo (LaunchGuard, PQ Vault, registry export, RugCall) |
| RugCall off-chain app + resolver | `rugcall/` in this repo |
| Public status snapshot | `status/` in this repo |
| Solana OS (browser interface) | Hosted browser artifact (slug `solana-os`), source at `~/workspace/ts-spaces/solana-os/index.html` — not a GitHub repo |

Verified totals at this date (`make verify`, 2026-11-20, against
commit `5c978ad`): **707 passed, 0 failed** — bridge workspace 197,
LaunchGuard 68, PQ Vault 17, registry export 11, RugCall 127, fork
suite 49, resolver JS 99, app JS 139.

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
from the custom genesis; the vote-authority patch that makes
`solana-test-validator` vote is in this repo
(`patch-vote-authority.py`) and reproducible. On 2026-11-14 the chain
was observed voting, confirming, and finalizing (processed 20,569,
finalized 20,569 at ~2.4 slots/second).

**Honest limits.** Single validator, local testnet only. **The local
validator is STALLED at slot 29,100** (processed / confirmed /
finalized all 29,100, 29,098 transactions) since ~22:11 PST
2026-11-14; `getHealth` still returns `ok` and plain restarts from
the existing ledger re-stall at the same slot — the advancing-slots
evidence above is historical, not current (STATUS.md P3). **User
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
protection, absolute caps plus dynamic unlock caps (1% of locked
total per transaction, 5% per 216,000-slot window, quartered for one
window after any unpause), pause, and a one-way kill-switch on the
Machine side. Every mint/unlock writes a permanent crossing-receipt
PDA. `Initialize` in both programs is bound to the program's upgrade
authority, and `MintWrapped` binds the message's source mint to the
mint registered in the config. The off-chain relayer verifies the
source transfer behind each event before signing and keeps a
tamper-evident, hash-chained audit log of what it signed, collected,
and submitted. No admin mint; $MACHINE itself is never bridged.

**What it is not.** Not deployed. Not audited. The off-chain
relayer exists (`bridge/crates/relayer/`) but has never run live;
no asset has crossed, nothing here holds real funds.

**Proven.** Built and tested locally only. Re-verified 2026-11-20:
`cargo test --release` in `bridge/` — **197 passed, 0 failed**
(41 bridge-common, 32 machine-bridge, 26 solana-lock
including the precompile-cost test, 60 relayer including the
full end-to-end drill, 23 proof bundle, 15 receipt indexer).
Full output in `bridge/VERIFY.md`.

**Honest limits.** SBF builds of all five programs in this repo are
verified locally (hashes in `bridge/deploy/SBF-BUILD-NOTES.md`);
that is a build proof, not a deployment. No deployment and no audit.
All seven prerequisites in `BRIDGE.md` §5 (live 3-validator net,
public RPC, verified user transfers, fresh production keys, security
review, worthless-amount test crossings, monitoring) are unmet.

### Layer 3 — Native programs (`programs/`)

**What they are.** Four crates built in this repo, in the repo's
native Rust style, all tested locally and none deployed to Machine:

- **LaunchGuard** (`programs/crates/launch-guard/`, spec
  `programs/LAUNCH-GUARD.md`) — Machine's chain-native anti-rug
  launch standard. A launch registers only with mint and freeze
  authorities revoked and at least 10% of supply locked for at
  least 30 days (6,480,000 slots) in a program-PDA vault; locks can
  be extended, never shortened. Five instructions: `Initialize`
  (one-time, upgrade-authority-bound, sets a minimum lock floor),
  `RegisterAndLock`, `ExtendLock`, `Release`, and `CloseVault`
  (creator-only residual recovery after a completed release). A
  per-creator reputation PDA records registered/completed counters.
  **68 passed, 0 failed.** In the audit scope (AUDIT.md Scope A);
  not deployed.
- **PQ Vault** (`programs/crates/pq-vault/`, design
  `programs/PQ-VAULT.md`) — an opt-in post-quantum cold-storage
  vault whose withdrawals are authorised by RFC 8391 WOTS+
  one-time signatures (SHA-256) instead of Ed25519. Only 32-byte
  commitments sit on-chain; the program enforces one-time
  discipline (strict in-order key use, rotation only on a one-time
  signature, exhaustion fails closed). **17 passed, 0 failed.**
  Measured verification cost is 337K–395K CU, so withdrawals need a
  raised compute-budget instruction. Not deployed.
- **Registry export** (`programs/crates/registry-export/`) — an
  off-chain reader (`launchguard-registry`) that scans LaunchGuard
  record and reputation accounts and emits a machine-readable
  public JSON registry; `--verify` re-checks vault coverage and
  revoked authorities instead of trusting the record. **11 passed,
  0 failed.** Off-chain tooling only.
- **RugCall** (`programs/crates/rug-call/`, off-chain halves in
  `rugcall/`) — a parimutuel prediction market over rug evidence:
  six instructions (InitializeConfig, CreateMarket, BuyPosition,
  ResolveMarket, Claim, CancelMarket) with a per-mint lock that
  enforces one live market per mint. **127 passed, 0 failed**, plus
  99 resolver JS tests and 139 app JS tests. **RugCall is the one
  deployed program in the ecosystem — on Solana devnet only** (not
  Machine, not mainnet), program
  `DcwvEDi4yCk16wQ2HVzc3dQiBFccj8VxzboGUYdUXseZ`, custoding only
  devnet play collateral (dUSDC); one smoke market holds 350 dUSDC
  across both sides. Its resolver's first live pass deferred
  fail-closed when the public mainnet RPC rate-limited the evidence
  calls (HTTP 429); nothing was signed or submitted. Devnet-only,
  and it gates nothing in the bridge audit scope.

**What they are not.** LaunchGuard and PQ Vault are not deployed
anywhere. RugCall's devnet deployment is a hackathon entry with
play collateral, not a Machine deployment and not a bridge
deployment.

### Layer 4 — Browser interface (Solana OS)

**What it is.** A separate browser-based desktop artifact by the
same author (v2, rebuilt from scratch on 2026-11-16): a
Windows-style desktop whose apps are browser apps, not programs
deployed on Machine. The registry in the source lists 18 apps,
including Wallet ($MACHINE · localnet), LaunchGuard, Rug Shield,
Pump Radar, x402 Store, MCP Hub, Terminal, Chain Files, and
**Clerk** — a resident agent that sweeps meme coins on live Solana
mainnet data and judges them by bundle patterns, side wallets
linked to one funder, and creator rug history (never by mint/freeze
authority alone, which launchpads revoke at creation), and can
block its own demo-wallet payment when LaunchGuard evidence says
not to pay. The slot shown in the OS is the **Solana mainnet**
slot, not Machine's.

**What it is not.** It is not the chain, not a validator, and not
currently reading Machine chain data. Today its live data comes from
Solana mainnet (DexScreener + public Solana RPC) — there is no public
Machine RPC for it to read yet. It is **not publicly launched**:
the launch gate is full setup first, then bug-hunt reviews on the
completed OS, fixes, a full-system demo recording, and only then
launch.

**Honest limits.** Wallet connect is a local preview only — the
Phantom / Solflare / Backpack buttons request no signature and
nothing leaves the page. x402 Store payments are demo funds only
(0.001 SOL per call inside the OS, from Clerk's capped demo
wallet); no real x402 payment has settled and no real funds move.
Rug Shield alone is a live mint-authority/supply checker; the full
launch detection is Clerk's. Pump Radar reads a live DexScreener
feed; Wallet figures are localnet demo numbers, labelled as such;
session state (watchlist, flags, receipts) resets when the page
closes. The static page holds no keys.

### Layer 5 — Public status snapshot (`status/`)

**What it is.** A one-page static status snapshot and its
generator, built only from STATUS.md, the VERIFY logs, and the
last bridge drill. It discloses the stalled local validator and
never claims a live chain.

**What it is not.** Not a live status service and not hosted
monitoring — a generated snapshot that is only as fresh as its
last regeneration.

---

## 2. How the layers connect TODAY

Concrete integration points that exist now — each one verifiable:

| # | Connection | Evidence |
|---|---|---|
| 1 | **Clerk's demo payments run on the OS's x402 rail, in demo funds only.** When Clerk needs a data call it pays the x402 Store from its own capped demo wallet and files the receipt in Chain Files. No real payment has settled and no real funds move. | OS `index.html` (Clerk ledger + x402 Store). |
| 2 | **Bridge design ↔ both chains.** The bridge programs name Solana mainnet and Machine as their two endpoints and encode both chain IDs in the signed message format. This is a code-level link only: with no deployment and no relayer running live, no value or message moves between the chains today. | `bridge/README.md`, `bridge/VERIFY.md`, `BRIDGE.md` §2.3. |
| 3 | **Registry export → LaunchGuard.** The registry reader consumes LaunchGuard's on-chain record and reputation accounts and republishes them as machine-readable JSON — the format wallets, bots, and checkers can consume. | `programs/crates/registry-export/` (P24 in STATUS.md). |
| 4 | **RugCall shares the OS verdict kernel.** `rugcall/app/evidence.js` is extracted from the Solana OS Clerk/Rug Shield export — the one concrete code link between the OS and a program's off-chain half. The RugCall resolver evaluates markets against that kernel and, in live mode, submits to the devnet program. | STATUS.md P38; `rugcall/app/`, `rugcall/resolver/`. |
| 5 | **Status snapshot → repo evidence.** `status/generate.py` builds the public snapshot only from STATUS.md, the VERIFY logs, and the last drill, and refuses to write on a missing value. | `status/generate.py`, `status/README.md`. |
| 6 | **Same author, same accounts.** The chain repo is published under `github.com/yourmememachine`; the OS is a hosted artifact by the same author and links to that repo. That is an authorship link, not a technical one. | Repo URLs in each codebase. |

What does **not** connect today:

- The OS does **not** read Machine chain data (no public Machine RPC).
- The bridge does **not** move anything (not deployed; no relayer has run live).
- No program is deployed **to Machine** — the chain's only programs
  are the local test validator's system programs. RugCall's
  deployment is on Solana devnet, not on Machine.

---

## 3. What is PLANNED (not claimed)

| Item | Depends on | Status |
|---|---|---|
| Public Machine RPC | 3-validator live net on a host with working UDP + fd limit 1,000,000 | NEXT — `livenet/` package exists, not deployed |
| OS reads Machine chain data | Public RPC above | NEXT |
| Bridge deployment | All `BRIDGE.md` §5 prerequisites (live net, public RPC, verified transfers, fresh keys, audit, test crossings, monitoring) | NEXT — programs built + tested locally only |
| Multi-validator consensus (3 nodes) | Host where UDP works and fd limit can be raised | NEXT — top roadmap item |
| Independent audit | Funding + a deployment to review | NEXT — Oak Seed quote received ($26,840); no deposit committed |
| LaunchGuard / PQ Vault deployment | Public chain + verified transfers + audit | NEXT — built + tested locally only |
| Solana OS public launch | Full setup, bug-hunt reviews, fixes, full-system demo | NEXT — gated, not launched |

---

## 4. Reading this honestly

- Saying "the ecosystem ships a day-one apps layer" is true of the
  **apps existing and working in-browser as a prototype**. It is
  not a claim that those apps run **on** Machine today — their live
  data is Solana mainnet, the OS is not publicly launched, and the
  chain they belong to is a local testnet whose validator is
  currently stalled at slot 29,100.
- Saying "the bridge is built" is true of the **programs and their
  tests** (197/197 locally, including 60/60 relayer, 23/23 proof
  bundle, and 15/15 receipt indexer). It is not a claim
  that bridging works — nothing is deployed, nothing has crossed,
  and no relayer has run live.
- Saying "a program is deployed" is true of **exactly one**:
  RugCall, on Solana devnet, with play collateral only. It is not
  a claim about Machine, mainnet, or the bridge.
- Every "PROVEN" in `STATUS.md` stays proven only for the
  item and evidence named there. Anything marked NEXT there stays
  NEXT until the named proof exists.
