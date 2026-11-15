#!/bin/bash
# install.sh — install the prebuilt Agave binaries for MemeMachine.
#
# Downloads the Agave 2.3.13 prebuilt release from GitHub and extracts
# the binaries MemeMachine needs into ./bin/. No source build, ever:
# a source build will not finish on a small VM (1.5 CPU / ~6 GB RAM).
# If the download fails, this script reports it plainly and stops —
# it never falls back to building from source.
set -e
cd "$(dirname "$0")"

AGAVE_VERSION="2.3.13"
AGAVE_COMMIT="5466f4592b1983adb13ba0a5d53f41ea2de69fba"
URL="https://github.com/anza-xyz/agave/releases/download/v${AGAVE_VERSION}/solana-release-x86_64-unknown-linux-gnu.tar.bz2"
TARBALL="solana-release-${AGAVE_VERSION}.tar.bz2"

NEEDED="solana solana-keygen solana-genesis solana-test-validator agave-validator agave-ledger-tool"

# Idempotent: skip if all binaries are present and report the right version.
if [ -x bin/solana ] && [ -x bin/solana-test-validator ] && [ -x bin/agave-validator ]; then
    if bin/solana --version 2>/dev/null | grep -q "${AGAVE_VERSION}"; then
        echo "Agave ${AGAVE_VERSION} already installed in ./bin/ — nothing to do."
        exit 0
    fi
fi

echo "Downloading Agave ${AGAVE_VERSION} (prebuilt release)..."
echo "  ${URL}"
if command -v curl >/dev/null 2>&1; then
    curl -L -o "${TARBALL}" "${URL}" || { echo "ERROR: download failed (curl). Check the URL and your network." >&2; exit 1; }
elif command -v wget >/dev/null 2>&1; then
    wget -O "${TARBALL}" "${URL}" || { echo "ERROR: download failed (wget). Check the URL and your network." >&2; exit 1; }
else
    echo "ERROR: need curl or wget to download the release." >&2
    exit 1
fi

echo "Extracting..."
mkdir -p bin
tar -xjf "${TARBALL}" -C . --strip-components=1 solana-release/bin 2>/dev/null \
  || tar -xjf "${TARBALL}"  # fallback: extract the whole tree, then copy
# Normalise: make sure the binaries we need are in ./bin/
if [ -d solana-release/bin ]; then
    cp solana-release/bin/* bin/ 2>/dev/null || true
fi
for b in $NEEDED; do
    if [ ! -x "bin/$b" ]; then
        echo "ERROR: binary bin/$b missing after extraction." >&2
        exit 1
    fi
done
chmod +x bin/*

echo "Verifying..."
bin/solana --version
bin/solana-test-validator --version 2>/dev/null | head -1 || true
echo "Agave ${AGAVE_VERSION} (commit ${AGAVE_COMMIT}) installed in ./bin/"
echo "Binaries: $NEEDED"
