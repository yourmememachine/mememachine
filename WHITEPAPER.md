# Machine — Whitepaper

**Chain:** Machine · **Token:** $MACHINE · **Version:** 1.0 — 2026-11-14

Machine is a Solana fork: its own chain, its own genesis, its own native
token. This document states what Machine is, what it has proven, and what
it has not. Nothing in it is claimed without evidence.

Repository: https://github.com/yourmememachine/mememachine
Website: machine.ai (domain not yet registered)

---

## 1. Introduction

Machine is an independent Layer 1 derived from Solana. It runs Agave
validator software against a custom genesis, with $MACHINE as the native
gas token from the first block. The chain exists today as a single-node
localnet that boots, votes, confirms, and finalizes. The next step is a
3-validator live network, for which the deployment package already exists
in the repository under `livenet/`.

Machine was built to be a working chain first and a narrative second. Its
genesis, fee schedule, and token distribution are published on day one.
Day-one applications ship with it: the Rug-Farm Registry, an open registry
and API for Solana, and Solana OS, a browser-native applications layer.

## 2. Architecture and Consensus

### 2.1 Software

Machine runs **Agave 2.3.13, prebuilt release binaries only**. Nothing was
compiled from source. The install path downloads the release; the chain is
reproducible from the repository without a build toolchain.

### 2.2 Genesis

| Field | Value |
|---|---|
| Genesis hash | `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb` |
| Shred version | 35836 |
| Founder (identity) | `7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz` |
| Vote account | `GMe5eK5iefNXFN3NbFUx7ge27JQQDi1irsnCkYJdzcDA` |
| Stake account | `CxRRi4HZqJoG3fWugwF5wT2bqVwyMgPXxfKDRWCfe87e` |
| Faucet | `FVFXT54BR2W6qmbnMuoCcHiufxX47LjXh1Nhissku7KC` |
| Total supply | 1,000,000,000 $MACHINE |

### 2.3 Consensus

Machine inherits Solana's proof-of-stake consensus: validators vote on
slots, votes accumulate as credits, and slots are confirmed and then
finalized (rooted) as votes converge. Finality is vote-based, not
time-based — a slot is final once a supermajority of stake has voted for
it.

One implementation detail is documented because it determines whether the
chain votes at all. `solana-genesis` sets a vote account's authorized
voter to the identity keypair, but the validator votes with the vote
keypair, so a stock genesis never confirms. The repository contains
`patch-vote-authority.py`, which patches the genesis so the authorized
voter is the vote keypair while the founder keeps withdraw authority.
The script has layout checks and aborts on mismatch; it is the documented
reason the localnet votes.

### 2.4 Observed performance

Verified on the localnet: slot 1,649 and advancing, confirming and
finalizing, at approximately 3 slots per second. Vote credits grow
continuously. Full snapshots are produced (snapshot at slot 1,100
observed). These numbers are from the single-node localnet; they are a
lower bound on what the software does, not a claim about a multi-validator
network.

## 3. Tokenomics and Distribution

$MACHINE is the native gas token. It exists from genesis: no other token
exists at genesis.

### 3.1 Localnet distribution (observed, dev-only)

This is the distribution of the single-node localnet that exists today.
It is a throwaway dev genesis, not the live economics — see the note at
the end of this section.

| Allocation | Amount ($MACHINE) |
|---|---|
| Founder | 998,999,000 |
| Stake | 1,000,000 |
| Faucet | 1,000 |
| System | ~0.2536 |
| **Total** | **1,000,000,000** |

The localnet distribution is deliberately simple and fully published.
There is no hidden allocation, no second mint, and no undisclosed
reserve on the localnet.

### 3.2 Livenet distribution (the live network)

The live network uses a fresh genesis with a new hash — the localnet
genesis has a single bootstrap validator baked in and cannot be extended
to three validators. The livenet allocation is balanced: no single
holder is near the whole supply. Each of the five primordial buckets has
its own keypair at genesis (`livenet/generate-keys.sh` creates
`founder.json`, `ecosystem.json`, `community.json`,
`validator-rewards.json`, and `treasury.json`), so the split is on-chain
from the first block, not just documented. `livenet/generate-genesis.sh`
implements exactly this table.

| Allocation | Amount ($MACHINE) | Share |
|---|---|---|
| Founder / team (vested — see below) | 150,000,000 | 15.0000% |
| Ecosystem fund | 250,000,000 | 25.0000% |
| Community / airdrop | 200,000,000 | 20.0000% |
| Validator rewards reserve | 150,000,000 | 15.0000% |
| Treasury / operations | 243,999,000 | 24.3999% |
| Validator identities (3 × 1,000,000) | 3,000,000 | 0.3000% |
| Validator stakes (3 × 1,000,000) | 3,000,000 | 0.3000% |
| Faucet | 1,000 | 0.0001% |
| **Total** | **1,000,000,000** | **100%** |

Why these buckets: the founder/team share is capped at 15% and vested;
the ecosystem fund (25%) is the largest bucket because grants,
integrations, and liquidity for applications built on the chain are the
main use of supply after launch; community (20%) funds distribution to
users rather than insiders; the validator rewards reserve (15%) pays for
securing the chain beyond the 6,000,000 $MACHINE of genesis validator
funding; treasury (24.3999%) is the residual that makes the total exact
and covers operations. There is no hidden allocation, no second mint,
and no undisclosed reserve in the livenet genesis either.

### 3.3 Founder / team vesting

The 150,000,000 $MACHINE founder/team allocation vests over 48 months
from livenet genesis:

- **Months 0–12 (cliff):** 0 $MACHINE unlocked. Nothing from this bucket
  may be sold or transferred before the cliff.
- **Months 13–48:** linear vesting, 1/36 of the allocation per month
  (about 4,166,667 $MACHINE; the final month is adjusted so the total
  vested is exactly 150,000,000).
- **Month 48:** fully vested.

Honest limit on this schedule: it is a published commitment, not yet a
code-enforced lockup. The livenet genesis holds the founder/team
allocation in a plain System account under its own keypair; no on-chain
vesting program or multisig exists yet to enforce the cliff. Until such
a program is deployed, compliance with the schedule is verifiable only
by watching the founder keypair's balance and transfers on-chain. No
claim is made here that the vesting is enforced by code.

> **The localnet genesis is dev-only and is NOT the livenet economics.**
> The localnet genesis (`32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`)
> is a throwaway dev genesis whose founder holds ~999M of 1B $MACHINE.
> That founder-weighted distribution exists only because the localnet
> was built for testing a single validator, and it must not be quoted
> as the livenet allocation. The livenet allocation is the balanced
> table in Section 3.2 above, implemented by `livenet/generate-genesis.sh`
> in a fresh genesis with a new hash.

## 4. Fee Model

The fee schedule is set in genesis and published here in full.

| Parameter | Value |
|---|---|
| Fee per signature | 9,500 lamports |
| Target fee | 10,000 lamports |
| Minimum fee | 5,000 lamports |
| Maximum fee | 100,000 lamports |
| Burn | 50% of fees |
| Target signatures per slot | 20,000 |

Fees are paid in $MACHINE. Half of every fee is burned, so fee volume
reduces supply over time. The fee schedule is fixed in genesis; changing
it requires a network upgrade, not a quiet edit.

## 5. Roadmap

Each item is labelled **PROVEN** (with its evidence) or **NEXT** (with
what it needs). Nothing is marked proven on a claim.

### PROVEN

| Item | Evidence |
|---|---|
| Chain boots from a custom genesis | Localnet running on the repository's `ledger/`; genesis hash `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`, shred version 35836 |
| Genesis hash published | `GENESIS.md` |
| Validator keys published | `GENESIS.md` — identity, vote, stake, and faucet public keys |
| Fee schedule published | `GENESIS.md` and Section 4 of this document |
| Distribution written down | `GENESIS.md` and Section 3 of this document |
| $MACHINE is the native gas token from genesis | Founder balance decreases only by vote-transaction fees (998,999,000 → 998,998,999.99411), paid in $MACHINE; no other token exists at genesis |
| Slots advancing | Slot 1,649 and rising, ~3 slots/second |
| Voting and confirming | Confirmed slot tracking the processed slot; vote credits growing; vote authority is the vote keypair, withdraw authority is the founder |
| Finalizing | Finalized slot tracking behind confirmed; root slot advancing |
| Snapshots working | Full snapshot at slot 1,100 |
| Balances correct | Founder 998,998,999.99411, faucet 1,000, stake 1,000,000 — all via `solana balance` on the live RPC |
| Day-one apps shipped | Rug-Farm Registry (public repository, hosted API) and Solana OS (browser-native apps layer) |
| Fast blocks | ~3 slots/second observed on the localnet, confirming and finalizing continuously |
| Wallet support at the OS layer | Solana OS wallet connect supports Phantom, Solflare, and Backpack (real USDC signing) |
| Prebuilt binaries only | Agave 2.3.13 release binaries in `bin/`; `install.sh` downloads the release; nothing compiled from source |
| Vote-authority fix reproducible | `patch-vote-authority.py` in the repository, with layout checks that abort on mismatch |
| Whitepaper published | This document |

### NEXT

| Item | What it needs |
|---|---|
| Transfer between two fresh keypairs, with a signature | A host where UDP send works. On the current VM the container blocks UDP send, which breaks RPC-to-TPU forwarding; the faucet also fails there. On a host with working UDP: start the validator, fund a fresh keypair, transfer — the signature is the proof. |
| Multi-validator: 3 nodes reaching consensus | A host with `--ulimit nofile=1000000` and working UDP. The `livenet/` deployment package (3-validator genesis, systemd units, cluster check script) is already in the repository. |
| Public RPC endpoint | Hosting: a machine with a public IP and the validator's RPC exposed. Not claimed until it answers. |
| Chain-level wallet support | A public RPC endpoint plus the published chain identity, so wallets can add the Machine network. |
| Bridge (Solana mainnet ↔ Machine) | A bridge implementation (lock/mint or equivalent) plus a security review. Nothing is claimed as bridged until a real asset crosses. |
| Fair-launched token bridged in as the flagship ecosystem asset | A fair launch on Solana mainnet, then bridging to Machine. $MACHINE stays the gas token; the bridged token is the ecosystem asset. |
| Audit | An independent audit of the validator configuration and any bridge or program code, with the report published. |
| Bug bounty | A published bounty program with scope and rewards. |
| Public chain | Hosted validators, stake, monitoring, incident response, and a public RPC. A separate step, not claimed now. |

## 6. Governance

Machine has no on-chain governance program today, and this document does
not pretend otherwise. Current state:

- The genesis, fee schedule, and distribution are fixed in the
  repository and published. Changing them requires a new genesis or a
  network upgrade, both of which are public events.
- The founder holds the 15% vested founding allocation stated in
  Section 3.2, with a 12-month cliff and 36 months of linear vesting
  thereafter. That allocation is disclosed, not hidden; the localnet's
  founder-weighted dev distribution does not carry over to the
  livenet.
- The day-one applications (Rug-Farm Registry, Solana OS) are open
  source and independently verifiable.

The direction is toward validator-set governance as the network becomes
multi-validator: protocol changes proposed in the open, decided by the
validators securing the chain, and executed as scheduled upgrades. No
governance token is planned, and no governance claim is made until a
mechanism exists and is used.

## 7. Honest Limits

Stated plainly, so no one has to discover them:

- **No token transfer has been verified yet.** The chain votes and
  finalizes, but no transfer between two fresh keypairs has produced a
  signature. This needs a host where UDP works; it is the first proof
  to obtain on such a host.
- **1 validator today, not 3.** Machine is a single-node localnet. The
  3-validator deployment package exists in `livenet/`, but the live
  network is not running.
- **No bridge exists yet.** No asset has crossed between Solana mainnet
  and Machine, and none will be claimed as bridged until one does.
- **No audit yet.** Neither the validator configuration nor any future
  bridge or program code has been independently audited.
- **No public RPC yet.** There is no public endpoint to query; the
  localnet RPC is local only.
- **The website domain is not registered.** machine.ai is the intended
  address; until it is registered and the site is hosted there, the
  website lives on its current share link.

Machine is a real chain with real limits. The roadmap in Section 5 is
the list of proofs that close those limits, in order.
