# SBF build — how the deployable binaries were produced (2026-11-14)

**Result: both bridge programs compile to deployable SBF binaries.**
Built with the Agave 2.3.13 toolchain (`cargo build-sbf`, platform-tools
cargo 1.84.0) from commit `6c06d64` plus the docs update that records this.

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `solana_lock.so` | 349,952 | `a229cb6ff0d3b7992a576891be375429474167c8d11ccca9b17b5ae8ad554090` |
| `machine_bridge.so` | 350,192 | `672b42a8ca9562ecd6c535833b4a65143f6485f25c12f9fb6dd5e8b417402095` |

**Rebuilt 2026-11-14 (evening)** from the code-review changes
(ed25519-precompile verification, dynamic caps — see
`bridge/VERIFY.md`):

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `solana_lock.so` | 205,736 | `05ab551aa74d916b886cd77d9d1cbc977437917f6e24b30361d022a67b9be6a6` |
| `machine_bridge.so` | 206,016 | `eacc1a86a10095e34688487eec019a206ce975ce57bc08610bc019191478da21` |

Build note from the same evening: the scratch tree's manifests must
keep their `[dev-dependencies]` stripped (step 1) — restoring the
full workspace manifests makes cargo re-resolve the graph, pulls the
edition2024 test-only chain back in, and the platform-tools cargo
fails exactly as described below. With the stripped manifests, the
scratch lockfile is byte-identical to the saved `sbf-Cargo.lock`
(pins listed below), and the rebuild above was produced from it.

Both are ELF 64-bit sBPF shared objects (e_machine 0x107), stripped.
They are **not deployed anywhere** — deployment stays gated on
BRIDGE.md §5 (audit, fresh production keys, live net).

## Why the build does not run directly against the bridge workspace

`bridge/deploy/build-sbf.sh` against the workspace fails before any
program code is compiled: the platform-tools cargo (1.84) cannot even
*parse* manifests of crates that use `edition2024`, and the unified
workspace `Cargo.lock` contains several — some pulled in only by
test-only dependencies (`solana-program-test` → `solana-accounts-db`
requires `blake3 ^1.8`, whose `digest 0.11` → `block-buffer 0.12` chain
is edition2024). This is a toolchain-age problem, not a program-code
problem: the host toolchain (cargo 1.100) builds and tests the same
code fine (56/56).

## Method used (reproducible)

1. Copy `crates/bridge-common`, `crates/solana-lock`,
   `crates/machine-bridge` into a scratch workspace containing only
   those three crates, with `[dev-dependencies]` stripped (test-only;
   not part of the deployable libraries).
2. Seed its lockfile from the bridge workspace `Cargo.lock`, then pin
   the edition2024 offenders down to the newest pre-edition2024
   releases their dependents allow:
   - `blake3 1.8.7 → 1.5.5` (satisfies `solana-program`'s `^1.5.5`)
   - `zeroize 1.9.1 → 1.8.1`
   - `base64ct 1.8.3 → 1.6.0`
   - `indexmap 2.14.2 → 2.13.1` (drops the edition2024 `hashbrown`)
   - `proc-macro-crate 3.5.0 → 3.3.0` (drops `toml_edit 0.25` /
     `toml_parser` for `toml_edit 0.22`)
   The resulting lockfile is saved as `bridge/deploy/sbf-Cargo.lock`.
3. Run `cargo build-sbf` for both program manifests with the Agave
   2.3.13 release `bin/` on `PATH` (installed by `./install.sh`; on
   restricted containers where tar cannot chown, run it with
   `TAR_OPTIONS=--no-same-owner`).

## Caveats an auditor should check

- The pins above change *transitive dependency versions* relative to
  the workspace lockfile used for host tests. The program crates' own
  code and their direct dependencies are unchanged, and the host test
  suites (56/56) were run against the workspace lock. Rebuilding from
  `sbf-Cargo.lock` and comparing hashes is the reproducibility check.
- The generated `*-keypair.json` files from this build are throwaway
  build artifacts in a scratch directory — they are **not** program
  identities and **not** production keys. Production program keypairs
  are generated on the deployment host per DEPLOY-BRIDGE.md.
