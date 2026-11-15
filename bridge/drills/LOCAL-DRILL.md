# Local end-to-end drill (no network, no deployment, no real assets)

The full crossing flow exercised locally against the real programs:
two `solana-program-test` banks stand in for the two chains, and the
relayer's actual phase-1 transport (signed attestation files in a
shared directory) and collector sit in the middle. This is a local
drill, not a live crossing — for the live testnet version see
`test-crossing.md`, `pause-drill.md`, and `relayer-outage.md` in this
directory.

## Run it

```
cd bridge
cargo test --release -p relayer --test full_drill -- --nocapture
```

## What it does

Setup: both programs initialized in their banks with the same
5-relayer set (3-of-5 quorum); user funded with 20,000,000 source
tokens on the Solana side.

- **Phase A — deposit → mint.** Deposit 5,000,000 (nonce 1). The
  drill reads the on-chain EventRecord PDA back, builds the bridge
  message from it, three relayers write attestation files, the
  collector reaches quorum, and the relayer's own submitter builds
  the [ed25519 precompile, MintWrapped] transaction. Result:
  recipient holds 5,000,000 wrapped; vault holds 5,000,000.
- **Phase B — pause mid-flow.** Deposit 2,000,000 (nonce 2), quorum
  collected. The Machine bridge is paused (2 relayer signers). The
  mint submission fails with `BridgePaused` and the wrapped balance
  does not move. Unpause (2 admin signers — relayers cannot unpause)
  and resubmit: mint succeeds, recipient at 7,000,000.
- **Phase C — burn → unlock.** Burn 50,000 wrapped (nonce 1; sized
  to stay within the dynamic unlock caps: 1% of the 7,000,000 locked
  total is 70,000). Burn EventRecord read back, attested, collected,
  unlocked on the Solana side. Result: user source balance
  13,050,000; vault 6,950,000 — exactly the wrapped supply still
  outstanding (7,000,000 minted − 50,000 burned).
- **Phase D — relayer disagreement.** Deposit 10,000 (nonce 3). Two
  relayers attest the true message; two attest a tampered message
  (amount + 1). The collector refuses to form a quorum over either
  hash; nothing mints. The deposit stays in the vault — stalled,
  not lost, which is the designed failure mode.

## Result (2026-11-14, latest commits)

```
drill A ok: deposit 5,000,000 -> minted 5,000,000 wrapped
drill B ok: pause blocked the mint (BridgePaused), unpause released it
drill C ok: burn 50,000 -> unlocked 50,000 (within the dynamic caps)
drill D ok: disagreement blocked quorum; nothing minted
test full_local_drill ... ok
```

Implementation: `bridge/crates/relayer/tests/full_drill.rs`. It
links both program crates into one test binary, which is why the
programs' `entrypoint!` is gated behind a `no-entrypoint` cargo
feature (the same pattern spl-token uses); the feature is off by
default and never enabled for SBF builds, so deployed binaries keep
their entrypoint.
