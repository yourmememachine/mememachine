# Machine Bridge — Design Document

**Status: programs built and tested locally (see `bridge/` — bridge workspace `cargo test --release`: 108 passed, 0 failed, including the relayer's 39; LaunchGuard 23 passed in `programs/`); NOT deployed, NOT audited. Last verified commit: `266f6c5` (2026-11-16, `make verify` green). Nothing has been bridged, and no asset may cross until the prerequisites in §5 are met.**

- **Chains:** Solana mainnet ↔ Machine
- **Machine identity:** Solana fork (Agave 2.3.13), custom genesis `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`, native gas token **$MACHINE**, fixed supply 1,000,000,000, no inflation.
- **Machine today:** single-node localnet, voting and finalizing, ~3 slots/s. The 3-validator live net is the next step (see `livenet/` in this repo).
- **Use case:** approved SPL tokens on Solana mainnet are bridged to Machine as wrapped assets. **$MACHINE stays the gas token** — a bridged token is a separate asset and never replaces it.

---

## 1. Architecture: lock / mint

One asset, two representations. The original token never leaves Solana mainnet; Machine holds a wrapped representation backed 1:1 by the locked originals.

### Solana mainnet → Machine (lock / mint)

1. User sends the SPL token to the **lock vault** on Solana mainnet.
2. The lock program records the deposit and emits a signed event: amount, source mint, sender, destination Machine address, nonce.
3. Relayers observe the finalized deposit, attest to it, and submit the attestation to Machine.
4. The Machine bridge program verifies the quorum attestation and **mints** the wrapped token to the destination address.

### Machine → Solana mainnet (burn / unlock)

1. User sends the wrapped token to the **burn program** on Machine; the tokens are burned.
2. The burn program emits a signed event: amount, destination Solana address, nonce.
3. Relayers observe the finalized burn, attest, and submit to Solana mainnet.
4. The Solana lock program verifies the quorum attestation and **unlocks** the original tokens to the destination address.

### Who holds the lock

- The lock vault is a program-owned account (PDA) on Solana mainnet. No individual holds the keys.
- The vault's authority is the **lock program itself**, which only releases funds on a verified quorum attestation of a burn on Machine.
- Program upgrades are behind a **multisig timelock** (see §4). There is no single admin key.
- The wrapped token's mint authority on Machine is held by the **Machine bridge program PDA**. Minting is only possible through the program's attestation path — there is no direct mint instruction callable by any key.

### What is NOT in this design

- No liquidity-pool bridge (no AMM, no swapping, no price exposure). This is a 1:1 custody bridge only.
- No wrapping of $MACHINE itself. $MACHINE is native to Machine and is never bridged; only approved SPL assets cross.
- No "validator set controls the vault" in phase 1. Custody is program + multisig; validator-set attestation is a later phase (see §6).

---

## 2. Program design

Two programs, one per chain, plus a shared message format.

### 2.1 Solana side — lock program

Instructions:

| Instruction | Effect |
|---|---|
| `deposit(mint, amount, destination)` | Transfers SPL tokens from the user into the vault PDA. Records the deposit in a per-nonce account. Emits `Deposit` event. |
| `unlock(attestation)` | Verifies a quorum attestation of a burn on Machine, checks the nonce has not been processed, releases the locked tokens to the destination address, marks the nonce processed. |
| `pause()` / `unpause()` | Multisig-only. Halts deposits and unlocks. |
| `set_cap(...)` | Multisig-only, timelocked. Adjusts per-transaction and daily limits. |

State: one `ProcessedNonce` PDA per (direction, nonce). A nonce can never be processed twice.

### 2.2 Machine side — bridge program

Instructions:

| Instruction | Effect |
|---|---|
| `mint_wrapped(attestation)` | Verifies a quorum attestation of a lock on Solana, checks the nonce, mints the wrapped SPL token to the destination address, marks the nonce processed. |
| `burn(amount, destination)` | Burns the wrapped token from the user. Records the burn in a per-nonce account. Emits `Burn` event. |
| `pause()` / `unpause()` | Multisig-only. Halts mints and burns. |
| `set_cap(...)` | Multisig-only, timelocked. |

The wrapped token is a standard SPL mint created by the bridge program at deployment. Its mint authority is the program PDA; its freeze authority is the program PDA (used only by the pause path, never to freeze individual users).

### 2.3 Message format

Every cross-chain event is a canonical, versioned message signed by relayers:

```
BridgeMessage {
  version:        u8,          // 1
  source_chain:   u8,          // 1 = Solana mainnet, 2 = Machine
  dest_chain:     u8,
  nonce:          u64,         // strictly increasing per direction
  source_mint:    Pubkey,      // original SPL mint on Solana
  sender:         Pubkey,
  recipient:      Pubkey,      // address on the destination chain
  amount:         u64,         // raw base units, no decimals conversion
  source_tx:      [u8; 64],    // source-chain transaction signature
  source_slot:    u64,         // slot of the source transaction
  expiry_slot:    u64,         // attestation valid only until this slot
}
```

The message hash is `sha256(domain_separator || canonical_encoding)`. Relayers sign the hash, never the raw fields.

**On-chain verification (as implemented):** the submitting transaction carries an
ed25519-program (precompile) instruction holding the quorum signatures over the
message hash; the runtime verifies those signatures before any instruction
executes. The bridge programs read the attestation back from the instructions
sysvar and require at least 3 *distinct registered* relayers on exactly this
hash — the Solana-standard verification path, with no in-program signature
cryptography.

### 2.4 Replay protection

- **Nonce registry:** each direction maintains a strictly increasing nonce counter and a set of processed nonces (PDA per nonce). `unlock` / `mint_wrapped` reject any nonce already processed.
- **Chain binding:** `source_chain` and `dest_chain` are part of the signed hash, so an attestation for one direction cannot be replayed in the other.
- **Expiry:** attestations carry an `expiry_slot`. Stale attestations (older than the expiry) are rejected, so a captured attestation cannot be replayed later.
- **Source-tx binding:** the source transaction signature is in the message, so one deposit cannot fund two mints even if a relayer double-signs.

### 2.5 Finality requirements before minting / unlocking

- **Solana → Machine:** the deposit transaction must be **finalized on Solana mainnet** (confirmed at 32 slots, ~13 seconds, plus a safety margin of 16 slots) before any relayer signs. Never attest to a merely confirmed or processed transaction.
- **Machine → Solana:** the burn transaction must be **finalized on Machine** (rooted slot) before any relayer signs. On the current single-node localnet this is fast, but the rule is the same: only rooted slots count.
- A reorg on either side after finality is the design's hard assumption — if a chain can reorg past finality, the bridge is unsafe by construction and must be paused.

---

## 3. Relayer / validator set

Relayers are off-chain processes that watch one chain, verify finality, sign attestations, and submit them to the other chain. They hold no funds and cannot move assets by themselves — they can only sign messages, and a quorum is required.

### 3.1 Composition (phase 1)

- **5 relayers, 3-of-5 quorum.** One relayer run by each of the 3 livenet validators, plus 2 independent operators not running validators.
- Each relayer has its own keypair. Keys are generated fresh for production — never the dev keys in this repo, which are throwaway test keys.
- Quorum is **3 distinct valid signatures** over the same message hash. The on-chain programs verify the signatures against the registered relayer set and enforce the threshold.

### 3.2 Liveness

- If relayers go down: bridging **stalls, it does not lose funds**. Deposits sit in the vault; burns sit burned; nothing moves until a quorum returns. This is the correct failure mode — liveness can wait, safety cannot.
- If a relayer is slow or offline, the remaining relayers still reach quorum (3-of-5 tolerates 2 offline).
- If relayers **disagree** (different message hashes for the same nonce): no quorum forms, nothing executes. Disagreement is logged and alarmed; a human investigates before any manual intervention. There is no tie-break by a single party.

### 3.3 Later phases

- **Relayer transport (production requirement):** phase 1 exchanges signed attestation files through a shared directory — acceptable for local work and testnet drills, where the collector re-verifies every signature and any disagreement blocks quorum. Before any real value moves, the transport must become an authenticated channel (or the file exchange must gain equivalent authentication, validation, and monitoring). This is Phase-2 production work and part of the §5 gate in spirit: an unauthenticated transport is not a production transport.
- **Phase 3+:** move attestation toward the Machine validator set itself — validators sign attestations as part of their duties, with the independent relayers kept as a second quorum. This removes the separate relayer trust assumption but only after the validator set is larger than 3 and stake is meaningfully distributed.
- **Slashing / removal:** a relayer that signs two conflicting messages for the same nonce is provably faulty and is removed from the set by multisig. Bonded stake for relayers is a later-phase addition, not a phase-1 requirement.

---

## 4. Security model and failure modes

### 4.1 What an attacker would need

| Attack | What it requires |
|---|---|
| Steal locked funds | Quorum of relayer keys (3 of 5) **and** a forged burn attestation, or a program exploit. One relayer key is useless. |
| Infinite mint on Machine | Quorum of relayer keys **and** a forged lock attestation, or the mint authority. The mint authority is the program PDA — there is no key to steal. |
| Replay an old crossing | A processed nonce, an expired attestation, or a cross-direction replay — all rejected by §2.4. |
| Drain via fake finality | Convincing relayers that an unfinalized transaction is final. Mitigated by the finalized-only rule (§2.5) and by relayers independently querying their own RPC nodes. |
| Upgrade the program maliciously | Control of the upgrade multisig **and** the timelock. See below. |

### 4.2 Lessons applied (general bridge-failure patterns)

- **Never mint on unconfirmed transactions.** The majority of bridge losses trace to trusting a transaction that was not final. §2.5 is a hard rule, not a guideline.
- **No infinite mint authority in any human hand.** Minting is only reachable through the program's attestation path. There is no admin mint instruction.
- **Cap the blast radius.** Per-transaction caps and daily caps (§4.3) bound the loss from any single failure, including a quorum compromise.
- **No upgrade without delay.** Program upgrades go through a multisig with a timelock (minimum 48 hours), so a malicious or buggy upgrade is visible before it activates, and users can exit.
- **Separate the keys.** Relayer keys, multisig keys, and validator keys are distinct keypairs held by different parties. Compromising one role compromises nothing else.

### 4.3 Pause / kill-switch

- **`pause()`** on either program halts all deposits, mints, burns, and unlocks. Callable by the multisig **or** by any 2 of 5 relayers (a low threshold for stopping, a high threshold for moving funds).
- Pause is **not** reversible by relayers — only the multisig can unpause, after the cause is understood.
- **Kill-switch (phase 2+):** a separate `freeze_mint` instruction that permanently disables new minting while leaving unlocks open, so users can always exit back to Solana. This is a one-way safety valve, used only if minting itself is compromised.

### 4.4 Rate limits and caps

Caps are enforced on-chain in the lock and bridge programs, not in the relayer — a compromised relayer cannot bypass them. Two layers, both must pass:

- **Absolute caps** (both programs): per-transaction and daily caps set as exact amounts at initialization (and by multisig `set_cap`). The daily window is **slot-based** (216,000 slots ≈ 24h at the 400ms target), not wall-clock — slot counts cannot be skewed the way validator timestamps can.
- **Dynamic percentage caps** (Solana-side unlocks): each unlock may move at most **1% of the vault's locked total**, and each window's unlocks at most **5%** — the phase-3 launch values above, enforced dynamically against the live locked total rather than translated to absolutes once. The mint side uses the absolute layer only: the wrapped supply is created by the mints themselves, so a percentage-of-supply rule there has no independent reference (any first mint would be 100% of the supply it creates). The Machine program does track total wrapped supply outstanding on-chain, so either side can be reconciled against the other at any slot.
- **Vault ceiling:** the vault refuses deposits that would push total locked value above a configured ceiling until the ceiling is raised by multisig + timelock.
- **Automatic tightening after an incident:** every unpause of the lock program starts a recovery window of one full cap window (216,000 slots) during which the dynamic caps drop to a quarter of normal — **0.25% per transaction, 1.25% per window** — and then relax on their own; the deadline is a slot number in the config, so the relaxation needs no transaction and cannot be forgotten or skipped. The reasoning: anything serious enough to pause the bridge (including a reconciliation mismatch, which the incident-response runbook answers with a pause) should not be followed by a full-speed reopening. Tightening is deliberately not triggerable by the off-chain reconciliation checker itself — the checker has no chain key; it alerts, a human/multisig pauses, and the on-chain tightening follows automatically from the unpause.

### 4.5 Crossing receipts

Every successful mint and unlock writes one more account in the same transaction: a **crossing receipt**, a PDA derived from (direction, nonce) holding the message hash, the source-chain transaction id carried by the message, the amount, the recipient, and the destination slot and timestamp. Anyone can derive the address for any crossing and check whether it exists and what it says — a public, permanent proof that a specific crossing completed, readable without trusting the relayers or any website. One honest limit: a program cannot observe its own transaction's signature, so the receipt does not store the destination transaction id; indexers bind that off-chain by resolving the receipt PDA's creating transaction. Everything the program itself can prove is on-chain.

---

## 5. What must exist before any real asset crosses

None of these is optional. If any item is missing, the bridge stays off.

1. **Live 3-validator net.** The livenet package (`livenet/`) deployed, 3 validators in consensus, and a confirmed user transfer on that net. The single-node localnet is not a bridge endpoint.
2. **Public RPC.** Both chains reachable from public RPC endpoints the relayers and users can query independently.
3. **User transfers verified.** A transfer between two fresh keypairs on Machine, with a signature, on the live net — the proof that the chain accepts user transactions at all.
4. **Fresh production keys.** New validator, relayer, and multisig keypairs generated on the deployment hosts. The dev keys in this repo are never used for anything real.
5. **Security review / audit.** An independent review of both programs and the relayer, with findings fixed and the report published. A bridge without an audit does not hold other people's funds.
6. **Test transfers with worthless amounts.** On the live net, bridge a token with no value first (a test mint), in both directions, including a pause/unpause drill and a relayer-outage drill. Only then consider a real asset.
7. **Monitoring and incident response.** Vault balances watched continuously, a reconciliation job comparing locked vs. minted supply every minute, and a named human who can pause the bridge at any hour.

---

## 6. Phased rollout plan

| Phase | What happens | Gate to proceed |
|---|---|---|
| **0 — Design** | This document. Message format, program interfaces, relayer set, caps, and the pause design frozen and reviewed internally. | Design reviewed by at least two people who did not write it. |
| **1 — Testnet bridge** | Programs deployed on a Machine testnet and Solana devnet. Worthless test assets cross in both directions. Relayer-outage and pause drills run. | 100 test crossings, 0 lost or duplicated tokens, both drills passed. |
| **2 — Audit** | Independent security review of both programs, the relayer, and the key management. All critical and high findings fixed; mediums fixed or accepted in writing. | Audit report published. |
| **3 — Capped mainnet** | Bridge goes live with one approved SPL asset only, per-transaction and daily caps on, vault ceiling low, allowlist of recipient addresses optional. Monitoring and the pause drill are live. | 30 days of capped operation with 0 incidents and reconciliation matching every minute. |
| **4 — Full** | Caps raised in steps, ceiling raised, additional approved SPL assets added one at a time, relayer set expanded, validator-set attestation evaluated. | Each cap raise is a separate multisig + timelock decision, never automatic. |

---

## Honest limits

- **This document is the design; the code exists but no bridge is live.** The programs and the relayer are written and tested locally (see `bridge/`), but nothing is deployed, no relayer is running against a live chain, no asset has crossed, and nothing here should be read as a claim that bridging works live.
- **A bridge is the highest-risk component in this project.** It holds other people's funds by design. Every shortcut in §5 converts directly into loss.
- **The 3-of-5 relayer set in phase 1 is a trust assumption.** It is weaker than full validator-set attestation and is chosen only to get a safe, capped launch. It is not presented as decentralized.
- **Machine is a single-node localnet today.** Until the 3-validator live net exists and user transfers are verified on it, the Machine side of this design has no real endpoint to connect to.
- **Dust and state growth.** Zero-amount crossings are rejected, and every nonce / EventRecord PDA is rent-paid by the account that creates it; on the mint and unlock paths, creation additionally requires a valid 3-of-5 attestation, so an attacker cannot force state growth on the value-extracting side without relayer cooperation. A minimum-transfer floor was considered and deliberately left as a §5 deployment-gate decision (taken with the auditor's input) rather than a hardcoded constant — see THREAT-MODEL.md.
- **The event log is on-chain state, not just logs.** Every crossing writes an EventRecord PDA (amount, destination, nonce, direction) that any external watcher can enumerate and reconcile against balances, and every state change (mint, burn, pause, kill, relayer rotation, cap change) also emits a program log line. History can be reconstructed from accounts alone, without trusting log retention.
