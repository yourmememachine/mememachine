# Adoption metrics — tracked from zero

The three adoption metrics committed to in the Solana Foundation
grant proposal (2026-11-15), tracked here from the start so the
numbers — including the zeros — are public and auditable. Update
this file whenever a number changes; every entry needs a link or a
tx signature behind it, the same rule as STATUS.md.

| Metric | Target | Current | How it is counted |
|---|---|---|---|
| External testnet node operators | ≥ 3 | **0** | A validator not run by the project joins the public testnet and votes. Counted once per operator, when their validator first appears in the leader schedule / vote accounts. |
| LaunchGuard integrations or evaluations | ≥ 3 | **0** | An external project registers a launch on the testnet, or completes a documented evaluation (public post, issue, or write-up) of the standard. |
| Documented public testnet crossings | ≥ 100 | **0** | A completed bridge crossing on the public testnet (either direction, worthless test assets only) with its tx signatures recorded in `bridge/VERIFY.md` style write-ups. |

## Log

- 2026-11-15 — Tracking started. All three metrics are 0: the
  public testnet is not live yet (Phase 1 of STAGED-LAUNCH.md is
  prepped; hosting is pending), and nothing external has integrated
  or crossed. Local drills do not count toward these numbers —
  they are adoption metrics, not test metrics.
