# Machine — Whitepaper

**Chain:** Machine · **Token:** $MACHINE · **Version:** 2.0 — 2026-11-16
**Status:** Pre-launch. Localnet proven; unaudited; no real assets.

> Machine was previously named MemeMachine. The project was renamed on
> 2026-11-16; grant and hackathon applications submitted in November 2026
> under the former name refer to this same project and this same code.

**Repository:** https://github.com/yourmememachine/mememachine
**X:** https://x.com/yourmememachine · **Telegram:** https://t.me/Greenpumpsol

---

## Abstract

Machine is an independent Layer 1 blockchain derived from Solana's Agave
validator software, with its own genesis and $MACHINE as its native gas
token. It exists to answer a specific failure of the current token
landscape: launches are cheap to create and cheaper to abandon, and the
bridges that move value between chains keep failing in the same,
well-documented ways.

Machine's answer is to move the protections into the chain itself:

1. **LaunchGuard**, a chain-native launch standard. A token that
   registers with LaunchGuard must have its mint and freeze authorities
   permanently revoked, and its creator must lock at least 10% of supply
   for at least 30 days in a program vault. Locks can be extended, never
   shortened. There is no admin key. Compliance is not a promise a
   creator makes — it is a state any observer can verify from on-chain
   accounts, and every launch builds a public, permanent creator
   reputation record.
2. **A native bridge designed against the published post-mortems** of
   bridge failures: quorum attestation verified on-chain through the
   ed25519 precompile, two-layer transfer caps, replay protection on
   every path, a pause and kill switch that never traps user funds, a
   permanent receipt for every crossing, and relayers that verify the
   source transfer on-chain before they sign anything.

Everything claimed in this paper is implemented and tested in the public
repository, or is explicitly labelled as not yet done. Machine today is
a single-node localnet with 190 automated tests passing across its
bridge, launch standard, registry export, and fork checks. It is not audited, it is not
deployed, and no real asset has crossed its bridge. The launch is
staged and gated on an independent audit funded through public grants —
because a chain that asks for trust should first earn it in the open.

---

## 1. The problem

### 1.1 Launches rug because nothing stops them

Token creation on modern chains is nearly free, and the numbers show
what that produces. On a single day in September 2026, roughly 263,000
new SPL tokens were minted on Solana. Fewer than 1.15% of pump.fun
launches "graduate." A 2026 study (SolRugDetector) that screened 100,063
Solana tokens flagged 76,469 as rug candidates and measured a median of
about 35 minutes from token creation to the creator's first
profit-taking.

The study's most important finding is *why* the usual defences fail:
pump-and-dump schemes typically use legitimately held insider supply,
so revoking mint authority — the check most launchpads advertise —
does nothing. The signal that actually separates launches that survive
from launches that rug is whether insider supply is **locked**.
Industry data (Streamflow, 2026) points the same way: projects with
locked supply retain 58–72% of their liquidity 30–90 days after launch,
against 12–28% for unlocked projects.

Today, locking is optional, off-chain, and verified by screenshots.
Machine makes it a property of the chain.

### 1.2 Bridges fail in patterns, not surprises

Cross-chain bridges have lost billions, and the post-mortems rhyme:

- **Phantom deposits.** In August 2026 the Coreum bridge was drained
  (~200,000 XRP) because its relayers attested a deposit that never
  happened — they treated a transaction in the bridge account's history
  as proof of delivery instead of verifying the transfer itself.
- **Forged events.** In July 2026 an attacker forged 1,627 deposit
  events against a major intent bridge, roughly $41.7M in face value.
- **Trusted records.** In October 2026 the Cysic bridge was exploited
  through a backend database: forged records were enough for signers
  to mint 10M tokens. The fix, in the project's own post-mortem:
  signers must independently verify the source transaction.
- **Contained by caps.** In June 2026 the Taiko bridge lost ~$1.75M
  through a compromised prover — but per-asset limits and an emergency
  pause are what kept the loss at that figure.

None of these required new mathematics to prevent. They required
design discipline: verify on-chain, never trust a record; bound the
blast radius with caps; keep an exit open for users at all times.
Machine's bridge is built line-by-line against this list (Section 6).

### 1.3 Why a new chain

A launch standard bolted onto an existing chain is an app competing
for attention. A launch standard *native to a chain* is a rule of the
world: every launch on Machine can be checked against the same
registry, every creator accumulates reputation in the same place, and
the bridge that serves the chain is governed by the same conservative
rules. Sovereignty over the rules is the point — Machine sets its own
genesis, fee schedule, and program set, and publishes all of them.

---

## 2. Design principles

1. **Safety by construction, not by policy.** If a protection matters,
   it is enforced by a program or by arithmetic — quorum thresholds,
   caps, replay nonces — not by an operator's promise.
2. **Verify on-chain; never trust a record.** Every claim a relayer
   signs is checked against the source chain's state. Every claim in
   this paper is checkable against the repository.
3. **The exit is always open.** Pause and kill switches stop new risk;
   they never trap funds already in the system. Burns and withdrawals
   of wrapped assets stay available even when minting is halted.
4. **Publish the limits with the features.** Threat model, known
   limitations, and audit status are documents in the repository, not
   discoveries for users to make.
5. **Staged, gated deployment.** Public proof first, testnet second,
   value last — and value only after an independent audit. No stage is
   skipped because a later one is more exciting.

---

## 3. Architecture

### 3.1 Base software

Machine runs **Agave 2.3.13**, the Solana validator implementation,
using the stock prebuilt release binaries with a custom genesis. The
fork is deliberately shallow: three small source patches exist in the
repository (`patches/`, documented in `FORK.md`) — a client identifier
(wire value 4), a startup banner, and a snapshot-interval change. The
patched source has **not** been built or run yet; the chain that exists
today runs the unmodified release binaries. Machine's originality is
not in re-writing a validator — it is in the programs, the bridge, and
the launch standard built on top of a proven base.

### 3.2 Genesis

| Field | Value |
|---|---|
| Genesis hash | `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb` |
| Shred version | 35836 |
| Founder (identity) | `7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz` |
| Vote account | `GMe5eK5iefNXFN3NbFUx7ge27JQQDi1irsnCkYJdzcDA` |
| Stake account | `CxRRi4HZqJoG3fWugwF5wT2bqVwyMgPXxfKDRWCfe87e` |
| Faucet | `FVFXT54BR2W6qmbnMuoCcHiufxX47LjXh1Nhissku7KC` |
| Total supply | 1,000,000,000 $MACHINE (fixed) |

Consensus is Solana's proof-of-stake: validators vote on slots, and a
slot is final once a supermajority of stake has voted for it — finality
is vote-based, not time-based. The localnet boots, votes, confirms, and
finalizes continuously (verified; see `STATUS.md` for the commands that
reproduce every claim).

One implementation detail is documented because it decides whether the
chain votes at all: `solana-genesis` sets a vote account's authorized
voter to the identity keypair, but the validator votes with the vote
keypair, so a stock genesis never confirms. The repository's
`patch-vote-authority.py` corrects the genesis (authorized voter = vote
keypair; withdraw authority stays with the founder) with layout checks
that abort on mismatch.

### 3.3 What is proven, and what is not

Proven on the localnet: boot from custom genesis, slot production,
voting, confirmation, finalization, snapshots, correct balances.
Not yet proven — because the development environment blocks UDP send
and caps open files, both of which a validator network needs: signed
transfers between fresh keypairs, multi-validator consensus, and a
public RPC endpoint. These are host requirements, not code unknowns;
the deployment packages for them exist in the repository
(`testnet/`, `ops/`), and `STATUS.md` tracks each item as PROVEN or
NEXT with its blocker stated.

---

## 4. Tokenomics and fees

### 4.1 $MACHINE

$MACHINE is the native gas token, created at genesis with a fixed
supply of 1,000,000,000. Three commitments define it:

- **It is never sold.** There is no token sale, no presale, and no
  launch event for $MACHINE itself.
- **It is never bridged.** The bridge (Section 6) moves approved SPL
  assets between Solana and Machine. $MACHINE stays home.
- **Its distribution is published** — for the dev genesis that exists
  today, and for the livenet genesis that will replace it (4.3).

### 4.2 Fees

The fee schedule is set in genesis and published in full. Changing it
requires a network upgrade, not a quiet edit.

| Parameter | Value |
|---|---|
| Fee per signature | 9,500 lamports |
| Target fee | 10,000 lamports |
| Minimum fee | 5,000 lamports |
| Maximum fee | 100,000 lamports |
| Burn | 50% of fees |
| Target signatures per slot | 20,000 |

Half of every fee is burned, so fee volume reduces supply over time.

### 4.3 Distribution

The **localnet genesis** that exists today is a throwaway development
genesis: founder 998,999,000, stake 1,000,000, faucet 1,000. It is
deliberately simple, fully published, and must not be quoted as the
live economics.

The **livenet genesis** uses a fresh genesis with a new hash (the
localnet genesis has a single bootstrap validator baked in and cannot
be extended). Its allocation is implemented by
`livenet/generate-genesis.sh`, with a separate keypair per bucket so
the split exists on-chain from the first block:

| Allocation | Amount ($MACHINE) | Share |
|---|---|---|
| Founder / team (vested, 4.4) | 150,000,000 | 15.0000% |
| Ecosystem fund | 250,000,000 | 25.0000% |
| Community / airdrop | 200,000,000 | 20.0000% |
| Validator rewards reserve | 150,000,000 | 15.0000% |
| Treasury / operations | 243,999,000 | 24.3999% |
| Validator identities (3 × 1,000,000) | 3,000,000 | 0.3000% |
| Validator stakes (3 × 1,000,000) | 3,000,000 | 0.3000% |
| Faucet | 1,000 | 0.0001% |
| **Total** | **1,000,000,000** | **100%** |

The ecosystem fund is the largest bucket because grants, integrations,
and application liquidity are the main productive use of supply after
launch; the founder share is capped at 15% and vested. There is no
hidden allocation, no second mint, and no undisclosed reserve in
either genesis.

### 4.4 Founder vesting — and its honest limit

The 150,000,000 founder/team allocation vests over 48 months from
livenet genesis: a 12-month cliff (nothing unlocked), then linear
vesting of 1/36 per month for 36 months.

Stated plainly: this schedule is a **published commitment, not yet a
code-enforced lockup**. The livenet genesis holds the allocation in a
plain account under its own keypair; no vesting program or multisig
enforces the cliff yet. Until one is deployed, compliance is verifiable
by watching the founder keypair's balance on-chain — which is exactly
why the keypair is published. No stronger claim is made.

---

## 5. LaunchGuard: the native launch standard

LaunchGuard is a program that lives on Machine and defines what a
protected launch *is*. Any token can exist on Machine; a token that
wants the LaunchGuard designation must satisfy rules enforced by the
program itself.

### 5.1 The rules

Registration (`RegisterAndLock`) succeeds only if **all** of the
following hold, each checked on-chain by the program:

1. The mint's **mint authority is revoked** — no new supply can ever
   be created.
2. The mint's **freeze authority is revoked** — holders cannot be
   frozen out of their own tokens.
3. The token is a classic SPL mint. Token-2022 mints are rejected
   (`UnsupportedTokenProgram`): their extension model reintroduces
   exactly the authority surface this standard exists to remove.
4. The creator locks **at least 10% of total supply** (1,000 bps) in
   the program's vault PDA.
5. The lock runs **at least 30 days** — 6,480,000 slots, measured in
   slots rather than wall-clock time because slot height is the
   chain's own clock and cannot be skewed by a validator's timestamp.
   The maximum lock is 5 years.

After registration:

- **Locks extend, never shorten.** The only lock mutation is
  `ExtendLock` to a later unlock slot. There is no instruction that
  releases early, and no admin key that could.
- **Release is creator-only and time-gated.** After the unlock slot,
  the recorded creator — and only the recorded creator — can release,
  and the tokens return only to the creator.
- **There is no admin.** No pause, no override, no upgrade authority
  held back by the team. The standard's credibility comes from the
  absence of an escape hatch.

### 5.2 Creator reputation

Every registration creates or updates a **creator reputation account**
(PDA, one per creator) recording launches registered, locks completed,
and the slots of the first registration and last completion. It has no
decrement path and no edit path: reputation can only be earned by
registering launches and completing locks, in public, forever. A
creator's history is a queryable on-chain fact, not a social claim.

### 5.3 Evidence, not assertions

LaunchGuard is covered by 23 automated tests, including a named
adversarial suite that attacks it the way a rugger would: minting
after revocation, releasing early, shortening a lock, releasing as a
stranger, draining the vault directly, forging a reputation account,
double-registering a mint. Every attack fails.

On 2026-11-16 the standard was exercised end to end in a public demo
launch (localnet): a 1,000,000,000-supply demo token
(`AjTP23T2HhMryZVDPpzVYrTsWHgH6jKkMt9b8cLdn7hc`) was created, both
authorities revoked, and 100,000,000 tokens (exactly 10%) locked until
slot 6,480,003. The four in-demo rug attempts failed with the
program's own errors (`StillLocked`, `LockPeriodTooShort`,
`NotCreator`, and the SPL `FixedSupply`). The full record — every
account a reviewer must inspect — is in `programs/DEMO-LAUNCH.md`,
and a video of the run is linked from the repository README.

### 5.4 Why native matters

Because LaunchGuard is the chain's own program, its registry is the
single canonical answer to "is this launch protected?" — one set of
PDAs, one reputation history per creator, no competing badges. Wallets
and tools can consume the registry mechanically; a machine-readable
export of it is on the project's public backlog.

---

## 6. The bridge

The bridge moves approved SPL tokens between Solana and Machine:
lock on one side, mint a 1:1 wrapped representation on the other;
burn to redeem. $MACHINE itself never crosses (Section 4.1). The
bridge is implemented as two native Rust programs (no Anchor) plus an
off-chain relayer set, and it is **not deployed**: it runs and is
tested on localnet only, against Solana devnet in the Phase 1 plan,
and it does not touch real assets until the deployment gate in
Section 8 is satisfied.

### 6.1 Attestation: quorum, verified on-chain

A crossing begins when a user deposits (locks) or burns. The source
program writes an **EventRecord PDA** — the canonical on-chain record
of the event, written by the program itself after the transfer, never
asserted by a user or a relayer. Relayers observe the finalized event,
independently verify the source transfer on-chain (6.5), and sign a
canonical 195-byte `BridgeMessage`; the message hash is
SHA-256 over a domain separator (`MACHINE-BRIDGE-V1`) and the
canonical encoding.

Release on the destination requires **3-of-5 relayer signatures**,
verified *inside the destination program* through Solana's ed25519
precompile: the transaction carries the precompile instruction, and
the program parses the instructions sysvar itself — with its own
bounds-checked parser, after fuzzing showed the upstream parsing
helpers could panic on malformed sysvar data — and confirms that at
least three distinct configured relayers signed this exact message.
A signature over anything else is worthless; a record from any other
source does not exist as far as the program is concerned.

### 6.2 Replay protection

Every direction of every path is guarded by a **nonce PDA**: a
crossing's nonce can be processed exactly once, enforced by a shared
check in the common library used by all four entry points (deposit,
burn, mint, unlock). Re-submitting a valid, fully signed crossing a
second time fails.

### 6.3 Caps: two layers

- **Absolute caps** on both programs: a maximum per transaction and a
  maximum per day, set at initialization.
- **Dynamic caps** on the Solana unlock path, where real value would
  leave the vault: at most **1% of the live locked total** per
  transaction and **5% per 216,000-slot window** — so even a fully
  compromised quorum cannot drain the vault quickly; the loss rate is
  bounded by arithmetic while humans respond.

### 6.4 Pause, kill switch, and automatic tightening

- **Pause** can be triggered by 2-of-5 relayers or 2-of-3 admins —
  deliberately easy to pull in an emergency.
- **Unpause and the kill switch** require the 2-of-3 admin multisig.
- The **kill switch is one-way and asymmetric**: it permanently stops
  minting wrapped assets, while burns — the user's exit back to the
  source chain — remain open. An incident can stop the bridge; it
  cannot trap the users.
- **Unpausing tightens the caps automatically.** For 216,000 slots
  after any unpause, the dynamic caps drop to quarter strength
  (0.25% per transaction / 1.25% per window). Resuming service after
  an incident is therefore cautious by construction, not by someone
  remembering to be careful.

### 6.5 Relayers that verify, never trust

The Coreum and Cysic failures (Section 1.2) shared one root cause:
signers trusted a record instead of the chain. Machine's relayer is
built against that. Before attesting, a relayer verifies the source
transaction itself — the actual delivery of the configured asset to
the configured vault — and refuses to sign anything it cannot
independently confirm. The off-chain reconciliation checker that
watches for divergence between the two chains holds **no chain key at
all**: it can alert, but it cannot act. Humans and the multisig pause;
no hot key anywhere in the system can move funds or change state.

Relayer rotation exists (`SetRelayers` in both programs, gated by the
2-of-3 admin multisig, rejecting default and duplicate keys). The
admin set itself is fixed at initialization; rotating it means
re-initializing, by design.

### 6.6 Receipts: every crossing leaves evidence

Each mint and unlock writes a **permanent receipt account** in the
same transaction: direction, nonce, the signed message hash, amount,
recipient, slot, and timestamp. A crossing on Machine is therefore not
a claim in a relayer's log — it is a durable on-chain record that any
third party can enumerate and audit. (One precision: a program cannot
observe its own transaction signature, so the receipt does not store
the destination transaction ID; indexers bind the two by resolving the
receipt account's creating transaction. This is documented in
`BRIDGE.md` §4.5 rather than papered over.)

### 6.7 Drills

The bridge has been exercised in a full local end-to-end drill —
deposits, mints, burns, unlocks, a pause mid-flow, and a relayer
disagreement — and in a 45-crossing volume drill whose final state
reconciles exactly (vault balance equals wrapped supply to the unit).
The drill reports live in the repository (`bridge/drills/`).

---

## 7. Security posture

### 7.1 Testing

All figures from the latest full verification (`make verify`, run
2026-11-17 against commit `7cd6f5b`; re-runnable by anyone):

| Suite | Tests | Covers |
|---|---|---|
| Bridge workspace | 127 passing | bridge-common (37), machine-bridge (27), solana-lock (21), precompile-cost (1), relayer (41) |
| LaunchGuard | 29 passing | 16 integration, 12 adversarial, 1 full demo launch |
| Registry export | 11 passing | 9 unit, 2 program-test integration |
| Fork suite | 23 checks OK | genesis, patches, chain bring-up |
| **Total** | **190** | |

Beyond unit and integration tests: cargo-fuzz harnesses run against
the message and sysvar parsers and the crossing-receipt
serialization (fuzzing found a real panic in
upstream parsing helpers, which is why the sysvar parser is in-crate
and bounds-checked); `cargo audit` findings (10 advisories, all in
transitive dependencies) are recorded with dispositions in
`AUDIT.md`; clippy is clean of errors on both workspaces.

### 7.2 Published security documents

`THREAT-MODEL.md` (attack surface and mitigations), `LIMITATIONS.md`
(everything known to be unproven), `SECURITY.md` (disclosure policy
and a scoped, currently unfunded bug bounty), `SECURITY-CHANGELOG.md`,
and `AUDIT.md` (scope, self-review results, and the audit programme
below) are all in the repository.

### 7.3 The audit programme

Machine's bridge and LaunchGuard have **not** been independently
audited, and this paper does not soften that. The path to audit is
funded publicly and is already in motion:

- Audit inquiries sent to Accretion and Sec3; a scoped quote
  requested from Oak Security (both scope options, with and without
  the relayer) — replies pending.
- **Areta Audit Subsidy** application submitted (December 2026
  cohort) toward the audit cost.
- **Solana Foundation grant** application submitted: $30,000,
  milestone-based — $5,000 for the published security self-review and
  public testnet, $25,000 for the completed independent audit with
  the report published in the repository.

Auditors quote against the current `main`; every change that alters
the program scope since the inquiries is flagged in `AUDIT.md`.

---

## 8. Staged launch and the deployment gate

Machine launches in stages (`STAGED-LAUNCH.md`), and the bridge has a
hard deployment gate (`BRIDGE.md` §5) that binds at every stage.
Before any real asset crosses, **all** of the following must hold:

- an independent audit of the bridge and LaunchGuard, with the report
  published;
- fresh production keys, generated on the deployment host (never on a
  development machine, never committed);
- SBF binaries built from the audited source and deployed;
- initialization with the production relayer and admin sets and the
  absolute caps set deliberately;
- worthless-amount test crossings in both directions, plus pause and
  outage drills, executed and written up.

| Phase | What | Status |
|---|---|---|
| 0 — Public proof | Open repo, tests, drills, docs, this paper | **Done** ($0) |
| 1 — Public testnet | 3 validators on one host, public RPC, faucet, bridge vs Solana devnet, LaunchGuard live | Package ready (`testnet/`); awaiting a host (~$20/mo) |
| 2 — Livenet, no bridged value | The chain live with $MACHINE only; bridge programs deployed but moving nothing of value | Gated on Phase 1 evidence |
| 3 — Audited bridge deployment | Real assets cross, under the §5 gate | Gated on the audit + grant funding |
| 4 — Funded bug bounty | The scoped bounty in `SECURITY.md` goes live with rewards | Gated on funding |

## 9. Governance

Machine has no on-chain governance program today, and this paper does
not pretend otherwise. The genesis, fee schedule, and distributions
are fixed in the repository and published; changing them requires a
new genesis or a network upgrade — both public events. The bridge's
admin powers are a 2-of-3 multisig limited to pause, unpause, kill,
and relayer rotation; LaunchGuard has no admin at all. No governance
token is planned. The direction — validator-set governance as the
network becomes multi-validator, with changes proposed in the open —
will be claimed only when a mechanism exists and has been used.

## 10. Honest limits

- **Unaudited.** No independent audit has been completed. Quotes and
  grant applications are in flight; nothing here should be read as
  audited safety.
- **One node.** Machine today is a single-node localnet. Multi-
  validator consensus is packaged, not proven.
- **No transfer proof yet.** Signed transfers between fresh keypairs
  await a host with working UDP and a raised file limit.
- **The bridge has never moved value.** It is tested against
  localnet and drills only. No asset has crossed, and none will be
  claimed to have crossed until one does, under the gate.
- **The patched validator source is unbuilt.** The running chain uses
  stock Agave 2.3.13 binaries; the three fork patches are documented
  and unexercised.
- **Vesting is a commitment, not a lockup** (Section 4.4).
- **Small numbers, honestly framed.** Solana runs ~1,000+ validators
  and years of production load; Machine runs one local node and 190
  tests. Machine's claim is narrower and, we believe, more durable:
  a launch standard that exists nowhere else, a bridge designed
  against the industry's own post-mortems, and a public record that
  lets anyone check both.

## 11. Conclusion

Most forks change a name, a logo, and a supply. Machine changed what a
chain can promise a token launch: authorities gone, insider supply
locked by the chain itself, a creator reputation that cannot be
edited, and a bridge whose every crossing leaves evidence and whose
worst day is bounded by arithmetic. The work is public, the tests are
green, the limits are published, and the audit gate stands between
this paper and anyone's money. That is the whole pitch — verifiable,
or it doesn't ship.

---

## Appendix A — Key facts for verification

| Item | Value |
|---|---|
| Genesis hash | `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb` |
| Base software | Agave 2.3.13 (stock binaries; patches documented, unbuilt) |
| Supply | 1,000,000,000 $MACHINE, fixed |
| Bridge quorum | 3-of-5 relayers, ed25519 precompile verified in-program |
| Dynamic caps (unlock) | 1% per tx / 5% per 216,000 slots of live locked total; quarter strength for 216,000 slots after any unpause |
| LaunchGuard minimums | Authorities revoked; ≥10% of supply locked ≥6,480,000 slots (30 days) |
| Demo launch mint | `AjTP23T2HhMryZVDPpzVYrTsWHgH6jKkMt9b8cLdn7hc` (localnet, worthless) |
| Tests | 190 passing (127 bridge workspace, 29 LaunchGuard, 11 registry export, 23 fork checks) |
| Licence | Apache-2.0 |

## Appendix B — Document map

- `STATUS.md` — proven vs next, with the commands behind each claim
- `BRIDGE.md` — bridge design, §5 deployment gate
- `programs/LAUNCH-GUARD.md` — the LaunchGuard specification
- `programs/DEMO-LAUNCH.md` — the 2026-11-16 demo launch record
- `THREAT-MODEL.md`, `LIMITATIONS.md`, `AUDIT.md`, `SECURITY.md`
- `STAGED-LAUNCH.md` — the phase plan and its costs
- `GENESIS.md`, `FORK.md` — genesis values and the fork patches

*Machine is built in public, one verified commit at a time:
https://github.com/yourmememachine/mememachine*
