# Drill: Worthless-Amount Test Crossing (both directions)

Purpose: prove a real asset — with no value — can cross the deployed
bridge in both directions, exactly once, with reconciliation matching
at every step. This is BRIDGE.md section 5 item 6 and a Phase 1 gate
item. Run it only on the deployed testnet/capped setup from
`bridge/deploy/DEPLOY-BRIDGE.md`; never as a first run with a real
asset.

## Setup
- A test SPL mint on Solana that you control, with no value.
- The bridge deployed and initialized for that mint (deploy runbook
  steps 1–6), relayers running with `submit_enabled: true` on the
  test setup.
- `ops/reconcile.py` pointed at the test vault + wrapped mint.
- A worksheet (spreadsheet or paper): nonce, direction, amount,
  source signature, destination signature, balances before/after.

## Direction 1: Solana -> Machine (lock / mint)
1. Record: vault balance, wrapped supply, recipient's Machine
   balance. Reconciliation must already match.
2. Deposit a small amount (e.g. 10 test tokens) with the lock
   program's `Deposit` instruction, destination = the recipient's
   Machine address, nonce = next unused Solana->Machine nonce.
3. Wait for finality on Solana (finalized + 16 slots; the relayers
   enforce this — a mint before that is a drill failure, report it).
4. Observe the 5 relayers' attestation files: at least 3, all with
   the same message hash. Any disagreement stops the drill.
5. `MintWrapped` lands on Machine. Check: recipient balance +10,
   wrapped supply +10, vault balance +10, nonce PDA exists on
   Machine (replay protection armed).
6. Replay attempt: resubmitting the same attestation must fail with
   the nonce already processed. Record the error.
7. Reconciliation matches.

## Direction 2: Machine -> Solana (burn / unlock)
1. Record balances and supply again.
2. Burn the same 10 wrapped tokens with the bridge program's `Burn`
   instruction, destination = the recipient's Solana address,
   nonce = next unused Machine->Solana nonce.
3. Wait for a rooted slot on Machine; relayers attest; at least 3
   signatures, one hash.
4. `Unlock` lands on Solana. Check: recipient's Solana balance +10,
   vault balance −10, wrapped supply −10.
5. Replay attempt on the unlock must fail on the nonce.
6. Reconciliation matches. Net effect of the round trip: zero,
   everywhere. Any non-zero residue is a failed drill.

## Pass criteria
- Both directions complete with exactly-once effect.
- Both replay attempts rejected.
- Reconciliation matched before, between, and after.
- The worksheet is complete enough for an auditor to re-verify every
  number from the two chains' RPCs alone.
- Repeat toward the Phase 1 total: 100 crossings, 0 lost or
  duplicated tokens (BRIDGE.md section 6).
