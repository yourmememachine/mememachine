# Receipt Indexer

**Status: off-chain tooling, built and tested locally (crate
`bridge/crates/receipt-indexer/`, binary `receipt-index`). The
bridge itself is NOT deployed and NOT audited — see BRIDGE.md
section 5. Nothing has crossed a live bridge; the crossing indexed
in the tests comes from the local program-test drill harness.**

Every completed crossing writes a **crossing receipt** on the
destination chain (BRIDGE.md 4.5): a PDA derived from (direction,
nonce) holding the attested message's hash, the source transaction,
the amount, the recipient, and the destination slot and timestamp.
One thing the receipt deliberately does not hold is the destination
transaction's own signature — a program cannot observe its own
transaction — so the design leaves that last binding to indexers.
This tool is that indexer: it resolves a receipt PDA to the
transaction that created it and prints the full crossing record.

That binding is not bookkeeping trivia. The Ankr bridge loss
(SlowMist, Nov 2026, ~$304,500) was five forged receipts recycled
from **one** already-redeemed deposit, each altered just enough to
produce a distinct proof hash and slip past a duplicate guard. A
receipt that resolves to exactly one creating transaction — in
exactly the slot the receipt itself records — is how that shape is
detected after the fact instead of argued about.

## How to run it

```sh
cd bridge && cargo build --release -p receipt-indexer

# Live: read the destination chain's RPC (finalized commitment).
./target/release/receipt-index \
  --rpc http://127.0.0.1:8899 \
  --program <DEST_PROGRAM_ID> \
  --direction solana-to-machine --nonce 7 [--json]

# Offline / drill mode: the identical binding over saved RPC
# output — a getAccountInfo result, a getSignaturesForAddress
# result array, and a getTransaction result, as JSON files.
./target/release/receipt-index offline \
  --program <DEST_PROGRAM_ID> \
  --direction solana-to-machine --nonce 7 \
  --account account.json --signatures sigs.json --transaction tx.json [--json]
```

Exit 0 prints the record; exit 1 prints `NOT BOUND: <reason>`,
naming the exact check that failed.

## What the record contains

| Field | What it is |
|---|---|
| `receipt_pda` | The receipt address — **derived** from (program, direction, nonce) by the tool, never supplied by the caller. |
| `receipt` | The receipt's own fields: message hash (hex), source transaction (base58), amount, recipient, destination slot and timestamp. |
| `destination_tx` | The signature this tool resolved: the transaction that created the receipt. |
| `destination_slot` | That transaction's slot — verified equal to the slot recorded in the receipt. |
| `signatures_for_receipt` / `successful_signatures` | How many transactions the RPC lists for the receipt PDA, and how many succeeded. |
| `extra_successful_signatures` | Anomaly flag: see the honest limits below. |

## The checks, in order

1. **Strict receipt parse.** The account at the derived PDA must
   be owned by the destination program, be exactly
   `CrossingReceipt::LEN` bytes, and carry the receipt
   discriminator — the registry exporter's discipline, applied to
   receipts. Foreign or malformed accounts are rejected, never
   misread.
2. **Self-derivation.** The receipt's own (direction, nonce)
   fields must re-derive the very address it was read from, and
   must match the requested crossing. A receipt for nonce 8
   sitting at nonce 7's address is not a receipt for nonce 7.
3. **Creation resolution.** From the PDA's signature list, the
   creating transaction is the **oldest successful** entry. Failed
   transactions create nothing, so a failed replay attempt against
   the same nonce — however recent — is skipped, never mistaken
   for the crossing. (The end-to-end test includes exactly this: a
   real on-chain replay that failed, listed ahead of the real
   creation.)
4. **Transaction verification.** The resolved transaction must
   have succeeded (`meta.err` null), must sit in **exactly the
   slot the receipt records** (the program wrote that slot from
   the Clock sysvar as it created the receipt), its account list
   must contain the receipt PDA, and it must carry a top-level
   instruction of the destination program whose accounts include
   the receipt PDA — the MintWrapped / Unlock that wrote it. The
   signature list's own slot for the creation entry must agree
   too: three independent RPC views pinned to one slot.

## Honest limits

- **It binds, it does not judge.** A bound record proves this
  receipt was written by this transaction in this slot. Whether
  the crossing was *valid* — quorum, hash, caps — is the proof
  bundle's job (`crossing-proof`, bridge/PROOF-BUNDLE.md); the two
  tools compose: the bundle proves the rules, the indexer proves
  where the receipt came from.
- **Extra successful signatures are flagged, not hidden.** A
  receipt PDA is written exactly once (replay fails on-chain), so
  a second *successful* transaction touching the address is an
  anomaly a human should look at. The record reports it
  (`extra_successful_signatures: true`) and still binds to the
  oldest successful transaction — the creation.
- **It trusts the RPC it is pointed at for the raw views** —
  account data, signature lists, transaction contents — and then
  cross-pins those views against each other and against the
  receipt's own on-chain fields. A fully malicious RPC can still
  fabricate a consistent set; run it against a node you trust, or
  two and compare. That is the same trust shape every indexer has;
  the cross-checks narrow what a merely *careless* or *stale* RPC
  can get wrong.
- **Local only.** The bridge is undeployed; the exercised crossing
  is a program-test drill. No live chain state is claimed.
