# Verify everything — a new contributor's walkthrough

One page from a fresh clone to a green `make verify`. This is the
single walkthrough; the per-area records stay in `bridge/VERIFY.md`,
`programs/VERIFY.md`, and `programs/PQ-VERIFY.md`, and the current
totals always live in `STATUS.md`.

Everything here is local. `make verify` starts no validator, touches
no ledger or keys, deploys nothing, and moves no assets.

## 1. Prerequisites

- Linux with `git`, `make`, `bash`, and Python 3 (the fork/ops suite
  runs under `python3 -m unittest`).
- A Rust toolchain with Cargo (`cargo --version` must work). If
  Cargo is installed under `~/.cargo/bin` and that directory is not
  on your `PATH`, add it first:

  ```sh
  export PATH="$HOME/.cargo/bin:$PATH"
  ```

- Node.js (the RugCall resolver and app suites run on the built-in
  Node test runner; no `npm install` step — there are no third-party
  dependencies to fetch for the test suites).
- Enough scratch space: the first run compiles both Rust workspaces
  in release mode. Set `TMPDIR` to a roomy directory if `/tmp` is
  small:

  ```sh
  export TMPDIR="$HOME/workspace/tmp"   # any writable directory
  mkdir -p "$TMPDIR"
  ```

No Solana CLI, no SBF toolchain, and no network service are needed
for `make verify` itself. (Fetching crates on the first build does
use the network; later runs reuse the local Cargo cache.)

## 2. Clone and run

```sh
git clone https://github.com/yourmememachine/mememachine.git
cd mememachine
make verify
```

That is the whole walkthrough. A green run ends with:

```
All local checks passed (bridge + programs + fork suite + resolver JS suite).
```

## 3. What each step proves

`make verify` runs five targets, in this order. Any failure stops
the run — fix that step before trusting later ones.

| Target | Command behind it | What it covers |
|---|---|---|
| `test-bridge` | `cd bridge && cargo test --release` | Bridge programs (lock + mint), bridge-common, the relayer, the crossing proof bundle, and the receipt indexer — including the local end-to-end drill and the 45-crossing volume drill. |
| `test-programs` | `cd programs && cargo test --release` | The programs workspace: LaunchGuard (integration, adversarial, demo launch, dust floor), the PQ vault, the registry export, and the RugCall program. |
| `fork-suite` | `./tests/run.sh` | Fork and ops script tests (Python unittest): shell syntax, genesis/patcher checks, cluster and ops script behaviour against stubs. Never starts a validator. |
| `test-resolver` | `./rugcall/resolver/run-tests.sh` | RugCall resolver + evidence suites (Node). Prints its TAP `# tests / # pass / # fail` summary. |
| `test-predict` | `./rugcall/app/run-tests.sh` | RugCall app data-layer suites (Node). Prints its TAP summary the same way. |

You can run any one target on its own, e.g. `make test-bridge`,
while iterating; run the full `make verify` before opening a PR.

## 4. Expected totals

At the time this walkthrough was written and tested end to end
(2026-11-22, from a fresh clone of commit `b98b5d0`; totals updated 2026-11-28 for commit `d67e868`), a green run
reported **827 passed, 0 failed**:

- bridge workspace 247
- programs workspace 226 (LaunchGuard 68, PQ vault 17, registry
  export 11, RugCall program 130)
- fork/ops suite 66
- RugCall resolver JS suite 114
- RugCall app JS suite 174

Totals move as tests are added — `STATUS.md` carries the current
verified numbers and the commit they were verified against. If your
totals differ from `STATUS.md`, that is a finding: check whether you
are on the pinned commit before assuming a regression.

## 5. If it fails

- `cargo: command not found` — the `PATH` step in §1.
- A Rust suite fails on a clean clone at the pinned commit — open an
  issue with the target name, the failing test name, and the last
  ~30 lines of output. Do not patch around it silently.
- A JS suite fails — confirm `node --version` works; the suites use
  only the built-in test runner, so there is no dependency step to
  redo.
- Slow first run — expected; release-mode compilation of two
  workspaces is the cost. Later runs reuse `target/`.

## 6. What `make verify` does NOT prove

Saying this plainly, because the walkthrough would mislead without
it:

- **No SBF build.** The on-chain binaries are built by the separate
  scratch-tree method in `bridge/deploy/SBF-BUILD-NOTES.md`; their
  hashes are recorded in `bridge/VERIFY.md`.
- **No fuzzing.** Bounded fuzz sessions are separate, scheduled
  work; results live in `AUDIT.md`'s fuzz section.
- **No validator, transfer, or cluster proof.** This host family
  blocks UDP send and caps file descriptors, so those proofs need a
  suitable host — see `STATUS.md` NEXT (N1, N2) and the stalled
  local validator disclosed there.
- **No deployment and no audit.** Everything verified here is
  localnet-only code and tests. The deployment gate in `BRIDGE.md`
  §5 still stands.
