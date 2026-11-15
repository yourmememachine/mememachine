# Drill: Relayer Outage

Purpose: prove the bridge's failure mode is *stall, not loss*
(BRIDGE.md 3.2): with relayers down, deposits and burns wait safely;
when a quorum returns, they complete exactly once.

## Steps
1. Stop 2 of the 5 relayers (the most the 3-of-5 quorum tolerates).
2. Make a test deposit. The remaining 3 relayers attest, quorum
   forms, the mint completes. Record: slower is fine, wrong is not.
3. Stop 1 more relayer (3 now down, 2 up — below quorum).
4. Make a second test deposit. Observe: signatures are produced by
   the 2 live relayers, no quorum forms, **no mint happens**, the
   deposit remains in the vault. This stall is the expected state —
   anyone "fixing" it by lowering the quorum has failed the drill.
5. Make a test burn on Machine as well; it likewise waits.
6. Restart the stopped relayers. They sign the pending events, both
   quorums form, both crossings complete exactly once each.
7. Replay either attestation: rejected on the nonce.
8. Reconciliation matches at the end; total elapsed stall time is
   in the drill log.

## Pass criteria
- Quorum of 3 (with 2 down) still crossed.
- Below quorum: nothing crossed, nothing lost, no manual override
  was used or needed.
- On recovery: every pending crossing completed exactly once.
- Reconciliation matched at the end.
