# Machine Bridge — On-Chain Programs

The first original on-chain code for the Machine chain: a lock/mint bridge
between Solana mainnet and Machine, implementing `../BRIDGE.md` exactly.
Two programs, one per chain, plus a shared crate.

**Status: built and tested locally. NOT deployed. NOT audited. No asset has
crossed. Nothing here holds real funds.**

## Layout

```
bridge/
├── Cargo.toml                  # workspace (resolver 2)
├── README.md                   # this file
├── VERIFY.md                   # real build/test output
└── crates/
    ├── bridge-common/          # message format, attestation verification,
    │                           # caps, authority checks, shared state
    ├── machine-bridge/         # Machine-side program (mint wrapped / burn)
    │   ├── src/lib.rs
    │   ├── src/processor.rs
    │   └── tests/mint_tests.rs # program-test integration tests
    ├── solana-lock/            # Solana-side program (deposit / unlock)
    │   ├── src/lib.rs
    │   ├── src/processor.rs
    │   └── tests/lock_tests.rs # program-test integration tests
    └── relayer/                # off-chain relayer (see its README)
        └── src/                  # watcher, signing, attestation
                                # collector, submitter, RPC, CLI
```

## Design (from BRIDGE.md)

- **Lock / mint:** lock SPL tokens in a program-owned vault PDA on Solana,
  mint wrapped tokens 1:1 on Machine. Burn on Machine, unlock on Solana.
  $MACHINE itself is never bridged.
- **Message format:** versioned (`version = 1`), fixed 195-byte canonical
  encoding, `sha256("MACHINE-BRIDGE-V1" || canonical)` is what relayers
  sign. Chain binding, nonce, amount, recipient, and source transaction
  are all in the signed hash.
- **Quorum:** 5 relayers, 3-of-5 distinct valid signatures required.
  Relayers hold no funds and cannot move assets alone.
- **Replay protection:** one PDA per (direction, nonce). A nonce can never
  be processed twice. Attestations expire (`expiry_slot`).
- **Caps:** per-transaction and daily mint/unlock caps, enforced on-chain
  in the programs, not in the relayer. The lock side also enforces a vault
  ceiling on deposits.
- **No admin mint:** minting is ONLY reachable through a verified quorum
  attestation. There is no admin mint instruction in either program.
- **Pause:** callable by the 2-of-3 admin multisig OR any 2 of 5 relayers.
  Unpause is multisig-only (BRIDGE.md section 4.3 — pause is not
  reversible by relayers).
- **Kill-switch (Machine side only):** `KillSwitch` permanently disables
  minting. Burns stay open so users can always exit back to Solana.
  There is no un-kill.
- **Relayer set changes:** `SetRelayers`, multisig-only (BRIDGE.md 3.3:
  a faulty relayer is removed by multisig).

## Build and test

Requires a Rust toolchain (cargo 1.100+ works; the Solana program crates
target Agave 2.3.13, so `solana-program = "2.3"`).

```sh
cd bridge
cargo build --release
cargo test --release
```

`cargo test` runs:

- **bridge-common unit tests** (15): canonical encoding and hashing,
  valid 3-of-5 and 5-of-5 attestations, 2-of-5 quorum failure, duplicate
  relayer signature, unknown relayer, tampered signature, wrong direction,
  expired attestation, wrong version, daily-cap window reset, per-tx cap,
  admin threshold, pause authority, kill-switch semantics, config
  serialization round-trip.
- **machine-bridge integration tests** (9, program-test with the real SPL
  token program): valid 3-of-5 claim mints real tokens, replay of a used
  nonce fails, 2-of-5 fails, over per-tx cap fails, over daily cap fails,
  paused bridge rejects mints (and relayers cannot unpause), kill-switch
  is permanent while burns stay open, admin signatures cannot mint,
  burn records the event and rejects replay.
- **solana-lock integration tests** (10, program-test with the real SPL
  token program): deposit locks tokens in the vault, same-nonce deposit
  fails, deposit over the vault ceiling fails, unlock with a valid 3-of-5
  burn attestation releases tokens, unlock replay fails, 2-of-5 fails,
  over per-tx and daily caps fail, admin signatures cannot unlock, pause
  blocks deposit and unlock (and relayers cannot unpause).

## Deploy (later — not done)

Prerequisites from BRIDGE.md section 5, none of which are met yet: live
3-validator net, public RPC, verified user transfers, fresh production
keys, security audit, worthless-amount test crossings, monitoring.

When those exist, the deploy path is:

1. Build the SBF binaries: `cargo build-sbf` (needs the Solana CLI
   toolchain; plain `cargo build` produces host libraries, not
   deployable programs).
2. Create the wrapped SPL mint on Machine with mint authority = the
   bridge-authority PDA and freeze authority = the bridge-authority PDA.
3. Create the vault token account on Solana with owner = the
   vault-authority PDA.
4. Deploy both programs, then call `Initialize` with the production
   relayer set (5 fresh keys), admin multisig (3 keys, threshold 2),
   and the launch caps. Caps are absolute amounts; BRIDGE.md section 4.4
   gives launch values as 1% of locked supply (per-tx) and 5% (daily) —
   translate them to absolute numbers at deployment.
5. Run the relayer (off-chain, `crates/relayer/`) and do worthless-amount
   test crossings in both directions, including a pause/unpause drill and
   a relayer-outage drill, before any real asset crosses.

## Honest limits

- **No audit.** These programs have not been independently reviewed.
  A bridge holds other people's funds by design; do not deploy this
  with real assets until the audit in BRIDGE.md section 5 is done.
- **No live deployment.** No program is deployed anywhere, no relayer is
  running live, no asset has crossed.
- **Attestation verification uses the ed25519 precompile +
  instructions sysvar** (since the 2026-11-14 code-review changes):
  the runtime verifies the quorum signatures; the programs check
  distinct registered relayers over the message hash in the sysvar.
  The relayer still verifies peers' attestations off-chain with
  ed25519-dalek before aggregating.
- **Relayer: implemented, never run live.** The off-chain relayer
  (watching for finalized deposits/burns, signing attestations,
  collecting a 3-of-5 quorum, submitting them) is in
  `crates/relayer/` and passes its tests locally (22 tests). It has
  never watched a real deposit — there is no deployed bridge or
  public Machine RPC for it to watch yet.
- **No upgrade path in the programs.** Program upgrades go through a
  multisig with a timelock (BRIDGE.md 4.2); the upgrade authority is a
  deployment concern, not program code.
- **Caps are absolute, not percentage-of-vault.** The design's launch
  values are percentages; the programs enforce the absolute numbers set
  at initialization. A percentage-of-vault cap would need the vault
  balance read at mint time and is a possible later change.
- **Daily-cap window uses the Clock sysvar's unix timestamp.** If the
  chain's clock is ever wrong, the window boundary moves with it.
