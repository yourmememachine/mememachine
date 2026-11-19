#!/bin/bash
# build-sbf.sh — build the deployable SBF binaries for both bridge
# programs.
#
# Plain `cargo build` produces host libraries, not deployable
# programs (see bridge/README.md, Deploy). The deployable artifacts
# are produced by `cargo build-sbf`, which needs the Solana CLI
# toolchain (platform tools). This script checks for it, builds,
# and prints the artifacts with their sha256 hashes — record those
# hashes in the deployment notes; they are what an auditor and a
# verifier rebuild against.
#
# The build does NOT run against the bridge workspace directly: the
# platform-tools cargo (1.84) cannot parse the edition2024 manifests
# in the unified workspace graph, so that invocation fails before any
# program code compiles. This script implements the method recorded
# in SBF-BUILD-NOTES.md ("Method used (reproducible)"):
#   1. copy the three program crates into a scratch workspace,
#   2. strip their [dev-dependencies] (test-only; not part of the
#      deployable libraries, and the source of the edition2024 chain),
#   3. seed the scratch lockfile from the pinned sbf-Cargo.lock
#      beside this script (never the live workspace Cargo.lock),
#   4. cargo build-sbf both program manifests in the scratch tree,
#   5. copy the artifacts to bridge/target/deploy/ and hash them.
#
# Usage: ./bridge/deploy/build-sbf.sh
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BRIDGE_DIR="$(dirname "$SCRIPT_DIR")"
SCRATCH="$BRIDGE_DIR/target/sbf-scratch"
OUT_DIR="$BRIDGE_DIR/target/deploy"
CRATES="bridge-common solana-lock machine-bridge"

if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo not found. Install a Rust toolchain first." >&2
    exit 1
fi
if ! cargo build-sbf --help >/dev/null 2>&1; then
    echo "ERROR: 'cargo build-sbf' is unavailable. Install the Solana CLI" >&2
    echo "toolchain (Agave 2.3.13 release, matching the programs' SDK)" >&2
    echo "and make sure its cargo-build-sbf is on PATH, then re-run." >&2
    exit 1
fi
[ -f "$SCRIPT_DIR/sbf-Cargo.lock" ] || {
    echo "ERROR: $SCRIPT_DIR/sbf-Cargo.lock not found — the pinned SBF lockfile is required for a reproducible build." >&2
    exit 1
}

echo "Preparing scratch workspace: $SCRATCH"
rm -rf "$SCRATCH"
mkdir -p "$SCRATCH/crates"
for crate in $CRATES; do
    cp -r "$BRIDGE_DIR/crates/$crate" "$SCRATCH/crates/$crate"
done
cat > "$SCRATCH/Cargo.toml" <<'EOF'
[workspace]
resolver = "2"
members = [
    "crates/bridge-common",
    "crates/machine-bridge",
    "crates/solana-lock",
]
EOF
# Strip [dev-dependencies] sections from the copied manifests.
python3 - "$SCRATCH" <<'PYEOF'
import re
import sys
from pathlib import Path

for manifest in Path(sys.argv[1]).glob("crates/*/Cargo.toml"):
    lines = manifest.read_text().splitlines(keepends=True)
    out, skipping = [], False
    for line in lines:
        header = re.match(r"\s*\[([^\]]+)\]", line)
        if header:
            skipping = header.group(1).strip().startswith("dev-dependencies")
        if not skipping:
            out.append(line)
    manifest.write_text("".join(out))
PYEOF
cp "$SCRIPT_DIR/sbf-Cargo.lock" "$SCRATCH/Cargo.lock"

cargo build-sbf --manifest-path "$SCRATCH/crates/solana-lock/Cargo.toml"
cargo build-sbf --manifest-path "$SCRATCH/crates/machine-bridge/Cargo.toml"

mkdir -p "$OUT_DIR"
echo
echo "SBF artifacts:"
for so in solana_lock.so machine_bridge.so; do
    if [ -f "$SCRATCH/target/deploy/$so" ]; then
        cp "$SCRATCH/target/deploy/$so" "$OUT_DIR/$so"
        sha256sum "$OUT_DIR/$so"
    else
        echo "MISSING: $SCRATCH/target/deploy/$so (build reported success but the artifact is absent)" >&2
        exit 1
    fi
done
echo
echo "Next: bridge/deploy/DEPLOY-BRIDGE.md (prerequisites are not optional)."
