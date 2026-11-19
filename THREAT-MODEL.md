# Machine — Threat Model

One page, in plain language: what the bridge and LaunchGuard are
built to withstand, what they are not, and what they assume. The
binding design document is BRIDGE.md; the audit scope is AUDIT.md;
current limitations are in LIMITATIONS.md.

## Assets being protected

- SPL tokens locked in the bridge vault on Solana.
- The wrapped supply on Machine (must never exceed what is
  locked — reconciliation, `ops/reconcile.py`, checks exactly this).
- $MACHINE itself is **never** bridged and is not at risk from the
  bridge.
- LaunchGuard vaults: creator tokens locked for a registered launch.

## Who might attack

- A stranger with no keys, submitting crafted transactions.
- One compromised relayer key (of five).
- One compromised admin key (of three).
- A malicious token creator trying to register a launch and still
  rug it.
- A malicious or careless relayer operator (signing too early,
  signing a mis-built message, going offline).

## What the design protects against

- **Forged crossings.** A mint or unlock needs a 3-of-5 relayer
  attestation over the exact message hash, verified on-chain via
  the ed25519 precompile + instructions sysvar. The hash binds
  every message field — amount, recipient, nonce, chains, expiry —
  and a test proves changing any single field breaks verification.
- **Replay.** Every crossing consumes a one-time nonce PDA on its
  value path; the same attestation can never be spent twice.
- **Runaway loss.** Absolute per-transaction and daily caps on both
  programs, plus dynamic caps on unlocks (1% of the live locked
  total per transaction, 5% per 216,000-slot window). Even a full
  quorum compromise drains at most the capped amounts before the
  pause or kill-switch engages.
- **A single bad key.** No single relayer can attest (3-of-5), no
  single admin can pause-rotate-kill (2-of-3), and relayers cannot
  unpause — only admins can.
- **Relayer disagreement or haste.** Relayers sign only finalized
  source events (+16 slots); the collector refuses to assemble a
  crossing when relayers disagree. Nothing mints on a split vote.
- **Phantom deposits (Coreum, Aug 2026).** A relayer must never
  treat "a transaction exists" as proof of a deposit: the watcher
  verifies the source transaction contains the transfer behind
  the event record — into the vault for a deposit, burning the
  wrapped mint for a burn, for exactly the record's amount
  (found in top-level or inner instructions, since the transfer
  is a CPI) — and refuses to sign otherwise. A transaction that
  only moves tokens between other wallets yields no sender at all.
- **Being trapped.** The kill-switch permanently stops minting but
  burns — the exit path — stay open. Pause likewise never blocks
  the design's exit guarantees beyond what BRIDGE.md §4.3 states.
- **LaunchGuard rugs.** Registration is only possible with mint
  and freeze authorities already revoked (an irreversible SPL
  state — a test proves they cannot be regained), at least 10% of
  supply locked for at least 30 days, releasable only to the
  creator and only after expiry. There is no admin key over
  launches; the program's one privileged call is a one-time
  Initialize (upgrade-authority-bound, §5) that sets the dust
  floor and touches no launch account.
- **Parser attacks.** The precompile/sysvar parser treats all
  transaction-supplied bytes as hostile: bounds-checked in-crate,
  fuzzed (which already caught one real panic in an upstream
  helper), and covered by deterministic adversarial tests.
- **Post-incident rush.** A bridge that was just paused — for a
  reconciliation mismatch or anything else — does not reopen at
  full speed: every unpause tightens the dynamic caps to a
  quarter (0.25% / 1.25%) for one full window, relaxing on a slot
  deadline stored on-chain, with no second decision to forget.
- **Unverifiable crossings.** Every completed mint and unlock
  writes a receipt PDA (message hash, source tx, amount,
  recipient, slot, timestamp) that anyone can derive from the
  crossing's (direction, nonce) and read. Proof that a crossing
  happened does not depend on the relayers or any website
  staying up or staying honest.

## What it does NOT protect against

- **Collusion at quorum.** If 3 of 5 relayers (or 2 of 3 admins)
  act together maliciously, they can attest false crossings up to
  the caps. This is the stated trust assumption — the set is a
  committee, not a proof. Caps and reconciliation bound the blast
  radius; they do not remove the assumption.
- **Bugs not yet found.** The code is **unaudited**. Testing,
  fuzzing, and three external reviews reduce risk; they are not a
  substitute for the independent audit that gates any real asset
  (BRIDGE.md §5).
- **The host and validator layer.** Key theft from a deployment
  host, a compromised validator, or Solana-side failures are
  operational risks handled by the deployment runbook (fresh keys
  on the host, role separation), not by the programs.
- **Dust and state spam, beyond self-limiting costs.** Zero-amount
  crossings are rejected, and every nonce/EventRecord PDA is paid
  for by the account that creates it — on the mint side, creation
  additionally requires a valid quorum attestation, so an attacker
  cannot force state growth there without relayer cooperation. A
  minimum-transfer floor was considered and deliberately left as a
  deployment-gate decision (with the audit's input) rather than a
  hardcoded constant.
- **Solana itself.** If Solana's runtime or the SPL Token program
  fails, the lock side inherits that failure. Machine inherits
  Agave's risk profile for the same reason it inherits its testing.

## Assumptions the design relies on

- At most 2 of 5 relayers and at most 1 of 3 admins are malicious
  or compromised at any time.
- Relayer and admin keys are generated and kept on their own
  hosts (never on a shared dev machine — the repo's `keys/` are
  throwaway and must never hold value).
- Finality on the source chain means what it normally means;
  relayers wait for finalization plus 16 slots before signing.
- The deployment gate (BRIDGE.md §5) is actually followed: fresh
  production keys, audited binaries, capped initialization, and
  worthless-amount test crossings before any real asset.
