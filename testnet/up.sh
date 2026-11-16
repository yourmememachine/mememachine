#!/bin/bash
# up.sh — bring up the Machine PUBLIC TESTNET on a single host.
#
# Thin sequencer over the existing, tested livenet package — it adds
# no new chain logic. See testnet/TESTNET.md for the full phase,
# including labeling rules and what happens after the chain is up.
#
# Safety shape:
#   - Refuses to run unless TESTNET=1 is set in the environment.
#   - Refuses to run in a checkout that already has livenet keys
#     (testnet and livenet never share a checkout or a host dir).
#   - Stops at the first failing step (set -e) and says which step.
#
# Usage (on the testnet VPS, from the repo root of the testnet checkout):
#   TESTNET=1 ./testnet/up.sh
set -e

if [ "${TESTNET:-}" != "1" ]; then
  echo "REFUSING: this script brings up the public TESTNET." >&2
  echo "Re-run as: TESTNET=1 ./testnet/up.sh" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$ROOT_DIR"

if [ -d livenet/keys ] || [ -f livenet/genesis-info.env ]; then
  echo "REFUSING: this checkout already has livenet keys/genesis." >&2
  echo "Use a dedicated testnet checkout (see testnet/TESTNET.md)." >&2
  exit 1
fi

step() { echo; echo "=== up.sh: $1 ==="; }

step "1/6 host gate (fd limits, UDP, disk, RAM)"
./ops/check-host.sh

step "2/6 Agave 2.3.13 toolchain (bin/)"
if [ ! -x bin/agave-validator ]; then
  TAR_OPTIONS=--no-same-owner ./install.sh
fi

step "3/6 test keys (generated here, never leave this host)"
./livenet/generate-keys.sh

step "4/6 test genesis"
./livenet/generate-genesis.sh

step "5/6 validators (systemd units from livenet/)"
for unit in livenet/mememachine-validator-*.service; do
  sudo cp "$unit" /etc/systemd/system/
done
sudo systemctl daemon-reload
for n in 1 2 3; do
  sudo systemctl enable --now "mememachine-validator-$n"
done

step "6/6 proof: consensus + signed transfer"
./livenet/check-cluster.sh --with-transfer

echo
echo "Testnet chain is up. Next, per testnet/TESTNET.md:"
echo "  - expose the public RPC (ops/PUBLIC-RPC.md) + start the faucet"
echo "  - deploy the bridge to Solana devnet <-> this testnet (drills!)"
echo "  - deploy LaunchGuard and register the demo launch"
