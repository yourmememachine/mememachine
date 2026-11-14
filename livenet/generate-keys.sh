#!/bin/bash
# generate-keys.sh — generate fresh keypairs for the MemeMachine livenet.
#
# Creates, under livenet/keys/:
#   v1-identity.json, v1-vote.json, v1-stake.json
#   v2-identity.json, v2-vote.json, v2-stake.json
#   v3-identity.json, v3-vote.json, v3-stake.json
#   faucet.json, treasury.json
#
# These are the LIVE keys. They are generated ON the deployment host and
# must never be copied into the git repo. The dev keypairs in the repo
# root (keys/) are throwaway test keys for the localnet — never use them
# for the livenet.
#
# Usage:
#   ./generate-keys.sh           # generate (refuses if keys already exist)
#   ./generate-keys.sh --force   # delete and regenerate (DANGEROUS)
#
# Env override (used for testing): AGAVE_BIN=/path/to/bin
set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
AGAVE_BIN="${AGAVE_BIN:-$ROOT_DIR/bin}"
KEYS_DIR="$SCRIPT_DIR/keys"

FORCE=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=1 ;;
        *) echo "Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

[ -x "$AGAVE_BIN/solana-keygen" ] || { echo "ERROR: $AGAVE_BIN/solana-keygen not found. Run ./install.sh from the repo root first." >&2; exit 1; }

if [ -d "$KEYS_DIR" ] && [ "$(ls -A "$KEYS_DIR" 2>/dev/null)" ]; then
    if [ "$FORCE" = 0 ]; then
        echo "ERROR: $KEYS_DIR already has keypairs. Refusing to overwrite." >&2
        echo "Back them up first, or re-run with --force to delete and regenerate." >&2
        exit 1
    fi
    echo "WARNING: --force given. Deleting existing keypairs in $KEYS_DIR ..."
    rm -rf "$KEYS_DIR"
fi
mkdir -p "$KEYS_DIR"
chmod 700 "$KEYS_DIR"

for n in 1 2 3; do
    for role in identity vote stake; do
        "$AGAVE_BIN/solana-keygen" new --no-bip39-passphrase -s \
            -o "$KEYS_DIR/v$n-$role.json" >/dev/null
    done
done
"$AGAVE_BIN/solana-keygen" new --no-bip39-passphrase -s -o "$KEYS_DIR/faucet.json" >/dev/null
"$AGAVE_BIN/solana-keygen" new --no-bip39-passphrase -s -o "$KEYS_DIR/treasury.json" >/dev/null
chmod 600 "$KEYS_DIR"/*.json

echo "Generated keypairs in $KEYS_DIR:"
for f in "$KEYS_DIR"/*.json; do
    echo "  $(basename "$f"): $("$AGAVE_BIN/solana-keygen" pubkey "$f")"
done
echo
echo "BACK THESE UP NOW. Lose the treasury key and the 993,999,000 MACHINE is gone."
echo "Do not commit this directory to git."
