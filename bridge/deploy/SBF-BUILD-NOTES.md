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

**Rebuilt 2026-11-16 (later)** from the self-audit fixes
(`7cd6f5b`: Initialize bound to the upgrade authority,
create-or-adopt PDA creation, shared member-set validation,
LaunchGuard ceiling-division minimum lock, token-program pin —
see `STATUS.md` P25). Same standalone-tree method and saved
lockfiles as the earlier rebuilds. All three programs rebuilt in
this pass — including LaunchGuard, closing the rebuild left
outstanding above:

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `solana_lock.so` | 210,256 | `46c586131e0a95fb4652586eb49690918b0f76f840805db53ffd3ec5da5e5c68` |
| `machine_bridge.so` | 211,856 | `8e4501e25f3ef840f60ede5e5e05e3e9e2ecfd9a7f7547340ac2b58b440f1406` |
| `launch_guard.so` | 126,512 | `cda7112195249da03cedcc968561519a9a9898f2773d32e860f486611a86b92d` |

**Rebuilt 2026-11-17** from the LaunchGuard CloseVault change
(`8e84d18`: a fourth instruction — creator-only, gated on the
record's Released state — sweeps residual vault tokens to the
creator and closes the vault account; see the AUDIT.md scope
update). LaunchGuard only — the bridge programs were not
touched and their binaries above stand. Same standalone-tree
method (`~/workspace/sbf-lg/`, manifests stripped, saved
`programs/sbf-Cargo.lock` byte-identical to the tree's
lockfile). The build is deterministic: two consecutive builds
produced the identical hash.

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `launch_guard.so` | 135,488 | `915dc769e342b695dc11c37548184a1edff2e7534603410cb0bb787c8fb33788` |

**Rebuilt 2026-11-17** from the code-review debate round 1
change (`0efccbc`: attested-mint binding in Unlock, mint /
freeze authority check in Machine Initialize, vault owner and
state check in Lock Initialize, authority-PDA / vault
destination guards, Burn mint pin, config-PDA re-derivation in
every handler; see the AUDIT.md scope update). Bridge programs
only — LaunchGuard was not touched and its binary above stands.
Same standalone-tree method (`~/workspace/sbf-build/`, saved
`bridge/deploy/sbf-Cargo.lock`).

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `solana_lock.so` | 211,648 | `e0586b1948f553cb268a69137fe6849a1bc0889110d318d2ece5c61b07765814` |
| `machine_bridge.so` | 213,880 | `56d23895278069e078177765c09d3ef9da92e90199b4669581814c2fbb351cdb` |

**Rebuilt 2026-11-18** from the code-review debate round 3
change (`10840ba`: LaunchGuard's ExtendLock cap is measured
from the registration slot, so a lock's total duration can no
longer exceed the documented 5-year maximum; see the AUDIT.md
scope update). LaunchGuard only — the bridge programs were not
touched and their binaries above stand. Same standalone-tree
method (`~/workspace/sbf-lg/`, saved `programs/sbf-Cargo.lock`).
The build is deterministic: two consecutive builds produced
the identical hash.

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `launch_guard.so` | 135,416 | `9aceb092789c5afb593945ab002d3ece779326182a39a34e2b3aa8586f86cccf` |

**Rebuilt 2026-11-18** from the code-review debate round 4
change (`236257b`: `MachineConfig` gains `source_mint` and
`MintWrapped` binds `message.source_mint`; `validate_message`
bounds attestation lifetime at 1,000,000 slots on the source
basis; zero/impossible quorum and admin thresholds rejected in
the shared verifiers; the ed25519 instruction builder refuses
oversized sets; see the AUDIT.md scope update). Bridge programs
only — LaunchGuard was not touched and its binary above stands.
Same standalone-tree method (`~/workspace/sbf-build/`, saved
`bridge/deploy/sbf-Cargo.lock`).

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `solana_lock.so` | 211,744 | `a22bccd8ea47e59972ca1e45f0bec6a23809b45c51c35480252556e3bc8f71fc` |
| `machine_bridge.so` | 215,312 | `7d0ab41f4a6eb7f6ce2d58c021793c1ffc4a4438b95ffce9cabeba5c4ac8defb` |

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

## Addendum — PQ Vault (2026-11-19)

`pq_vault.so` was built with the same method (scratch tree
`~/workspace/sbf-pq`: the launch-guard and pq-vault crates with
`[dev-dependencies]` stripped, seeded from the programs
lockfile):

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `pq_vault.so` | 148,944 | `b6a52b936b9af8c9fb7b60dcf1664a1bfde2bb18e62333ae00ad0048ea9c74d7` |

Deterministic: two builds, identical hash. The scratch
lockfile is saved back as `programs/sbf-Cargo.lock`, which now
covers launch-guard and pq-vault; its only change from the
previously saved lockfile is the added `pq-vault` package
entry — every dependency pin is unchanged, so the recorded
launch_guard.so build is unaffected. The full pq-vault suite
also passes against this binary (17/17 with `BPF_OUT_DIR`
pointed at the deploy directory), and the measured compute
units for its signature-verifying instructions are recorded
in `programs/PQ-VERIFY.md`. Not deployed; the generated
keypair file is a throwaway build artifact, as above.
