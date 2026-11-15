# Drill: Pause / Unpause

Purpose: prove the bridge can be stopped fast by the people who are
supposed to stop it, that relayers alone cannot restart it, and that
funds stay put throughout (BRIDGE.md 4.3). Run on the test setup
after `test-crossing.md` passes once.

## Steps
1. Start a test deposit so an event is in flight.
2. **Relayer pause:** any 2 of the 5 relayer operators sign `Pause`
   on the Machine bridge program. Time it — target: paused within
   5 minutes of the decision.
3. Attempt a `MintWrapped` with a valid quorum attestation. It must
   fail: bridge paused. The deposit sits in the vault; nothing is
   lost, and the drill log says so.
4. **Relayer unpause attempt:** the same 2 relayers sign `Unpause`.
   It must fail — unpause is multisig-only, by design.
5. **Multisig unpause:** 2-of-3 admins sign `Unpause`. It succeeds.
6. The in-flight crossing now completes normally (or is re-driven
   from the attestation files). Reconciliation matches.
7. Repeat steps 2–6 on the Solana lock program (pause blocks both
   deposits and unlocks there).

## Pass criteria
- Pause effective in both programs, within the time target.
- No mint/unlock succeeded while paused.
- Relayers could not unpause; the multisig could.
- No funds moved unexpectedly at any point; reconciliation matched
  at the end.
- Drill log (times, signatures, errors) filed with the deployment
  notes.
