#!/bin/bash
# start-validator.sh — boot the Machine single-node validator.
#
# Copies the shipped Machine genesis (ledger/) to a working ledger
# (ledger-test/), seeds the bootstrap validator keypairs, and starts
# solana-test-validator on RPC port 8899 (faucet on 9900).
#
# Usage:
#   ./start-validator.sh            # foreground (Ctrl-C to stop)
#   ./start-validator.sh --background   # background, logs to test-validator-stdout.log
#   ./start-validator.sh --fresh    # wipe the working ledger and start clean from the shipped genesis
#
# Notes:
# - This boots ONE validator (a local single-node testnet). It votes,
#   confirms, and finalizes. Multi-validator is a separate step
#   (see LAUNCH_CHECKLIST.md) and needs a host where the file-descriptor
#   limit can be raised to 1,000,000 and UDP send works.
# - User transfers need UDP send to work (RPC -> TPU forwarding). If
#   your container blocks UDP send, the chain will still vote and
#   finalize, but transfers will fail to confirm. See LAUNCH_CHECKLIST.md.
set -e
cd "$(dirname "$0")"

BACKGROUND=0
FRESH=0
for arg in "$@"; do
    case "$arg" in
        --background) BACKGROUND=1 ;;
        --fresh) FRESH=1 ;;
        *) echo "Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

[ -x bin/solana-test-validator ] || { echo "ERROR: bin/solana-test-validator not found. Run ./install.sh first." >&2; exit 1; }
[ -f ledger/genesis.bin ] || { echo "ERROR: ledger/genesis.bin not found. This repo ships the genesis in ledger/." >&2; exit 1; }
for k in founder vote stake faucet; do
    [ -f "keys/$k.json" ] || { echo "ERROR: keys/$k.json not found." >&2; exit 1; }
done

if [ "$FRESH" = 1 ]; then
    echo "Wiping working ledger..."
    rm -rf ledger-test
fi

if [ ! -f ledger-test/genesis.bin ]; then
    echo "Creating working ledger from shipped genesis..."
    rm -rf ledger-test
    cp -r ledger ledger-test
fi
# Complete an interrupted seeding: the keypair copies happen after
# the ledger copy, so a first run killed in between leaves a
# ledger-test that exists but has no keypairs — and the old
# directory-exists check then skipped seeding forever.
# Compare contents, not just existence: a copy interrupted mid-write
# leaves a truncated file that exists, and an existence check would
# strand that corrupt keypair forever.
cmp -s keys/founder.json ledger-test/validator-keypair.json || cp keys/founder.json ledger-test/validator-keypair.json
cmp -s keys/vote.json ledger-test/vote-account-keypair.json || cp keys/vote.json ledger-test/vote-account-keypair.json
cmp -s keys/stake.json ledger-test/stake-account-keypair.json || cp keys/stake.json ledger-test/stake-account-keypair.json
cmp -s keys/faucet.json ledger-test/faucet-keypair.json || cp keys/faucet.json ledger-test/faucet-keypair.json

echo "Starting Machine validator (genesis 32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb)..."
echo "  RPC: http://127.0.0.1:8899   Faucet: http://127.0.0.1:9900"
if [ "$BACKGROUND" = 1 ]; then
    nohup ./bin/solana-test-validator --ledger ledger-test --rpc-port 8899 --faucet-port 9900 > test-validator-stdout.log 2>&1 &
    VAL_PID=$!
    echo "Validator started in background (pid $VAL_PID). Log: test-validator-stdout.log"
    sleep 10
    if ! kill -0 "$VAL_PID" 2>/dev/null; then
        echo "ERROR: validator (pid $VAL_PID) exited within 10 seconds. Log: test-validator-stdout.log" >&2
        tail -5 test-validator-stdout.log >&2 || true
        exit 1
    fi
    tail -2 test-validator-stdout.log
else
    exec ./bin/solana-test-validator --ledger ledger-test --rpc-port 8899 --faucet-port 9900
fi
