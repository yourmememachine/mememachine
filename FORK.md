# Machine — How Deep Is the Fork?

Plain answer first: **today Machine runs on stock, prebuilt Agave
2.3.13 binaries with a custom genesis, custom on-chain programs, and
custom tooling — and now also has a real, verified source patch set that
is not yet built or run.** The patched build is the path to a deeper
fork; it is not the running code. Nothing in this file overstates that.

Repository: https://github.com/yourmememachine/mememachine

## 1. What is stock Agave

The validator currently running is the **prebuilt Agave 2.3.13 release**,
not a build from modified source:

| Item | Value | Source |
|---|---|---|
| Release | Agave 2.3.13 | `bin/` + `rel-2.3.13/solana-release/version.yml` |
| Commit | `5466f4592b1983adb13ba0a5d53f41ea2de69fba` | `version.yml`, `GENESIS.md` |
| Binaries | `bin/agave-validator`, `bin/solana-genesis`, `bin/solana-keygen`, `bin/solana`, `bin/agave-ledger-tool`, `bin/solana-test-validator` | `bin/` (from `solana-release-2.3.13.tar.bz2`) |
| Measured version output | `agave-validator 2.3.13 (src:5466f459; feat:2142755730, client:Agave)` | run on this VM, 2026-11-14 |
| Measured CLI output | `solana-cli 2.3.13 (src:5466f459; feat:2142755730, client:Agave)` | run on this VM, 2026-11-14 |

Note `client:Agave` in both lines: the running binaries identify as
stock Agave. Every consensus rule, vote rule, shred rule, fee rule, and
bank-hash computation in the running binary is exactly Agave 2.3.13 as
released. No source change of any kind is in the running code.

## 2. What is patched at source level

`patches/` holds three unified diffs against the real Agave 2.3.13
source (`anza-xyz/agave`, tag `v2.3.13`). Full detail and verification
output: `patches/README.md`.

| Patch | File | Change |
|---|---|---|
| `01-client-id.patch` | `version/src/lib.rs` | New client id 4, `Machine`, as the default client. A binary built from this source prints `client:Machine` in `--version` output and gossip instead of `client:Agave`. Gossip identity label only. |
| `02-snapshot-interval.patch` | `runtime/src/snapshot_bank_utils.rs` | Default incremental snapshot interval `100` -> `500` slots. Operational default only. |
| `03-startup-banner.patch` | `validator/src/commands/run/execute.rs` | One startup log line: `Chain: Machine (Agave 2.3.13 fork)`. Log line only. |

Deliberately untouched: ticks per slot, slots per epoch, vote/shred
rules, fee schedule, rent, bank-hash computation, and the full-snapshot
interval (50,000 slots). No consensus-critical parameter was changed.

Verification done 2026-11-14 on this VM (see `patches/README.md` for
commands and output):

- Source downloaded as the v2.3.13 tarball from GitHub codeload;
  SHA-256 `35323fd76b6e78578e157c6fac854b0930f42d0b073ca659f10ddfddf0e6ace5`.
- `patch --dry-run -p1`: exit 0 for all three patches.
- `git apply --check`: exit 0 for all three patches.
- All three applied for real in a scratch tree; each resulting file was
  byte-identical to the intended patched file.
- Patch 01 additionally compiled and passed its unit tests:
  `cargo test -p solana-version --lib` → 2 passed, 0 failed
  (`test_client_id`, `test_compute_commit`). All three patched files
  parse as valid Rust.
- `build-from-source.sh` was run end-to-end in `--skip-build` mode:
  fresh download, checksum verification, and patch application all
  succeeded.

Not verified, stated plainly:

- **The full Agave release build was not run.** This VM has 1.5 shared
  CPUs, ~8 GB RAM, and no Rust 1.86.0 toolchain installed (the source
  tree pins 1.86.0 in `rust-toolchain.toml`). A full build needs a much
  bigger machine and a long build. Patches 02 and 03 were therefore
  verified by clean application plus a syntax parse, not by compiling
  the `solana-runtime` or `agave-validator` crates.
- **No patched binary has been built or run.** Until one is,
  `client:Machine` is a property of the patch set, not of any
  running node.

Build it with `./build-from-source.sh` (downloads the source, verifies
the checksum, applies the patches, builds, and stages binaries in
`./bin-patched/` without touching `./bin/`). Swapping a patched binary
in for the prebuilt one is a separate, manual operator step.

## 3. What is custom genesis / config

All of this lives in genesis and configuration, not in Agave source.
Details: `GENESIS.md`, `livenet/`.

- **Chain identity:** name Machine; native gas token $MACHINE
  (1 MACHINE = 1,000,000,000 lamports); genesis hash
  `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`; shred version `35836`;
  cluster type Development; created 2026-11-14.
- **Supply:** 1,000,000,000 MACHINE, fixed, no inflation. Founder
  `7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz` holds 998,999,000;
  stake account `CxRRi4HZqJoG3fWugwF5wT2bqVwyMgPXxfKDRWCfe87e` holds
  1,000,000; faucet `FVFXT54BR2W6qmbnMuoCcHiufxX47LjXh1Nhissku7KC`
  holds 1,000. (Keys in `keys/` are throwaway development keys.)
- **Fee schedule (FeeRateGovernor, from genesis):** 9,500 lamports per
  signature current, 10,000 target, 5,000 minimum, 100,000 maximum;
  20,000 target signatures per slot; 50% fee burn; 50% rent burn;
  3,480 rent lamports per byte-year.
- **Vote-authority patch (genesis level, not a source patch):** the
  vote account's authorized voter was patched from the identity keypair
  to the vote keypair (`patch-vote-authority.py`) so
  `solana-test-validator` can vote. The live genesis hash above is the
  post-patch hash; the pre-patch creation hash was
  `7xzo2mxBzmZY4bbqHHyU3YUVqJxfUV6KvWyviuZzoRuE`. See `GENESIS.md`.
- **Livenet package (`livenet/`):** 3-validator genesis, per-validator
  start scripts, systemd units, port/firewall notes, VPS deployment
  guide. Status: **not running yet** — no host has been paid for. The
  local single-node net is the only one running.
- **Local-net limits, measured:** this VM cannot raise the
  file-descriptor limit past 4,096 (Agave needs 1,000,000) and blocks
  UDP send, so only 1 validator runs here and transfers do not confirm.
  The local net votes, confirms, and finalizes.

## 4. What is custom programs / tooling

Original code written for this chain, outside Agave source:

- **Bridge (`bridge/`, design in `BRIDGE.md`):** native Rust on-chain
  programs — `bridge-common` (shared message format, attestation
  verification, caps), `machine-bridge` (Machine side: mint wrapped /
  burn), `solana-lock` (Solana side: deposit / unlock) — plus program
  tests. Status per `bridge/README.md`: built and tested locally, **not
  deployed, not audited, no asset has crossed, nothing holds real
  funds**. No relayer exists yet.
- **Scripts and docs:** `start-validator.sh` (boots the single-node
  local net from the shipped genesis), `install.sh`,
  `patch-vote-authority.py`, `run-in-ns.sh`, plus `WHITEPAPER.md`,
  `SECURITY.md`, `LAUNCH_CHECKLIST.md`, `DEVTO_ARTICLE.md`, and this
  file. The website and dev.to article are separate from this repo
  checkout.

## 5. How deep is the fork, honestly

| Layer | Status |
|---|---|
| Validator binary (running) | Stock Agave 2.3.13, prebuilt. `client:Agave`. |
| Genesis, supply, fees, keys | Custom. This is what makes the chain its own chain today. |
| On-chain programs (bridge) | Custom Rust, local-only, not deployed. |
| Source patches | Real, verified to apply cleanly, one crate test-passing — **not yet built or run**. |
| Consensus rules | Stock Agave. Nothing changed, nothing claimed. |

What would make it deeper, in order: (1) build the patched source on a
machine big enough for a full Agave build and run the resulting binary
until it reports `client:Machine`; (2) add a source-level change
with an observable on-chain effect that is still safe to test locally
(e.g. a custom genesis-time default carried in source rather than in
flags); (3) only then consider any change near consensus rules, with a
multi-validator local net to test it on. None of step 2 or 3 has been
done, and none is claimed here.
