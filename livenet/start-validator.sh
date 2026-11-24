#!/bin/bash
# start-validator.sh — boot one Machine livenet validator.
#
# Usage:
#   ./start-validator.sh 1 [--background]   # validator 1 (bootstrap, public RPC)
#   ./start-validator.sh 2 [--background]   # validator 2 (joins via v1)
#   ./start-validator.sh 3 [--background]   # validator 3 (joins via v1)
#
# Start validator 1 first and wait until it is producing slots before
# starting 2 and 3. Validators 2 and 3 rendezvous with validator 1 over
# gossip (--entrypoint) and trust only its identity (--known-validator).
#
# What it does:
# - Reads validators.conf (ports, bind addresses) and genesis-info.env
#   (genesis hash, shred version, validator pubkeys).
# - Refuses to start if the file-descriptor hard limit is below
#   1,000,000 (agave-validator requires it; see DEPLOY.md).
# - On first boot, copies the canonical livenet genesis into the
#   validator's working ledger (ledgers/vN/).
# - Passes --expected-genesis-hash / --expected-shred-version so a
#   validator can never accidentally join the wrong chain.
#
# No vote-authority patch is needed: agave-validator votes with the
# identity keypair, which is what solana-genesis sets as the authorized
# voter. (The patch in patch-vote-authority.py is only for
# solana-test-validator.)
#
# Env override (used for testing): AGAVE_BIN=/path/to/bin
set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
AGAVE_BIN="${AGAVE_BIN:-$ROOT_DIR/bin}"

N="$1"
BACKGROUND=0
[ "${2:-}" = "--background" ] && BACKGROUND=1
case "$N" in
    1|2|3) ;;
    *) echo "Usage: $0 <1|2|3> [--background]" >&2; exit 1 ;;
esac

CONF="$SCRIPT_DIR/validators.conf"
INFO="$SCRIPT_DIR/genesis-info.env"
[ -f "$CONF" ] || { echo "ERROR: $CONF not found." >&2; exit 1; }
[ -f "$INFO" ] || { echo "ERROR: $INFO not found. Run ./generate-genesis.sh first." >&2; exit 1; }
# shellcheck disable=SC1090
. "$CONF"
# shellcheck disable=SC1090
. "$INFO"

[ -x "$AGAVE_BIN/agave-validator" ] || { echo "ERROR: $AGAVE_BIN/agave-validator not found. Run ./install.sh from the repo root first." >&2; exit 1; }

# Configuration and identity checks come before the host fd-limit
# check below: a wrong keypair or a bad gossip address is an operator
# error the operator should see first, on any host.
eval "PUBLIC_IP=\$V${N}_PUBLIC_IP"
eval "GOSSIP_PORT=\$V${N}_GOSSIP_PORT"
eval "DYNAMIC_RANGE=\$V${N}_DYNAMIC_RANGE"
eval "RPC_PORT=\$V${N}_RPC_PORT"
eval "RPC_BIND=\$V${N}_RPC_BIND"

if [ -z "$PUBLIC_IP" ]; then
    PUBLIC_IP=$(curl --fail -s --max-time 10 https://ifconfig.me || true)
    [ -n "$PUBLIC_IP" ] || { echo "ERROR: could not auto-detect the public IP. Set V${N}_PUBLIC_IP in validators.conf." >&2; exit 1; }
    echo "Auto-detected public IP: $PUBLIC_IP"
fi
# The gossip host is advertised to the whole network; never advertise
# an unvalidated third-party response (an error page, a CDN address,
# a hijacked answer) as this validator's identity.
valid_ip=0
if command -v python3 >/dev/null 2>&1; then
    python3 -c 'import ipaddress,sys; ipaddress.ip_address(sys.argv[1])' "$PUBLIC_IP" 2>/dev/null && valid_ip=1
elif printf '%s' "$PUBLIC_IP" | grep -Eq '^((25[0-5]|2[0-4][0-9]|1?[0-9]{1,2})\.){3}(25[0-5]|2[0-4][0-9]|1?[0-9]{1,2})$'; then
    valid_ip=1
fi
if [ "$valid_ip" != 1 ]; then
    echo "ERROR: public IP '$PUBLIC_IP' is not a valid IP address. Set V${N}_PUBLIC_IP in validators.conf." >&2
    exit 1
fi

KEYS_DIR="$SCRIPT_DIR/keys"
LEDGER_DIR="$SCRIPT_DIR/ledgers/v$N"
IDENTITY_KEY="$KEYS_DIR/v$N-identity.json"
VOTE_KEY="$KEYS_DIR/v$N-vote.json"
[ -f "$IDENTITY_KEY" ] || { echo "ERROR: $IDENTITY_KEY not found. Run ./generate-keys.sh first." >&2; exit 1; }
[ -f "$VOTE_KEY" ] || { echo "ERROR: $VOTE_KEY not found." >&2; exit 1; }
eval "VOTE_ACCOUNT=\$V${N}_VOTE"

# The keypair on disk must be the validator the genesis expects.
# Keys are copied between hosts for the 3-VPS setup, and a swapped or
# regenerated identity otherwise "starts" fine and simply never votes
# correctly — fail here, loudly, instead.
[ -x "$AGAVE_BIN/solana-keygen" ] || { echo "ERROR: $AGAVE_BIN/solana-keygen not found. Run ./install.sh from the repo root first." >&2; exit 1; }
eval "EXPECTED_IDENTITY=\$V${N}_IDENTITY"
ACTUAL_IDENTITY=$("$AGAVE_BIN/solana-keygen" pubkey "$IDENTITY_KEY")
if [ "$ACTUAL_IDENTITY" != "$EXPECTED_IDENTITY" ]; then
    echo "ERROR: $IDENTITY_KEY does not match V${N}_IDENTITY in genesis-info.env (keypair is $ACTUAL_IDENTITY, genesis expects $EXPECTED_IDENTITY)." >&2
    echo "The keypairs on this host are not the ones the genesis was built from. Restore the right keys; do not regenerate them." >&2
    exit 1
fi

if [ ! -f "$LEDGER_DIR/genesis.bin" ]; then
    echo "First boot: copying canonical livenet genesis into $LEDGER_DIR ..."
    [ -f "$SCRIPT_DIR/ledger/genesis.bin" ] || { echo "ERROR: $SCRIPT_DIR/ledger/genesis.bin not found. Run ./generate-genesis.sh first." >&2; exit 1; }
    mkdir -p "$LEDGER_DIR"
    cp "$SCRIPT_DIR/ledger/genesis.bin" "$LEDGER_DIR/"
    cp "$SCRIPT_DIR/ledger/genesis.tar.bz2" "$LEDGER_DIR/" 2>/dev/null || true
fi

ARGS=(
    --ledger "$LEDGER_DIR"
    --identity "$IDENTITY_KEY"
    --vote-account "$VOTE_ACCOUNT"
    --expected-genesis-hash "$GENESIS_HASH"
    --expected-shred-version "$SHRED_VERSION"
    --gossip-host "$PUBLIC_IP"
    --gossip-port "$GOSSIP_PORT"
    --dynamic-port-range "$DYNAMIC_RANGE"
    --rpc-port "$RPC_PORT"
    --rpc-bind-address "$RPC_BIND"
    --full-snapshot-interval-slots "$FULL_SNAPSHOT_INTERVAL_SLOTS"
    --limit-ledger-size "$LIMIT_LEDGER_SIZE"
    --log "$SCRIPT_DIR/ledgers/v$N-validator.log"
)

if [ "$N" != "1" ]; then
    # Join via validator 1's gossip address and trust only its identity.
    # If V1_PUBLIC_IP is unset, assume validator 1 is on this host
    # (single-host setup); on a multi-host setup it MUST be set.
    if [ -z "${V1_PUBLIC_IP:-}" ]; then
        V1_PUBLIC_IP="$PUBLIC_IP"
        echo "WARNING: V1_PUBLIC_IP not set in validators.conf — assuming validator 1 is on this host."
        echo "If validator 1 runs on another host, set V1_PUBLIC_IP in validators.conf and restart."
    fi
    ARGS+=( --entrypoint "$V1_PUBLIC_IP:$V1_GOSSIP_PORT" --known-validator "$V1_IDENTITY" )
else
    # Validator 1 is the public RPC node: serve transaction history.
    ARGS+=( --enable-rpc-transaction-history )
fi

echo "Starting Machine validator $N"
echo "  genesis: $GENESIS_HASH (shred $SHRED_VERSION)"
echo "  gossip:  $PUBLIC_IP:$GOSSIP_PORT   RPC: $RPC_BIND:$RPC_PORT"
[ "$N" != "1" ] && echo "  entrypoint: $V1_PUBLIC_IP:$V1_GOSSIP_PORT (validator 1)"

# agave-validator hard-requires a 1,000,000 file-descriptor limit.
# Checked last, just before launch, so the configuration and identity
# errors above are what an operator sees first on a misconfigured host.
HARD_NOFILE=$(ulimit -Hn)
if [ "$HARD_NOFILE" != "unlimited" ] && [ "$HARD_NOFILE" -lt 1000000 ]; then
    echo "ERROR: file-descriptor hard limit is $HARD_NOFILE, agave-validator needs 1,000,000." >&2
    echo "Fix it in /etc/security/limits.conf and systemd (DefaultLimitNOFILE=1000000), then reboot." >&2
    echo "See DEPLOY.md step 2. This host cannot run a livenet validator as it stands." >&2
    exit 1
fi
ulimit -n 1000000 2>/dev/null || true

if [ "$BACKGROUND" = 1 ]; then
    nohup "$AGAVE_BIN/agave-validator" "${ARGS[@]}" > "$SCRIPT_DIR/ledgers/v$N-stdout.log" 2>&1 &
    echo "Validator $N started in background (pid $!). Log: ledgers/v$N-stdout.log"
else
    exec "$AGAVE_BIN/agave-validator" "${ARGS[@]}"
fi
