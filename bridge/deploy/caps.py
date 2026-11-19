#!/usr/bin/env python3
"""Translate BRIDGE.md 4.4 launch caps into the absolute numbers the
programs are initialized with.

The programs enforce absolute caps set at Initialize (see
bridge/README.md). The design's launch values are percentages of the
vault's locked supply:

    per-transaction cap = 1% of locked supply
    daily cap           = 5% of locked supply

At deployment the locked supply starts at 0, so caps must be set
against the *planned* initial locked supply for the capped-mainnet
phase, and revisited by multisig + timelock as it grows. This script
does the arithmetic and refuses nonsense (daily < per-tx, zeros,
percentages above 100, non-finite percentages, and results that do
not fit the programs' u64 cap fields):

    python3 bridge/deploy/caps.py --planned-locked-supply 10000000000

Output is in raw base units, the unit every program field uses.
The arithmetic is exact (Decimal): at livenet scale (~1e18 base
units) float64 cannot represent the supply, and a float cap can
round UP above the intended percentage.
"""

import argparse
import sys
from decimal import Decimal, InvalidOperation

U64_MAX = 2**64 - 1


def parse_pct(text):
    try:
        return Decimal(text)
    except InvalidOperation:
        raise argparse.ArgumentTypeError(f"not a number: {text!r}")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--planned-locked-supply",
        type=int,
        required=True,
        help="planned initial locked supply, raw base units",
    )
    ap.add_argument("--per-tx-pct", type=parse_pct, default=Decimal("1.0"))
    ap.add_argument("--daily-pct", type=parse_pct, default=Decimal("5.0"))
    args = ap.parse_args()

    if args.planned_locked_supply <= 0:
        print("planned locked supply must be > 0", file=sys.stderr)
        return 1
    for name, pct in (("per-tx", args.per_tx_pct), ("daily", args.daily_pct)):
        if not pct.is_finite() or pct <= 0 or pct > 100:
            print(
                f"invalid {name} percentage: {pct} "
                "(must be finite, > 0 and <= 100)",
                file=sys.stderr,
            )
            return 1
    supply = Decimal(args.planned_locked_supply)
    per_tx = int(supply * args.per_tx_pct / 100)
    daily = int(supply * args.daily_pct / 100)
    if per_tx <= 0 or daily <= 0 or daily < per_tx:
        print(
            f"invalid caps: per_tx={per_tx} daily={daily} "
            "(both must be > 0 and daily >= per-tx)",
            file=sys.stderr,
        )
        return 1
    if per_tx > U64_MAX or daily > U64_MAX:
        print(
            f"invalid caps: per_tx={per_tx} daily={daily} "
            "(exceed the programs' u64 cap fields)",
            file=sys.stderr,
        )
        return 1
    print(f"planned_locked_supply = {args.planned_locked_supply}")
    print(f"per_tx_cap            = {per_tx}  ({args.per_tx_pct}% of planned supply)")
    print(f"daily_cap             = {daily}  ({args.daily_pct}% of planned supply)")
    print(f"vault_ceiling         = {args.planned_locked_supply}  (raise by multisig + timelock)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
