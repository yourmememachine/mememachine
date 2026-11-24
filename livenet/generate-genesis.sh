#!/bin/bash
# generate-genesis.sh — build the Machine 3-validator livenet genesis.
#
# Creates livenet/ledger/ (genesis.bin + genesis.tar.bz2 + blockstore)
# and livenet/genesis-info.env (genesis hash, shred version, validator
# pubkeys) from the keypairs in livenet/keys/. Runs generate-keys.sh
# first if no keys exist.
#
# This is a NEW genesis with a NEW hash — the localnet genesis
# (32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb) has one bootstrap
# validator baked in, and bootstrap validators can only be set at
# genesis creation. Everything else matches the localnet: Agave 2.3.13,
# 1,000,000,000 MACHINE, the same fee schedule and timing.
#
# Distribution (1,000,000,000 MACHINE) — balanced livenet allocation.
# No single holder is near the whole supply. The five primordial
# accounts below are separate keypairs (see generate-keys.sh), so the
# split is on-chain at genesis, not just documented:
#   founder / team (vested)  150,000,000  (15.0000%)
#   ecosystem fund            250,000,000  (25.0000%)
#   community / airdrop       200,000,000  (20.0000%)
#   validator rewards reserve 150,000,000  (15.0000%)
#   treasury / operations     243,999,000  (24.3999%)
#   each validator id         1,000,000 x3 (bootstrap-validator-lamports)
#   each validator stake      1,000,000 x3 (bootstrap-validator-stake-lamports)
#   faucet                    1,000
# Primordial subtotal 993,999,000 + validator 6,000,000 + faucet 1,000
# = 1,000,000,000 exactly. Vesting terms for the founder/team bucket
# are in WHITEPAPER.md Section 3. This replaces the old livenet split
# (treasury 993,999,000) and has nothing to do with the localnet dev
# genesis, whose founder-weighted distribution is throwaway (see the
# note at the end of WHITEPAPER.md Section 3).
#
# Notes on the flags (verified against Agave 2.3.13 on 2026-11-14):
# - --target-tick-duration takes MICROSECONDS in this build, despite the
#   help text saying MILLIS. 6250 gives the localnet's 6.25 ms tick.
# - --primordial-accounts-file is a YAML map of pubkey -> Base64Account
#   {balance, owner, data, executable}. The owner is the System Program
#   (11111111111111111111111111111111) for a plain SOL-holding account.
# - No vote-authority patch is needed: agave-validator votes with the
#   identity keypair, which is what solana-genesis sets as the authorized
#   voter. (The patch in patch-vote-authority.py is only for
#   solana-test-validator.)
#
# Usage:
#   ./generate-genesis.sh           # generate (refuses if ledger/ exists)
#   ./generate-genesis.sh --force   # delete and regenerate (DANGEROUS)
#
# Env override (used for testing): AGAVE_BIN=/path/to/bin
set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
AGAVE_BIN="${AGAVE_BIN:-$ROOT_DIR/bin}"
KEYS_DIR="$SCRIPT_DIR/keys"
LEDGER_DIR="$SCRIPT_DIR/ledger"
INFO_FILE="$SCRIPT_DIR/genesis-info.env"

FORCE=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=1 ;;
        *) echo "Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

[ -x "$AGAVE_BIN/solana-genesis" ] || { echo "ERROR: $AGAVE_BIN/solana-genesis not found. Run ./install.sh from the repo root first." >&2; exit 1; }
[ -x "$AGAVE_BIN/solana-keygen" ] || { echo "ERROR: $AGAVE_BIN/solana-keygen not found." >&2; exit 1; }
[ -x "$AGAVE_BIN/agave-ledger-tool" ] || { echo "ERROR: $AGAVE_BIN/agave-ledger-tool not found. It reads back the genesis hash and shred version below; without it this script would write an empty network identity." >&2; exit 1; }

if [ ! -d "$KEYS_DIR" ] || [ -z "$(ls -A "$KEYS_DIR" 2>/dev/null)" ]; then
    echo "No keypairs found — generating fresh ones..."
    "$SCRIPT_DIR/generate-keys.sh"
fi
for f in v1-identity v1-vote v1-stake v2-identity v2-vote v2-stake \
         v3-identity v3-vote v3-stake faucet treasury founder ecosystem \
         community validator-rewards; do
    [ -f "$KEYS_DIR/$f.json" ] || { echo "ERROR: missing keypair $KEYS_DIR/$f.json" >&2; exit 1; }
done

if [ -d "$LEDGER_DIR" ]; then
    if [ "$FORCE" = 0 ]; then
        echo "ERROR: $LEDGER_DIR already exists. Refusing to overwrite an existing genesis." >&2
        echo "Re-run with --force to delete and regenerate (the genesis hash WILL change)." >&2
        exit 1
    fi
    echo "WARNING: --force given. Deleting existing genesis in $LEDGER_DIR ..."
    rm -rf "$LEDGER_DIR"
    # The old genesis-info.env describes the genesis just deleted;
    # remove it so no later step can read a stale network identity
    # if this run fails before writing the new one.
    rm -f "$INFO_FILE"
fi

pubkey() { "$AGAVE_BIN/solana-keygen" pubkey "$KEYS_DIR/$1.json"; }

V1I=$(pubkey v1-identity); V1V=$(pubkey v1-vote); V1S=$(pubkey v1-stake)
V2I=$(pubkey v2-identity); V2V=$(pubkey v2-vote); V2S=$(pubkey v2-stake)
V3I=$(pubkey v3-identity); V3V=$(pubkey v3-vote); V3S=$(pubkey v3-stake)
FAUCET=$(pubkey faucet)
TREASURY=$(pubkey treasury)
FOUNDER=$(pubkey founder)
ECOSYSTEM=$(pubkey ecosystem)
COMMUNITY=$(pubkey community)
VALIDATOR_REWARDS=$(pubkey validator-rewards)

# All 15 pubkeys must be pairwise distinct: keys are copied between
# hosts by design, and a copy error that duplicates one file over
# another would otherwise silently merge two primordial allocations
# (duplicate YAML key) or make two bootstrap validators share one
# identity in an irreversible genesis.
ALL_KEYS="$V1I $V1V $V1S $V2I $V2V $V2S $V3I $V3V $V3S $FAUCET $TREASURY $FOUNDER $ECOSYSTEM $COMMUNITY $VALIDATOR_REWARDS"
if [ "$(printf '%s\n' $ALL_KEYS | sort -u | wc -l)" -ne 15 ]; then
    echo "ERROR: the 15 keypair pubkeys are not pairwise distinct — a keypair file was likely copied over another. Refusing to build a genesis with merged allocations or duplicate validator identities." >&2
    exit 1
fi

# Primordial allocation accounts (1 MACHINE = 1,000,000,000 lamports).
# Subtotal 993,999,000 MACHINE = 993,999,000,000,000,000 lamports.
PRIMORDIAL="$SCRIPT_DIR/.primordial.yaml"
cat > "$PRIMORDIAL" <<EOF
$FOUNDER:
  balance: 150000000000000000
  owner: 11111111111111111111111111111111
  data: ""
  executable: false
$ECOSYSTEM:
  balance: 250000000000000000
  owner: 11111111111111111111111111111111
  data: ""
  executable: false
$COMMUNITY:
  balance: 200000000000000000
  owner: 11111111111111111111111111111111
  data: ""
  executable: false
$VALIDATOR_REWARDS:
  balance: 150000000000000000
  owner: 11111111111111111111111111111111
  data: ""
  executable: false
$TREASURY:
  balance: 243999000000000000
  owner: 11111111111111111111111111111111
  data: ""
  executable: false
EOF

echo "Building 3-validator genesis..."
"$AGAVE_BIN/solana-genesis" \
    --ledger "$LEDGER_DIR" \
    --cluster-type development \
    --hashes-per-tick sleep \
    --ticks-per-slot 64 \
    --target-tick-duration 6250 \
    --slots-per-epoch 8192 \
    --enable-warmup-epochs \
    --inflation none \
    --fee-burn-percentage 50 \
    --target-lamports-per-signature 10000 \
    --target-signatures-per-slot 20000 \
    --lamports-per-byte-year 3480 \
    --rent-burn-percentage 50 \
    --rent-exemption-threshold 2 \
    --vote-commission-percentage 100 \
    --bootstrap-validator "$V1I" "$V1V" "$V1S" \
    --bootstrap-validator "$V2I" "$V2V" "$V2S" \
    --bootstrap-validator "$V3I" "$V3V" "$V3S" \
    --bootstrap-validator-lamports 1000000000000000 \
    --bootstrap-validator-stake-lamports 1000000000000000 \
    --faucet-pubkey "$FAUCET" \
    --faucet-lamports 1000000000000 \
    --primordial-accounts-file "$PRIMORDIAL"

GENESIS_HASH=$("$AGAVE_BIN/agave-ledger-tool" --ledger "$LEDGER_DIR" genesis-hash 2>/dev/null | tail -1)
SHRED_VERSION=$("$AGAVE_BIN/agave-ledger-tool" --ledger "$LEDGER_DIR" genesis 2>/dev/null | awk '/Shred version:/ {print $3}')

# These two values ARE the network identity: start-validator.sh feeds
# them to --expected-genesis-hash / --expected-shred-version and the
# info file is shared with anyone joining. A failed ledger-tool read
# must never be written down as an empty identity with a success
# message — the pipelines above hide the failure (their exit status
# is tail's / awk's), so check the values themselves.
# Format check, not just non-empty: the hash read above is `tail -1`
# of ledger-tool output, which could capture a warning line instead.
if ! printf '%s' "$GENESIS_HASH" | grep -Eq '^[1-9A-HJ-NP-Za-km-z]{32,44}$' || ! printf '%s' "$SHRED_VERSION" | grep -Eq '^[0-9]+$'; then
    echo "ERROR: genesis hash / shred version have unexpected formats (hash='$GENESIS_HASH' shred='$SHRED_VERSION'); refusing to write $INFO_FILE." >&2
    exit 1
fi
if [ -z "$GENESIS_HASH" ] || [ -z "$SHRED_VERSION" ]; then
    echo "ERROR: could not read the genesis hash / shred version back from $LEDGER_DIR (got hash='$GENESIS_HASH' shred='$SHRED_VERSION')." >&2
    echo "The genesis was built but its identity is unknown; refusing to write $INFO_FILE. Investigate agave-ledger-tool before re-running." >&2
    exit 1
fi

cat > "$INFO_FILE" <<EOF
# Machine livenet genesis info — written by generate-genesis.sh.
# Share the GENESIS_HASH and SHRED_VERSION with anyone joining the network.
GENESIS_HASH=$GENESIS_HASH
SHRED_VERSION=$SHRED_VERSION
V1_IDENTITY=$V1I
V1_VOTE=$V1V
V1_STAKE=$V1S
V2_IDENTITY=$V2I
V2_VOTE=$V2V
V2_STAKE=$V2S
V3_IDENTITY=$V3I
V3_VOTE=$V3V
V3_STAKE=$V3S
FAUCET=$FAUCET
TREASURY=$TREASURY
FOUNDER=$FOUNDER
ECOSYSTEM=$ECOSYSTEM
COMMUNITY=$COMMUNITY
VALIDATOR_REWARDS=$VALIDATOR_REWARDS
EOF

rm -f "$PRIMORDIAL"
echo
echo "Genesis built: $LEDGER_DIR"
echo "  Genesis hash:  $GENESIS_HASH"
echo "  Shred version: $SHRED_VERSION"
echo "  Info written to: $INFO_FILE"
echo "This hash is the livenet identity. It is NOT the localnet hash."
