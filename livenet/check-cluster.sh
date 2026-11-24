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
#      signature — the proof user transactions work. Costs 0.02 MACHINE
#      from validator 1's identity: 0.01 reaches keypair B, the rest is
#      fees plus the remainder stranded in keypair A, whose keypair
#      file is deleted on exit.
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
for arg in "$@"; do
    case "$arg" in
        --with-transfer) WITH_TRANSFER=1 ;;
        *) echo "Unknown argument: $arg" >&2
           echo "Usage: $0 [--with-transfer]" >&2
           exit 1 ;;
    esac
done

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
        # Genesis hash is checked on EVERY validator's RPC, not only
        # validator 1's: a node booted from a stale/wrong ledger is
        # exactly the split-genesis failure this gate exists to catch,
        # and it is never validator 1 (the bootstrap node).
        SEEN=$("$SOLANA" genesis-hash --url "http://$HOST:$PORT" 2>/dev/null || echo "")
        if [ "$SEEN" = "$GENESIS_HASH" ]; then
            say "validator $n genesis hash matches" "OK"
        else
            say "validator $n genesis hash matches (got ${SEEN:-?})" "FAIL"
            fail=1
        fi
    else
        say "validator $n RPC answers" "FAIL"
        fail=1
    fi
done

# Validator 1's later checks (slot advancing, validators list,
# transfer) query its public address; apply the same localhost-bind
# skip there, so a documented localhost rebind (ops/PUBLIC-RPC.md)
# does not turn them into false FAILs against an unbound address.
V1_SKIP=0
if [ "$V1_RPC_BIND" = "127.0.0.1" ] && [ "$V1_HOST" != "127.0.0.1" ]; then
    V1_SKIP=1
    say "validator 1 public-RPC checks (localhost-only, skipping)" "SKIP"
fi

# Slot advancing on the public RPC.
SLOT1=""
[ "$V1_SKIP" = 0 ] && SLOT1=$("$SOLANA" slot --url "http://$V1_HOST:$V1_RPC_PORT" 2>/dev/null || echo "")
if [ -n "$SLOT1" ]; then
    sleep 10
    SLOT2=$("$SOLANA" slot --url "http://$V1_HOST:$V1_RPC_PORT" 2>/dev/null || echo "")
    if [ -n "$SLOT2" ] && [ "$SLOT2" -gt "$SLOT1" ]; then
        say "slot advancing ($SLOT1 -> $SLOT2 in 10s)" "OK"
    else
        say "slot advancing ($SLOT1 -> ${SLOT2:-?})" "FAIL"
        fail=1
    fi
fi

# Three voting validators with stake. Only the Current Validators
# section counts: `solana validators` also lists delinquent (non-
# voting) validators, and counting lines anywhere in the output would
# certify a chain whose third validator has stopped voting. Each of
# the three identities must appear in the current section.
if [ "$V1_SKIP" = 1 ]; then
    say "validators list (localhost-only, skipping)" "SKIP"
elif VALIDATORS=$("$SOLANA" validators --url "http://$V1_HOST:$V1_RPC_PORT" 2>/dev/null); then
    CURRENT=$(printf '%s\n' "$VALIDATORS" | sed -n '/Current Validators/,/Delinquent Validators/p')
    echo "$VALIDATORS" | grep -E "Identity|  $V1_IDENTITY|  $V2_IDENTITY|  $V3_IDENTITY" || true
    COUNT=0
    for id in "$V1_IDENTITY" "$V2_IDENTITY" "$V3_IDENTITY"; do
        if printf '%s\n' "$CURRENT" | grep -Fq "  $id"; then
            COUNT=$((COUNT + 1))
        fi
    done
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
if [ "$WITH_TRANSFER" = 1 ] && [ "$V1_SKIP" = 1 ]; then
    say "transfer A -> B (localhost-only, skipping)" "SKIP"
elif [ "$WITH_TRANSFER" = 1 ]; then
    TMPDIR=$(mktemp -d)
    trap 'rm -rf "$TMPDIR"' EXIT
    "$AGAVE_BIN/solana-keygen" new --no-bip39-passphrase -s -o "$TMPDIR/a.json" >/dev/null
    "$AGAVE_BIN/solana-keygen" new --no-bip39-passphrase -s -o "$TMPDIR/b.json" >/dev/null
    A=$("$AGAVE_BIN/solana-keygen" pubkey "$TMPDIR/a.json")
    B=$("$AGAVE_BIN/solana-keygen" pubkey "$TMPDIR/b.json")
    URL="http://$V1_HOST:$V1_RPC_PORT"
    if "$SOLANA" transfer --url "$URL" --keypair "$SCRIPT_DIR/keys/v1-identity.json" \
            --from "$SCRIPT_DIR/keys/v1-identity.json" "$A" 0.02 --allow-unfunded-recipient \
            --fee-payer "$SCRIPT_DIR/keys/v1-identity.json" 2>/dev/null | grep -q "Signature"; then
        say "funded fresh keypair A (0.02 MACHINE from v1 identity)" "OK"
    else
        say "funded fresh keypair A" "FAIL"; fail=1
    fi
    # The verdict must come from the transfer's own output, never from
    # a pipeline's exit status: without pipefail, `... | grep | head`
    # reports head's status (0) even when the transfer failed and no
    # signature exists. Require a non-empty signature AND B's balance
    # showing the received 0.01 (B is fresh, so its balance is exactly
    # what it received).
    TRANSFER_OUT=$("$SOLANA" transfer --url "$URL" --keypair "$TMPDIR/a.json" "$B" 0.01 \
            --fee-payer "$TMPDIR/a.json" 2>/dev/null || true)
    SIG=$(printf '%s' "$TRANSFER_OUT" | grep -oE '[A-Za-z0-9]{87,88}' | head -1 || true)
    BAL_B=$("$SOLANA" balance --url "$URL" "$B" 2>/dev/null | awk '{print $1}' || true)
    if [ -n "$SIG" ] && [ "$BAL_B" = "0.01" ]; then
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
