#!/usr/bin/env python3
"""Bridge reconciliation: locked on Solana vs wrapped supply on Machine.

BRIDGE.md section 5 item 7 requires a reconciliation job comparing
locked vs. minted supply continuously, before any real asset crosses.
This is that job, in dependency-free Python (stdlib only):

    python3 ops/reconcile.py \
        --solana-rpc https://api.mainnet-beta.solana.com \
        --machine-rpc http://<machine-rpc>:8899 \
        --vault <vault token account> \
        --wrapped-mint <wrapped mint on Machine>

It reads:
  * the vault token account on Solana (tokens locked), and
  * the wrapped mint's supply on Machine (tokens minted),
and exits 0 only if they are exactly equal. Any mismatch, or any RPC
failure, exits 1 and prints RECONCILE FAIL — a mismatch means minted
tokens exist without backing (or backing without mints, e.g. a burn
whose unlock is still in flight; investigate before alarming, but
never silence it automatically).

The two numbers are bound to the same asset before they are compared:
the vault address must resolve to a parsed SPL token account (its
mint is read and reported), and the vault account's decimals must
equal the wrapped mint's decimals — raw base-unit equality across
different decimals would compare different real values, and two
unrelated empty accounts must never reconcile "OK".

Run it every minute from cron/systemd once the bridge is deployed
(see ops/mememachine-reconcile.timer). Until the bridge is deployed
there is nothing to reconcile and this script will fail on missing
accounts — that is correct, not a bug.
"""

import argparse
import json
import sys
import urllib.request


def rpc(url, method, params):
    body = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    ).encode()
    req = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        out = json.load(resp)
    if "error" in out:
        raise RuntimeError(f"{method}: {out['error']}")
    return out["result"]


def vault_account(url, account):
    """Return (amount, decimals, mint) for the vault token account.

    Raises unless the account is a parsed SPL token account, so a
    wrong address (a wallet, a mint, an unrelated account) fails the
    reconciliation instead of silently comparing some other number.
    """
    result = rpc(
        url,
        "getAccountInfo",
        [account, {"encoding": "jsonParsed", "commitment": "finalized"}],
    )
    value = result["value"]
    if value is None:
        raise RuntimeError(f"vault account not found: {account}")
    data = value["data"]
    if data.get("program") != "spl-token" or data["parsed"].get("type") != "account":
        raise RuntimeError(f"vault is not an SPL token account: {account}")
    info = data["parsed"]["info"]
    amount = info["tokenAmount"]
    return int(amount["amount"]), int(amount["decimals"]), info["mint"]


def mint_supply(url, mint):
    result = rpc(
        url,
        "getTokenSupply",
        [mint, {"commitment": "finalized"}],
    )
    return int(result["value"]["amount"]), int(result["value"]["decimals"])


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--solana-rpc", required=True)
    ap.add_argument("--machine-rpc", required=True)
    ap.add_argument("--vault", required=True, help="vault token account on Solana")
    ap.add_argument("--wrapped-mint", required=True, help="wrapped mint on Machine")
    args = ap.parse_args()

    try:
        locked, vault_decimals, source_mint = vault_account(
            args.solana_rpc, args.vault)
        minted, mint_decimals = mint_supply(args.machine_rpc, args.wrapped_mint)
    except Exception as e:  # RPC down is a reconciliation failure too.
        print(f"RECONCILE FAIL — could not read both chains: {e}")
        return 1

    print(f"vault mint (Solana):     {source_mint}")
    print(f"locked (Solana vault):   {locked}")
    print(f"minted (Machine supply): {minted}")
    if vault_decimals != mint_decimals:
        print(
            f"RECONCILE FAIL — decimals differ (vault {vault_decimals} vs "
            f"wrapped mint {mint_decimals}); raw amounts are not comparable. "
            "Pause the bridge and investigate (BRIDGE.md 4.3, "
            "ops/INCIDENT-RESPONSE.md)."
        )
        return 1
    if locked == minted:
        print("RECONCILE OK — locked == minted.")
        return 0
    print(
        "RECONCILE FAIL — locked != minted. Pause the bridge and "
        "investigate (BRIDGE.md 4.3, ops/INCIDENT-RESPONSE.md)."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
