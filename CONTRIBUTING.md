# Contributing to Machine

Machine is a Solana-derived Layer 1 (Agave 2.3.13 fork) with
LaunchGuard, a chain-native anti-rug launch standard, and a
lock/mint bridge to Solana. Everything here is public and
Apache-2.0. Outside eyes are welcome — reviewing, testing, and
breaking things locally is exactly the help the project needs
before any audit.

## Ground rules (non-negotiable)

- **Localnet only.** Nothing in this repo is deployed, and no
  contribution should deploy anything, anywhere, with real assets.
- **$MACHINE is never bridged** and there is no token sale. The
  bridge moves other assets, and only after an independent audit
  (see `BRIDGE.md` §5, the deployment gate).
- **Honest claims only.** If a thing is not proven by a run, a
  test, or a file, the docs say so. Pull requests that overclaim
  will be sent back, however good the code is.
- **No production keys.** Keys are generated on deployment hosts
  only, never committed, never generated in a PR.

## Get it running

```sh
git clone https://github.com/yourmememachine/mememachine.git
cd mememachine
make verify
```

`make verify` runs the full local suite: bridge workspace tests,
LaunchGuard tests (integration + adversarial + the demo launch),
and the fork checks. It must be green before and after your
change. Current totals and what is proven live in `STATUS.md`;
`bridge/VERIFY.md` and `programs/` carry the per-area records.

## Where help is wanted

- **Adversarial tests.** Try to break the bridge programs or
  LaunchGuard in a test: malformed inputs, wrong signers, cap
  boundaries, replay attempts. A failing-closed test is a
  contribution; a test that actually breaks an invariant is a
  bigger one — open an issue first for those.
- **Fuzzing.** Targets live in `bridge/fuzz/`. Bounded runs with
  recorded results (corpus size, executions, findings) belong in
  `AUDIT.md`'s fuzz section.
- **Review.** Read `THREAT-MODEL.md` and `LIMITATIONS.md`, then
  read the programs in `bridge/` and
  `programs/crates/launch-guard/` against them. Findings as
  issues, with the smallest reproduction you can build.
- **Docs accuracy.** If a doc states something the code or a test
  contradicts, that is a bug. File it or fix it.

## Pull requests

- One topic per PR; tests included for behavior changes.
- `make verify` green, and if your change moves test totals or
  hashes, update the status docs the way the recent commits do
  (see the pin commits after any feature commit).
- Program-scope changes must be flagged in `AUDIT.md` — auditors
  quote against this repo, and the scope record has to stay true.
- By contributing you agree your work is licensed Apache-2.0,
  same as the project.
