# Source patches — Machine fork of Agave 2.3.13

This directory holds the source-level patch set that makes Machine a
deeper fork than "stock Agave binaries + custom genesis". Each patch is a
unified diff against the real Agave 2.3.13 source
(`anza-xyz/agave`, tag `v2.3.13`), with a header at the top of the file
explaining what it does and why. Nothing here is pushed anywhere; this is
a local patch set applied by `../build-from-source.sh`.

## The patches

| Patch | File | Change |
|---|---|---|
| `01-client-id.patch` | `version/src/lib.rs` | New client id 4, `Machine`, reported as the default client. `--version` output becomes `... client:Machine` instead of `... client:Agave`. Gossip identity label only. |
| `02-snapshot-interval.patch` | `runtime/src/snapshot_bank_utils.rs` | Default incremental snapshot interval `100` -> `500` slots. Operational default only. |
| `03-startup-banner.patch` | `validator/src/commands/run/execute.rs` | One startup log line: `Chain: Machine (Agave 2.3.13 fork)`. Log line only. |

## What was deliberately NOT changed

No consensus-critical parameter was touched: no ticks per slot, slots per
epoch, vote or shred rules, fee schedule, rent, or bank-hash computation.
The fee schedule and supply live in genesis (`../GENESIS.md`), not in
source, and stay there. The full-snapshot interval (50,000 slots) is
unchanged. Patch 01's client id is a gossip identity label: stock Agave
nodes decode id 4 as `Unknown(4)`, which is expected — this chain runs its
own genesis and shred version (35836) and does not peer with stock Agave
clusters.

## Verification (done 2026-11-14, this VM)

Source obtained as the v2.3.13 source tarball from GitHub:

```
curl -sSL -o /tmp/agave-2.3.13.tar.gz \
  https://codeload.github.com/anza-xyz/agave/tar.gz/refs/tags/v2.3.13
sha256: 35323fd76b6e78578e157c6fac854b0930f42d0b073ca659f10ddfddf0e6ace5
```

Extracted to `/tmp/agave-2.3.13` (49 MB on disk), copied to a scratch
tree `/tmp/agave-verify`, and each patch was checked two ways:

```
cd /tmp/agave-verify
patch --dry-run -p1 < patches/01-client-id.patch        # exit 0, "checking file version/src/lib.rs"
patch --dry-run -p1 < patches/02-snapshot-interval.patch # exit 0, "checking file runtime/src/snapshot_bank_utils.rs"
patch --dry-run -p1 < patches/03-startup-banner.patch  # exit 0, "checking file validator/src/commands/run/execute.rs"
git apply --check patches/01-client-id.patch           # exit 0
git apply --check patches/02-snapshot-interval.patch  # exit 0
git apply --check patches/03-startup-banner.patch     # exit 0
```

Then all three were applied for real with `git apply` in that scratch
tree, and each resulting file was byte-identical to the intended patched
file (`diff` reported `identical` for all three). `git diff --stat` on the
scratch tree: 3 files changed, 34 insertions, 6 deletions.

Stronger check for patch 01 — compile + unit test of the touched crate
(stable Rust 1.100.0 on this VM, forced with `RUSTUP_TOOLCHAIN=stable`
because the pinned 1.86.0 toolchain is not installed here):

```
cd /tmp/agave-verify
cargo test -p solana-version --lib
# test test::test_client_id ... ok
# test test::test_compute_commit ... ok
# test result: ok. 2 passed; 0 failed
```

All three patched files also parse as valid Rust
(`rustfmt --edition 2021 <file>`, exit 0 for each).

## What was NOT verified

- The full Agave release build was **not** run on this VM
  (1.5 shared CPUs, ~8 GB RAM, no Rust 1.86.0 toolchain installed; a full
  Agave build needs a much bigger machine and a long build). Patches 02
  and 03 were therefore verified by clean application plus a syntax
  parse, not by compiling the `solana-runtime` or `agave-validator`
  crates. Patch 02 is a single same-type constant change
  (`100` -> `500`, both `Slot`), and the only in-repo validity assertion
  on it (`is_snapshot_config_valid` in `core/src/validator.rs`) still
  holds because 50,000 > 500. Patch 03 is a single `info!()` line with a
  string literal, in a function that already logs the same way.
- No patched binary has been built or run. The validator currently
  running is the prebuilt stock Agave 2.3.13 binary (see `../FORK.md`).
  The patched build is the path forward, not the running code.

## Applying by hand

```
tar -xzf agave-2.3.13.tar.gz   # or: git clone --branch v2.3.13 --depth 1 https://github.com/anza-xyz/agave.git
cd agave-2.3.13
patch -p1 < /path/to/patches/01-client-id.patch
patch -p1 < /path/to/patches/02-snapshot-interval.patch
patch -p1 < /path/to/patches/03-startup-banner.patch
```

Or run `../build-from-source.sh`, which does the download, the
checksum, the patching, and the build.
