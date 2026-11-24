#!/usr/bin/env bash
# build-from-source.sh — build the Machine validator binaries from
# patched Agave 2.3.13 source.
#
# What it does:
#   1. Downloads the Agave 2.3.13 source tarball from GitHub and verifies
#      its SHA-256 against the pinned value below.
#   2. Extracts it into BUILD_DIR (default: ./build/agave-2.3.13).
#   3. Applies every patch in ./patches/ (dry-run check first, then apply).
#   4. Builds the release binaries with the Rust toolchain pinned by the
#      source tree (rust-toolchain.toml: 1.86.0).
#   5. Copies the built binaries to ./bin-patched/ for inspection. It does
#      NOT touch ./bin/ and does NOT restart any running validator:
#      swapping in a patched binary is a separate, manual operator step.
#
# HONEST LIMIT: a full Agave source build was NOT run on the small VM this
# was written on (1.5 shared CPUs, ~8 GB RAM, no Rust 1.86.0 toolchain
# installed at the time). A full release build of Agave needs a much
# bigger machine (tens of GB of RAM and disk, and a long build — budget
# hours, not minutes) and a Rust 1.86.0 toolchain, which rustup will
# install on first run inside the source tree. Use --skip-build on a small
# machine to stop after the patches are applied and verified, and run the
# full build where the resources exist.
#
# Usage:
#   ./build-from-source.sh [--skip-build] [--skip-download] [--build-dir DIR]
#     --skip-build     Download + verify + apply patches, then stop.
#     --skip-download  Reuse an already-downloaded/extracted BUILD_DIR.
#     --build-dir DIR  Where to put the source tree (default ./build/agave-2.3.13).
#
# After a successful build, verify the fork identity in the new binary:
#   ./bin-patched/agave-validator --version
#   # expect: agave-validator 2.3.13 (src:...; feat:..., client:Machine)
# (The prebuilt binary in ./bin/ instead reports client:Agave.)
set -euo pipefail

AGAVE_VERSION="2.3.13"
AGAVE_TAG="v${AGAVE_VERSION}"
TARBALL_URL="https://codeload.github.com/anza-xyz/agave/tar.gz/refs/tags/${AGAVE_TAG}"
# SHA-256 of the tarball downloaded 2026-11-14. GitHub codeload tarballs
# for a fixed tag are deterministic; if this check fails, stop and inspect
# before proceeding — do not bypass it.
TARBALL_SHA256="35323fd76b6e78578e157c6fac854b0930f42d0b073ca659f10ddfddf0e6ace5"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCHES_DIR="${REPO_ROOT}/patches"
BUILD_DIR="${REPO_ROOT}/build/agave-2.3.13"
OUT_DIR="${REPO_ROOT}/bin-patched"
SKIP_BUILD=0
SKIP_DOWNLOAD=0

while [ $# -gt 0 ]; do
    case "$1" in
        --skip-build) SKIP_BUILD=1; shift ;;
        --skip-download) SKIP_DOWNLOAD=1; shift ;;
        --build-dir) BUILD_DIR="$2"; shift 2 ;;
        --build-dir=*) BUILD_DIR="${1#--build-dir=}"; shift ;;
        -h|--help)
            sed -n '2,30p' "${BASH_SOURCE[0]}"
            exit 0 ;;
        *)
            echo "error: unknown argument: $1" >&2
            exit 2 ;;
    esac
done

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: required tool '$1' not found in PATH" >&2
        exit 1
    }
}
need curl
need tar
need patch
need sha256sum
[ "$SKIP_BUILD" -eq 1 ] || need cargo

if [ ! -d "$PATCHES_DIR" ]; then
    echo "error: patches directory not found: $PATCHES_DIR" >&2
    exit 1
fi
if ! ls "$PATCHES_DIR"/*.patch >/dev/null 2>&1; then
    echo "error: no *.patch files in $PATCHES_DIR" >&2
    exit 1
fi

SRC_DIR="${BUILD_DIR}/agave-${AGAVE_VERSION}"

if [ "$SKIP_DOWNLOAD" -eq 0 ]; then
    mkdir -p "$BUILD_DIR"
    TARBALL="${BUILD_DIR}/agave-${AGAVE_VERSION}.tar.gz"
    echo "==> Downloading Agave ${AGAVE_VERSION} source tarball..."
    curl --fail -sSL -o "$TARBALL" "$TARBALL_URL"
    echo "==> Verifying SHA-256..."
    echo "${TARBALL_SHA256}  ${TARBALL}" | sha256sum -c -
    echo "==> Extracting to ${BUILD_DIR}..."
    # Extract into a fresh tree: re-extracting over a patched tree
    # would leave patch-created files behind in a mixed state.
    rm -rf "$SRC_DIR"
    tar --no-same-owner -xzf "$TARBALL" -C "$BUILD_DIR"
else
    echo "==> --skip-download: reusing existing source tree at ${SRC_DIR}"
fi

if [ ! -d "$SRC_DIR" ]; then
    echo "error: source tree not found: $SRC_DIR" >&2
    echo "       (run without --skip-download, or point --build-dir at the extracted tree)" >&2
    exit 1
fi

echo "==> Applying patches from ${PATCHES_DIR}..."
for patch_file in "$PATCHES_DIR"/*.patch; do
    name="$(basename "$patch_file")"
    echo "--- ${name}: dry-run check"
    if (cd "$SRC_DIR" && patch --dry-run -p1 < "$patch_file") >/dev/null 2>&1; then
        echo "--- ${name}: applying"
        (cd "$SRC_DIR" && patch -p1 < "$patch_file")
    elif (cd "$SRC_DIR" && patch --dry-run -R -p1 < "$patch_file") >/dev/null 2>&1; then
        # A re-run after an interrupted first run: this patch is
        # already fully applied, so skipping it is the resume path.
        echo "--- ${name}: already applied, skipping (resume)"
    else
        echo "error: patch $name neither applies nor is already applied" >&2
        exit 1
    fi
done
echo "==> All patches applied cleanly."

if [ "$SKIP_BUILD" -eq 1 ]; then
    echo "==> --skip-build: stopping after patching. Patched source is at:"
    echo "    ${SRC_DIR}"
    exit 0
fi

# The source tree pins its toolchain in rust-toolchain.toml (1.86.0);
# rustup installs it on first use inside the tree. The build needs a
# large machine: tens of GB of free disk for target/ plus the RAM to link
# the validator. Do not run this on a small VM and expect it to finish.
echo "==> Building release binaries (this takes a long time)..."
(cd "$SRC_DIR" && cargo build --release \
    -p agave-validator \
    -p solana-genesis \
    -p solana-keygen \
    -p solana-cli \
    -p solana-test-validator \
    -p agave-ledger-tool)

mkdir -p "$OUT_DIR"
for bin in agave-validator solana-genesis solana-keygen solana agave-ledger-tool solana-test-validator; do
    cp "${SRC_DIR}/target/release/${bin}" "${OUT_DIR}/${bin}"
done

echo "==> Built binaries staged in ${OUT_DIR}:"
ls -l "$OUT_DIR"
echo "==> Fork identity check:"
"${OUT_DIR}/agave-validator" --version
echo "    (expect client:Machine; the prebuilt ./bin/ binary reports client:Agave)"
echo "==> Done. ./bin/ was not touched; swapping binaries is a manual step."
