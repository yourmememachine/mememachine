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
exists at genesis, and the founder balance decreases only by
vote-transaction fees paid in $MACHINE.

| Allocation | Amount ($MACHINE) |
|---|---|
| Founder | 998,999,000 |
| Stake | 1,000,000 |
| Faucet | 1,000 |
| System | ~0.2536 |
| **Total** | **1,000,000,000** |

The distribution is deliberately simple and fully published. There is no
hidden allocation, no second mint, and no undisclosed reserve. The live
network will use a fresh genesis with a new hash — the localnet genesis
has a single bootstrap validator baked in and cannot be extended to three
validators — with the same structure: a treasury allocation, one million
$MACHINE per validator identity, one million per stake account, and a
1,000 $MACHINE faucet.

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
- The founder holds the founding allocation stated in Section 3. That
  concentration is disclosed, not hidden; the live network's fresh
  genesis is the point at which distribution can be set deliberately
  rather than inherited from the localnet.
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
