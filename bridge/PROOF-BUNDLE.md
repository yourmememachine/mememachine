# Crossing Proof Bundles

**Status: off-chain tooling, built and tested locally (crate
`bridge/crates/proof-bundle/`, binary `crossing-proof`). The bridge
itself is NOT deployed and NOT audited — see BRIDGE.md section 5.
Nothing has crossed a live bridge; the bundles exercised in tests
come from the local program-test drill harness.**

Bridges normally publish transaction links and ask you to trust an
explorer, an indexer, and the bridge's own website. A crossing
proof bundle is the opposite shape: **one self-contained JSON file
per crossing that anyone can re-verify offline**, with nothing but
this verifier and the file — no RPC, no explorer, no trust in
whoever handed over the bundle.

## What a bundle contains

| Field | What it is |
|---|---|
| `message` | The canonical `BridgeMessage` for the crossing (BRIDGE.md 2.3), in the same wire form the relayer attestation exchange uses. |
| `message_hash` | `sha256("MACHINE-BRIDGE-V2" \|\| canonical 203-byte encoding)`, hex. |
| `attestations` | The relayer signatures over that hash: who signed, and the signature bytes (base58). |
| `relayer_set` | The registered relayer set the signatures are checked against — all five pubkeys, in config order. |
| `quorum` | The quorum claimed. The verifier requires it to equal the protocol constant (3); see below. |
| `receipt` | The crossing receipt the destination program wrote in the same transaction that moved the value (BRIDGE.md 4.5): direction, nonce, message hash, source transaction, amount, recipient, destination slot and timestamp. |

## How to verify one

```sh
cd bridge && cargo build --release -p proof-bundle
./target/release/crossing-proof verify bundle.json
# VALID crossing proof ...   (exit 0)
# INVALID: <specific reason> (exit 1)
# Optionally pin the verdict to the registered relayer set:
./target/release/crossing-proof verify bundle.json --expected-relayers <PK1>,<PK2>,<PK3>,<PK4>,<PK5>
```

The verifier is fully offline and re-runs the bridge's own checks,
in this order:

1. **Format version** is one the verifier understands.
2. **Direction binding:** the bundle's direction label matches the
   message's source/destination chains.
3. **The rules are not weakened:** the relayer set has exactly five
   members, all valid, distinct, non-default pubkeys, and the
   claimed quorum equals the protocol constant `QUORUM` (3). A
   bundle that quietly claims a 1-of-5 quorum is rejected here —
   it would not be a proof under the bridge's rules.
4. **Receipt ↔ message agreement:** direction, nonce, amount,
   recipient, and source transaction in the receipt equal the
   attested message's fields, field by field; a mismatch names the
   field. The amount must also be non-zero — both programs reject
   zero-amount crossings on-chain (2026-11-18).
5. **Hash recomputation:** the stated message hash (in the bundle
   and in the receipt) equals the recomputed
   `sha256(domain separator || canonical encoding)`.
6. **Attestation verification:** bridge-common's own
   `verify_attestation` — the same code the relayer collector runs
   before submitting — checks every signature over the recomputed
   hash: signers must be registered members of the stated set,
   each counted at most once, at least a quorum of distinct valid
   signatures, and the message must not have been expired at the
   receipt's timestamp (the destination Clock time at which the
   crossing completed — the same timestamp basis the on-chain
   program used when it accepted the attestation). Note the
   limit: the receipt — including that timestamp — is
   exporter-supplied stated data (assumption A-4), so the expiry
   verdict is exactly as trustworthy as the receipt itself; the
   bundle proves the attestation was valid, not that the
   destination program ever saw it.

Every rejection exits non-zero with the specific reason: which
receipt field disagrees, that the hash does not recompute, that a
signer is not in the set, that the signatures are invalid over the
hash, that quorum was not reached, and so on.

## How to export one

Export pairs the two artifacts a completed crossing already
produces — the relayers' attestation files and the on-chain
receipt:

```sh
./target/release/crossing-proof export \
  --attestations ./exchange \          # relayer exchange directory
  --receipt receipt.json \             # receipt fields, as read from the receipt PDA
  --direction solana-to-machine --nonce 7 \
  --relayers <PK1>,<PK2>,<PK3>,<PK4>,<PK5> \
  --out bundle.json
```

The collector re-verifies the quorum from the exchange files with
bridge-common before anything is assembled, and the exporter runs
the full bundle verification on its own output before writing it:
**export never emits a bundle its own verifier would reject.**

## What a valid bundle proves — and what it does not

A valid bundle proves exactly this:

- a quorum (3-of-5) of the stated relayer set signed **this
  exact message** — its hash binds the amount, recipient, nonce,
  source transaction, both chains, and the expiry; and
- the bundle contains a **receipt whose fields match that
  message** — direction, nonce, amount, recipient, source
  transaction — at the stated destination slot.

It does **not** prove:

- that the bridge programs are correct or safe — that is the
  audit's job (BRIDGE.md section 5), and no audit has happened;
- that the stated relayer set is the *real* registered set — the
  bundle states the set it is checked against; confirming that set
  against the on-chain config account is a separate lookup (and
  is precisely why the set is stated in the open, not hidden).
  The verifier prints the set it used, and
  `verify --expected-relayers <PK,PK,PK,PK,PK>` pins the verdict
  to a set you supply (debate round 2026-11-18);
- that the destination program ever wrote the receipt — the
  receipt is carried as stated fields, not as an authenticated
  artifact (debate round 2026-11-18). Confirming a crossing
  on-chain — the receipt PDA in the destination program's
  account space — is a separate lookup, like the set check above;
- anything about a live crossing. The bridge is undeployed and
  runs only against local program-test banks today, so every
  bundle in existence is from a local drill with worthless test
  tokens. $MACHINE itself is never bridged (BRIDGE.md section 1),
  and bundles only ever cover wrapped-asset crossings.

## Tests

`cargo test --release -p proof-bundle` (bridge workspace):

- **Library (22 tests):** a valid 3-of-5 and 5-of-5 bundle verify;
  the JSON round-trip verifies; and the tamper matrix — amount
  flipped in the message, in the receipt, in both, and in both
  plus the hash fields (rejected in turn on receipt mismatch,
  hash mismatch, and signature validity); nonce changed; recipient
  changed; an attestation dropped below quorum; a signature from
  a non-relayer key; a duplicated attestation; a corrupted
  signature; a swapped relayer-set member; a weakened quorum
  claim; completion recorded after expiry; a wrong direction
  label; an unsupported bundle version. The 2026-11-18 debate
  round added: a non-ASCII message hash rejected (not a panic),
  a zero-amount bundle rejected, a stronger collection quorum
  (5-of-5 export) stamping the protocol quorum, and verification
  pinned to an expected relayer set.
- **End-to-end (1 test):** a real crossing through the existing
  drill harness (deposit on the lock program → attestation files
  → collector → `MintWrapped` → the receipt the program wrote) is
  exported and verified in-process and through the shipped
  `crossing-proof` binary; tampered copies are rejected by both.
