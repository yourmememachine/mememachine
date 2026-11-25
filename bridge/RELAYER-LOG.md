# Tamper-Evident Relayer Log

**Status: off-chain tooling, built and tested locally (module
`bridge/crates/relayer/src/audit_log.rs`, inspector binary
`relayer-log`). The bridge itself is NOT deployed and NOT audited —
see BRIDGE.md section 5. The relayer has never run live; the logs
exercised in tests are produced by the test suite and the local
drill harness.**

The attestation exchange proves *what was signed*: one immutable
file per relayer per nonce, each carrying that relayer's signature
over the message hash. It does not prove *the order in which a
relayer acted* — that it signed nonce 7 before collecting nonce 8,
that a collection failed with a disagreement, or that a submission
happened at all. After an incident, that sequence is the evidence:
in the Kelp DAO drain (April 2026, ~$292M) every on-chain
transaction was technically correct and the falsified step was in
the relayers' view of the source chain; in the Alephium exploit
(May 2026) legitimate guardians signed unauthorized messages after
being forced onto a weaker observation path. Reconstructing either
from signature files alone means trusting the operator's own
account of events. This log makes that account tamper-evident.

## What the log is

One JSON Lines file per relayer (default:
`<attestation_dir>/relayer-audit.jsonl`, overridable with the
config's `audit_log_path`). The relayer appends one entry for each
consequential action:

| Event | When it is written |
|---|---|
| `signed` | The relayer signed a message and wrote its attestation file (before the nonce cursor advances, so a log failure leaves the nonce current and the retry re-signs the identical message). |
| `collected` | A quorum was collected and re-verified with bridge-common. |
| `collection_failed` | Collection failed for a non-routine reason — disagreement, a malformed or forged exchange file, an unknown relayer. Quorum-not-reached is routine in the poll loop and is deliberately not logged. |
| `submitted` | A quorum attestation was submitted to the destination chain (records the transaction signature). |
| `dry_run` | A submission was prepared but not sent (`submit_enabled = false`), so a dry run leaves the same trail shape as a live one, minus the transaction. |

Each entry carries a sequence number, the previous entry's hash,
and its own hash:

```text
entry_hash = sha256("MACHINE-RELAYER-LOG-V1"
                    || prev_hash (32 bytes)
                    || seq (u64, little-endian)
                    || canonical JSON of the event)
```

Editing, deleting, or reordering any entry breaks every hash
after it, and verification names the first broken entry. Opening a
log whose contents do not verify is an error, and the relayer
fails closed: it will not sign, collect, or submit with a broken
log. A human investigates first; the log is never silently reset.

## How to verify one

```sh
cd bridge && cargo build --release -p relayer
./target/release/relayer-log verify relayer-audit.jsonl
# ok: N entries, head <seq>:<hash>   (exit 0)
# relayer-log: audit log error: …    (exit 1, names the broken entry)

./target/release/relayer-log head relayer-audit.jsonl
# <seq>:<hash>  — the current checkpoint
```

## The honest limit: a chain alone is not evidence

A hash chain proves **internal consistency only**. Anyone with
write access to the file can recompute a fully forged chain over
different events, and plain `verify` accepts it — the test suite
pins exactly this: a wholesale rewrite passes `verify_chain` and
is caught only by the checkpoint check below. This is the
standard result for hash-chained logs, not a defect in this one.

The chain becomes evidence once its **head leaves the writer's
control**. Publish the checkpoint (`relayer-log head`, a
`<seq>:<hash>` pair) somewhere the operator cannot rewrite it —
a release note, another relayer's log, an auditor's inbox, a
commit in this repo. From then on, anyone holding that value can
check any later copy of the log against it:

```sh
./target/release/relayer-log verify relayer-audit.jsonl \
    --checkpoint <seq>:<hash>
```

A log truncated or rolled back past the checkpoint, or rewritten
wholesale, fails this check even though it is internally
consistent. A log that simply *grew* past its checkpoint still
verifies — the checkpoint pins a prefix, not the future.

What the log does not do:

- It does not prove an entry's *contents* are true — a relayer
  can log a lie in a perfectly chained entry. It proves the lie
  was written at that point in the sequence and cannot be quietly
  revised later. Cross-checks come from the other four relayers'
  logs and the attestation files themselves.
- It has no timestamps of its own. Ordering is by sequence
  number; wall-clock claims would be the writer's assertion, so
  the format omits them rather than dressing one up as evidence.
  (Entries that matter carry on-chain anchors: a `submitted`
  entry's transaction signature has the destination chain's own
  slot and timestamp.)
- It is per-relayer, like the keys. There is no merged multi-
  relayer log; comparing five logs is an incident-time exercise.

## Tests

14 unit tests in `audit_log.rs`: chain construction and reopen
continuation; edited, deleted, reordered, and hash-forged entries
detected; truncation and wholesale rewrite detected *only* by the
checkpoint (both pinned as passing internal verification first);
a checkpoint prefix still verifying after further appends;
fail-closed open on a tampered file and on a corrupt line; the
empty log; and all five event kinds chaining together with a
file round-trip. The wire format was additionally cross-checked
against an independent Python implementation of the hash
construction, whose output the shipped `relayer-log` binary
verifies.
