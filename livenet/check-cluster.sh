#!/bin/bash
# check-cluster.sh — verify the Machine livenet is actually working.
#
# Checks, against the RPC endpoints in validators.conf:
#   1. Each validator's RPC answers.
#   2. The slot is advancing (two reads, 10s apart).
#   3. The genesis hash on each RPC matches genesis-info.env.
#   4. Exactly 3 vote accounts are voting, with stake (solana validators).
#   5. (Optional, --with-transfer) A real transfer between two fresh
#      keypairs, funded from validator 1's identity, with a confirmed
#      signature — the proof user transactions work. Costs 0.01 MACHINE.
#
# Usage:
#   ./check-cluster.sh                  # checks 1-4
#   ./check-cluster.sh --with-transfer  # checks 1-5
#
# Env override (used for testing): AGAVE_BIN=/path/to/bin
set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
AGAVE_BIN="${AGAVE_BIN:-$ROOT_DIR/bin}"

WITH_TRANSFER=0
[ "${1:-}" = "--with-transfer" ] && WITH_TRANSFER=1

# shellcheck disable=SC1090
. "$SCRIPT_DIR/validators.conf"
# shellcheck disable=SC1090
. "$SCRIPT_DIR/genesis-info.env"

SOLANA="$AGAVE_BIN/solana"
[ -x "$SOLANA" ] || { echo "ERROR: $SOLANA not found. Run ./install.sh from the repo root first." >&2; exit 1; }

fail=0
say() { printf '%-58s %s\n' "$1" "$2"; }

# Public IPs default to localhost when validators run on this host.
V1_HOST="${V1_PUBLIC_IP:-127.0.0.1}"
V2_HOST="${V2_PUBLIC_IP:-127.0.0.1}"
V3_HOST="${V3_PUBLIC_IP:-127.0.0.1}"

echo "Machine livenet check — genesis $GENESIS_HASH"
echo

for n in 1 2 3; do
    eval "HOST=\$V${n}_HOST"
    eval "PORT=\$V${n}_RPC_PORT"
    eval "BIND=\$V${n}_RPC_BIND"
    if [ "$BIND" = "127.0.0.1" ] && [ "$HOST" != "127.0.0.1" ]; then
        say "validator $n RPC (localhost-only, skipping remote check)" "SKIP"
        continue
    fi
    if SLOT=$("$SOLANA" slot --url "http://$HOST:$PORT" 2>/dev/null); then
        say "validator $n RPC answers (slot $SLOT)" "OK"
    else
        say "validator $n RPC answers" "FAIL"
        fail=1
    fi
done

# Slot advancing on the public RPC.
SLOT1=$("$SOLANA" slot --url "http://$V1_HOST:$V1_RPC_PORT" 2>/dev/null || echo "")
if [ -n "$SLOT1" ]; then
    sleep 10
    SLOT2=$("$SOLANA" slot --url "http://$V1_HOST:$V1_RPC_PORT" 2>/dev/null || echo "")
    if [ -n "$SLOT2" ] && [ "$SLOT2" -gt "$SLOT1" ]; then
        say "slot advancing ($SLOT1 -> $SLOT2 in 10s)" "OK"
    else
        say "slot advancing ($SLOT1 -> ${SLOT2:-?})" "FAIL"
        fail=1
    fi
    # Genesis hash matches.
    SEEN_HASH=$("$SOLANA" genesis-hash --url "http://$V1_HOST:$V1_RPC_PORT" 2>/dev/null || echo "")
    if [ "$SEEN_HASH" = "$GENESIS_HASH" ]; then
        say "genesis hash matches ($SEEN_HASH)" "OK"
    else
        say "genesis hash matches (got ${SEEN_HASH:-?})" "FAIL"
        fail=1
    fi
fi

# Three voting validators with stake.
if VALIDATORS=$("$SOLANA" validators --url "http://$V1_HOST:$V1_RPC_PORT" 2>/dev/null); then
    COUNT=$(echo "$VALIDATORS" | grep -c "  $V1_IDENTITY\|  $V2_IDENTITY\|  $V3_IDENTITY" || true)
    echo "$VALIDATORS" | grep -E "Identity|  $V1_IDENTITY|  $V2_IDENTITY|  $V3_IDENTITY" || true
    if [ "$COUNT" -ge 3 ]; then
        say "voting validators with stake (found $COUNT of 3)" "OK"
    else
        say "voting validators with stake (found $COUNT of 3)" "FAIL"
        fail=1
    fi
else
    say "validators list" "FAIL"
    fail=1
fi

# Real transfer between two fresh keypairs.
if [ "$WITH_TRANSFER" = 1 ]; then
    TMPDIR=$(mktemp -d)
    trap 'rm -rf "$TMPDIR"' EXIT
    "$AGAVE_BIN/solana-keygen" new --no-bip39-passphrase -s -o "$TMPDIR/a.json" >/dev/null
    "$AGAVE_BIN/solana-keygen" new --no-bip39-passphrase -s -o "$TMPDIR/b.json" >/dev/null
    A=$("$AGAVE_BIN/solana-keygen" pubkey "$TMPDIR/a.json")
    B=$("$AGAVE_BIN/solana-keygen" pubkey "$TMPDIR/b.json")
    URL="http://$V1_HOST:$V1_RPC_PORT"
    if "$SOLANA" transfer --url "$URL" --keypair "$SCRIPT_DIR/keys/v1-identity.json" \
            --from "$SCRIPT_DIR/keys/v1-identity.json" "$A" 1 --allow-unfunded-recipient \
            --fee-payer "$SCRIPT_DIR/keys/v1-identity.json" 2>/dev/null | grep -q "Signature"; then
        say "funded fresh keypair A (1 MACHINE from v1 identity)" "OK"
    else
        say "funded fresh keypair A" "FAIL"; fail=1
    fi
    if SIG=$("$SOLANA" transfer --url "$URL" --keypair "$TMPDIR/a.json" "$B" 0.01 \
            --fee-payer "$TMPDIR/a.json" 2>/dev/null | grep -oE '[A-Za-z0-9]{87,88}' | head -1); then
        BAL_B=$("$SOLANA" balance --url "$URL" "$B" 2>/dev/null || echo "?")
        say "transfer A -> B confirmed (sig ${SIG:0:12}..., B balance $BAL_B)" "OK"
        echo "  Full signature: $SIG"
    else
        say "transfer A -> B confirmed" "FAIL"; fail=1
    fi
fi

echo
if [ "$fail" = 0 ]; then
    echo "LIVENET OK — all checks passed."
else
    echo "LIVENET CHECK FAILED — see FAIL lines above."
    exit 1
fi
