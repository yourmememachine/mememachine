# Staged launch plan — value last, proof first

Adopted 2026-11-14. Machine does not wait for a full audit budget
to exist in public — and no real asset crosses the bridge before an
independent audit, no matter how long that takes. The phases below
separate **proving the system** (cheap, starts now) from **trusting
it with value** (expensive, gated).

The rule that never changes: **the bridge-deployment gate in
BRIDGE.md §5 binds at every phase.** A phase may only do what its
gate allows.

## Phase 0 — Public proof of work ($0) — *current*

- Everything in the open: code, tests (197 bridge + 49 fork/ops +
  68 LaunchGuard + 11 registry export + 17 PQ vault — 342 total
  as of 2026-11-19), SBF hashes, runbooks, drills, honest status docs.
- Free security passes: `cargo audit`, clippy, cargo-fuzz, plus a
  documented self-review checklist. (Sec3 X-Ray turned out to be
  paid-only and Soteria cannot read this codebase's toolchain —
  both recorded in AUDIT.md.)
- Audit quotes in hand (Accretion, Sec3 — inquiries sent 2026-11-14).
- Grant applications for audit funding (Solana ecosystem programs).
- Build in public (X updates from the repo's own status docs).

**Proves:** the code exists, builds reproducibly, and is test-covered.
**Costs:** $0. **Value at risk:** none.

## Phase 1 — Public testnet (~$20/mo)

- One VPS (Vultr-class, $20/mo): validators + public RPC + faucet,
  on a **test genesis** whose tokens are worthless by definition.
- Bridge programs deployed to Machine-testnet ↔ Solana **devnet**;
  worthless-amount crossings in both directions, plus the pause and
  relayer-outage drills from `bridge/drills/`, run for real and
  written up.
- LaunchGuard live on testnet: real projects can register launches
  and be verified against it at zero financial risk.

**Proves:** multi-validator consensus, signed transfers, public RPC,
the full bridge path, and the launch standard — end to end, live.
**Costs:** ~$20/mo hosting. **Value at risk:** none (test tokens).

## Phase 2 — Distributed livenet, still no bridged value (~$60/mo)

- Three servers, one validator each, in different regions
  (the `livenet/DEPLOY.md` runbook), public RPC, monitoring and
  reconciliation from `ops/` running for real.
- Community/stake onboarding per the public-readiness items in
  LAUNCH_CHECKLIST.md. Bridge stays **undeployed on mainnet** in
  this phase; LaunchGuard may deploy here only after its own review
  pass, since it custodies locked creator tokens (its audit joins
  Scope A or runs as its own smaller engagement).

**Proves:** the network runs distributed, in public, over time.
**Costs:** ~$60/mo. **Value at risk:** none bridged; locked-launch
custody only if LaunchGuard's review is complete.

## Phase 3 — Audited bridge deployment (grant/budget-gated)

- Independent audit of Scope A (bridge programs, bridge-common,
  relayer, key management) by the selected firm; findings fixed and
  re-reviewed; report published (per AUDIT.md).
- Fresh production keys generated **on the deployment hosts**,
  SBF deploy, initialize with production relayer/admin sets and
  absolute caps from `bridge/deploy/caps.py`.
- Worthless-amount crossings on mainnet first, drills again, then —
  and only then — real assets may cross, under the caps.

**Proves:** the value path is independently reviewed.
**Costs:** the audit ($20K–$60K band at a boutique firm; quotes
pending) + ~5 SOL of program rent deposits. **Value at risk:**
capped, audited, monitored.

## Phase 4 — Harden and grow

- Funded bug bounty (scoped in SECURITY.md) sized to the value the
  bridge actually holds.
- Fork-configuration audit (AUDIT.md Scope B).
- Caps raised only with operating history, never ahead of it.

## What this plan refuses to do

- No phase skips the audit because the code "looks fine."
- No phase lets marketing claim a phase's proofs it hasn't run:
  STATUS.md and LAUNCH_CHECKLIST.md stay the single source of truth.
- No token sale to fund the audit — the launch plan stays as decided
  2026-11-14 (see git history); funding comes from grants and budget,
  in that order.
