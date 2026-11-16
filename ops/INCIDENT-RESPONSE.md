# Machine — Incident Response

Who does what when something is wrong with the chain or the bridge.
Written before an incident, because during one is the wrong time to
invent a process. Named *roles*, not named people: fill in the
current holder of each role in your deployment notes (never in this
public repo if it identifies a private contact).

## Roles

| Role | Responsibility |
|---|---|
| Incident lead | Declares the incident, coordinates, decides severity, owns the public status note. |
| Chain operator | Can restart validators, read logs, and run `livenet/check-cluster.sh`. |
| Bridge pauser | Holds one of the keys that can pause the bridge: the admin multisig (2-of-3) or a relayer key (any 2-of-5 relayers can pause). Must be reachable at any hour while the bridge holds value. |
| Comms | Posts updates to the website / X / Discord at the cadence the incident lead sets. |

One person may hold several roles on a small team. The bridge pauser
role must never be a single point of failure: at least two people,
in different time zones, must be able to pause.

## Severity and first actions

### Chain: slots stopped / validators disagreeing
1. Run `livenet/check-cluster.sh` and save its output.
2. Check each validator's log (`journalctl -u mememachine-validator-N -f`).
3. If only one validator is down, restart that one unit. If the
   genesis hash differs between validators, stop the wrong one, wipe
   its `livenet/ledgers/vN`, restart (see livenet/DEPLOY.md
   troubleshooting).
4. Post a status note: chain halted / degraded, since when, next
   update time. Do not speculate on cause in public.

### Bridge: reconciliation mismatch (`ops/reconcile.py` exits 1)
1. **Pause the bridge first, investigate second.** Pause is callable
   by 2-of-3 admins or any 2-of-5 relayers; unpause is multisig-only
   and happens only after the cause is understood (BRIDGE.md 4.3).
2. Freeze the evidence: both RPC responses, the relayer attestation
   files for the affected nonce(s), and the relayer logs.
3. Identify the direction and nonce: compare the lock program's
   `total_locked`, the vault balance, the wrapped supply, and the
   processed-nonce PDAs on both programs.
4. If minted > locked, treat as a potential compromise: engage the
   kill-switch decision (multisig; permanent — minting stops, burns
   stay open so users can exit) and prepare a public disclosure.
5. Do not unpause on a "looks fine now". Unpause requires: root cause
   written down, fix deployed or risk accepted in writing by the
   multisig holders, and reconciliation matching for 60 consecutive
   minutes.

### Bridge: relayers disagree or stall
- Disagreement (different message hashes for one nonce): no quorum
  forms and nothing executes — this is the safe state. Log it, alarm
  the operators of the dissenting relayers, and investigate before
  any manual intervention. A relayer that signed two conflicting
  messages for the same nonce is removed by multisig (BRIDGE.md 3.3).
- Stall (relayers offline): deposits sit in the vault, burns stay
  burned, no funds are at risk. Restore relayers; do not lower the
  quorum to "get things moving".

### Key compromise (any role)
1. Assume the key is public. Act on-chain first: pause (bridge),
   rotate the compromised role's key (relayer set / admin set changes
   are multisig instructions), and for a validator identity, plan a
   coordinated restart with a fresh identity.
2. Preserve logs from the host; rebuild it from scratch rather than
   cleaning it.
3. Disclose after containment, with what the key could and could not
   do (one relayer key alone cannot move funds; one admin alone
   cannot unpause or change caps).

## After every incident
- A short public post-mortem: timeline, impact, root cause, fix.
- Update this document and the relevant runbook with anything that
  was missing. An incident that teaches nothing gets repeated.
