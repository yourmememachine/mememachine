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
# Usage: ./bridge/deploy/build-sbf.sh
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BRIDGE_DIR="$(dirname "$SCRIPT_DIR")"

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

cd "$BRIDGE_DIR"
cargo build-sbf --manifest-path crates/solana-lock/Cargo.toml
cargo build-sbf --manifest-path crates/machine-bridge/Cargo.toml

echo
echo "SBF artifacts:"
for so in target/deploy/solana_lock.so target/deploy/machine_bridge.so; do
    if [ -f "$so" ]; then
        sha256sum "$so"
    else
        echo "MISSING: $so (build reported success but the artifact is absent)" >&2
        exit 1
    fi
done
echo
echo "Next: bridge/deploy/DEPLOY-BRIDGE.md (prerequisites are not optional)."
