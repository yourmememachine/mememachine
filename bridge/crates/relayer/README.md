# Machine Bridge Relayer

The off-chain relayer for the Machine bridge (BRIDGE.md sections
2.5 and 3). It watches for finalized deposits and burns, signs
3-of-5 attestations, collects a quorum, and submits `MintWrapped` /
`Unlock` to the destination chain.

**Status: built and tested locally (22 tests, see below). NOT
deployed. NOT audited. No asset has crossed with it.** The deploy
gate in BRIDGE.md section 5 applies to the relayer exactly as it
does to the programs: live net, public RPC, fresh production keys,
independent audit, worthless-amount crossings, monitoring.

## What it does

1. **Watch** — by nonce cursor, not account scans. Nonces are
   strictly increasing per direction and each event lives at a
   deterministic PDA (`EventRecord` written by `Deposit` / `Burn`),
   so the relayer probes the next nonce, in order, and never skips
   one. A gap stalls the bridge; stalling is safe.
2. **Wait for finality** — never sign a merely confirmed event.
   Solana -> Machine requires the finalized slot plus a 16-slot
   safety margin (BRIDGE.md 2.5); Machine -> Solana requires a
   rooted slot. This is enforced in `message::check_finalized`,
   before any signature exists.
3. **Build the message deterministically** — every field of
   `BridgeMessage` is in the signed hash, so all relayers must build
   the identical message. `expiry_slot` is therefore derived from
   the source slot (`source_slot + expiry_window_slots`, a shared
   config value), never from a local clock. Sender and source
   transaction are read from the source transaction itself (parsed
   SPL transfer/burn authority), not asserted by any relayer.
4. **Sign** — ed25519 over the message hash, with this operator's
   own keypair. One signature moves nothing.
5. **Exchange + collect** — phase-1 transport is one signed JSON
   file per (direction, nonce, relayer) in a shared directory
   (`attestation.rs`). The collector refuses to form a quorum if
   files disagree on the message (different hashes for one nonce),
   if a signer is not in the registered set, or if a relayer signs
   twice — and it re-verifies the collected set with bridge-common's
   own `verify_attestation`, the same code the programs run.
6. **Submit** — builds `MintWrapped` / `Unlock` with the account
   order the processors expect and submits a signed transaction.
   Dry-run unless the config sets `submit_enabled: true`.

Relayers hold no funds. The mint authority is a program PDA; caps
and replay protection are enforced on-chain, not here.

## Run it

```bash
cp bridge/crates/relayer/relayer.example.json relayer.json
# Edit relayer.json: real program ids, mints, vault, YOUR keypair path.
# Every operator uses the same shared values and expiry_window_slots;
# only the keypair (and usually the payer) differs.

cargo run --release -p relayer -- --config relayer.json sign-once
cargo run --release -p relayer -- --config relayer.json collect \
    --direction solana-to-machine --nonce 7
cargo run --release -p relayer -- --config relayer.json submit \
    --direction solana-to-machine --nonce 7 \
    --destination-token-account <recipient's wrapped-token account>
cargo run --release -p relayer -- --config relayer.json run \
    --destination-token-account <...>
```

The destination token account must be owned by the message recipient
and hold the correct mint; the destination program enforces both.

## Tests

```bash
cargo test --release -p relayer
```

20 unit tests (config validation, finality margin, deterministic
expiry, signing against bridge-common's verifier, attestation
collection incl. disagreement and 2-of-5 failure, instruction
account order and data round-trip, sender extraction from parsed
transactions, watcher cursor behaviour) plus 3 integration tests
(deposit -> 3-of-5 quorum -> MintWrapped instruction; expired
attestation rejected; and the full local end-to-end drill in
`tests/full_drill.rs` — both directions executed against the real
programs, with a forced pause and a forced relayer disagreement;
see `bridge/drills/LOCAL-DRILL.md`).

## Honest limits
- **Phase-1 transport is files.** A peer-to-peer attestation
  protocol is a later, separately reviewed step; the collector's
  interface (`AttestationFile`) is the seam where it plugs in.
- **One destination token account per submission** is supplied by
  the operator. Deriving/creating recipients' associated token
  accounts automatically is future work, and must be reviewed.
  The failure mode for a wrong account is safe and explicit: the
  bridge program checks on-chain that the destination account is
  owned by the message recipient and holds the wrapped mint, so a
  wrong-owner account fails with `Unauthorized` and a wrong-mint
  account with `InvalidAccountData`; the transaction fails, no
  funds move, and the nonce is not consumed, so the crossing can
  be retried with the correct account. Both cases are covered by
  program tests (`mint_to_wrong_owner_account_fails_and_nonce_survives`,
  `mint_to_wrong_mint_account_fails`).
- **No live run has happened.** No relayer has watched a real
  deposit, because there is no deployed bridge and no public
  Machine RPC yet. The first live exercise is the worthless-amount
  test crossing in `bridge/drills/test-crossing.md`.
