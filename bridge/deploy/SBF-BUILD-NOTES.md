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
| `solana_lock.so` | 201,744 | `2b2beeb682229ae4e9f1296fe540439232506f2ba4ab002ac1583462e049839f` |
| `machine_bridge.so` | 202,024 | `42cae7f08e9194daafdc01fd30d9940ebba794b026005d9176eaf52be7640edd` |

**Rebuilt 2026-11-15** from the nonce-gate hardening (the four
processors' inline nonce-PDA checks moved into the shared
`bridge_common::check_nonce_unprocessed` helper; behavior
unchanged, see `bridge/VERIFY.md`):

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `solana_lock.so` | 201,704 | `f2bdc178495af0db34298178865fd9c9408f07f152169973c68a5ebd82b8aa41` |
| `machine_bridge.so` | 202,016 | `9570f07d503092fe586f8212987996274807fbe6d5de6f73558f8ed8cb73abb6` |

**Rebuilt 2026-11-16** from the hardening pass (receipts + cap
tightening from `8369f99`, precompile-parser adversarial tests from
`b9d98d3`; same program-only lockfile method — the scratch
lockfile was verified byte-identical to the saved
`sbf-Cargo.lock`; platform-tools v1.48):

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `solana_lock.so` | 204,272 | `cda1b66ffb32d2c6fe94e0ccbd96ffd95736a78845667a33ed3dc8a64cde8d35` |
| `machine_bridge.so` | 204,016 | `ecd19d0f8765eb1e333323526cf11e1abd760f98a87bf42d6e13ec55f5eecb77` |

Hashes differ from the 2026-11-15 record because the program code
changed after that rebuild. Rebuilding solana-lock in place
reproduced the identical hash (deterministic build). These binaries
were used for the SBF compute-unit measurement in
`bridge/VERIFY.md` (MintWrapped 92,152 CU total, Unlock 86,782 CU
total). **launch_guard.so was NOT rebuilt in this pass** — the
2026-11-15 hash (`405a0075…`, 115,208 B) is stale for the current
sources after the 2026-11-16 forgery patch; rebuild outstanding
before any LaunchGuard deployment. The `.so` files themselves live
only in the ephemeral scratch directory and are not committed —
the hashes above are the record, matching existing practice.

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
code fine (109/109 bridge workspace).

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
  suites (109/109 bridge workspace) were run against the workspace lock. Rebuilding from
  `sbf-Cargo.lock` and comparing hashes is the reproducibility check.
- The generated `*-keypair.json` files from this build are throwaway
  build artifacts in a scratch directory — they are **not** program
  identities and **not** production keys. Production program keypairs
  are generated on the deployment host per DEPLOY-BRIDGE.md.
