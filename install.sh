#!/bin/bash
# install.sh — install the prebuilt Agave binaries for Machine.
#
# Downloads the Agave 2.3.13 prebuilt release from GitHub and extracts
# the binaries Machine needs into ./bin/. No source build, ever:
# a source build will not finish on a small VM (1.5 CPU / ~6 GB RAM).
# If the download fails, this script reports it plainly and stops —
# it never falls back to building from source.
#
# The downloaded tarball is verified against the pinned SHA-256 below
# before anything is extracted: these binaries generate the livenet
# keys and build the genesis (livenet/generate-keys.sh uses ./bin/),
# so an unverified download would be the root of trust for the whole
# network. The digest is the one GitHub records for the v2.3.13
# release asset (checked against the release metadata 2026-11-19).
set -e
cd "$(dirname "$0")"

AGAVE_VERSION="2.3.13"
AGAVE_COMMIT="5466f4592b1983adb13ba0a5d53f41ea2de69fba"
URL="https://github.com/anza-xyz/agave/releases/download/v${AGAVE_VERSION}/solana-release-x86_64-unknown-linux-gnu.tar.bz2"
TARBALL="solana-release-${AGAVE_VERSION}.tar.bz2"
TARBALL_SHA256="c43539ebdf6942472e8b87635d6ea55f428a51e3d0219f7b6f720fc6b19fade0"

NEEDED="solana solana-keygen solana-genesis solana-test-validator agave-validator agave-ledger-tool"

# Idempotent: skip only if ALL needed binaries are present and the
# main one reports the right version.
all_present=1
for b in $NEEDED; do
    [ -x "bin/$b" ] || all_present=0
done
if [ "$all_present" = 1 ]; then
    if bin/solana --version 2>/dev/null | grep -q "${AGAVE_VERSION}"; then
        echo "Agave ${AGAVE_VERSION} already installed in ./bin/ — nothing to do."
        exit 0
    fi
fi

echo "Downloading Agave ${AGAVE_VERSION} (prebuilt release)..."
echo "  ${URL}"
if command -v curl >/dev/null 2>&1; then
    curl --fail -L -o "${TARBALL}" "${URL}" || { echo "ERROR: download failed (curl). Check the URL and your network." >&2; exit 1; }
elif command -v wget >/dev/null 2>&1; then
    wget -O "${TARBALL}" "${URL}" || { echo "ERROR: download failed (wget). Check the URL and your network." >&2; exit 1; }
else
    echo "ERROR: need curl or wget to download the release." >&2
    exit 1
fi

echo "Verifying sha256..."
if ! echo "${TARBALL_SHA256}  ${TARBALL}" | sha256sum -c - >/dev/null 2>&1; then
    echo "ERROR: sha256 mismatch for ${TARBALL} — the download does not match the pinned Agave ${AGAVE_VERSION} release. Refusing to extract it." >&2
    echo "Expected sha256: ${TARBALL_SHA256}" >&2
    exit 1
fi
echo "  sha256 OK (${TARBALL_SHA256})"

echo "Extracting..."
mkdir -p bin
tar --no-same-owner -xjf "${TARBALL}" -C . --strip-components=1 solana-release/bin 2>/dev/null \
  || tar --no-same-owner -xjf "${TARBALL}"  # fallback: extract the whole tree, then copy
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
