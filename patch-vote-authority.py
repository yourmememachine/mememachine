#!/usr/bin/env python3
"""Patch a solana-genesis vote account so solana-test-validator can vote.

Background: solana-genesis sets a bootstrap vote account's authorized voter
to the validator IDENTITY keypair. agave-validator uses the identity as its
voter, so it matches. But solana-test-validator uses the VOTE keypair as its
voter, so on a stock genesis it can never vote ("authorized keypair ... is
not available") and the chain never confirms a slot.

This script rewrites the vote account's authorized-voter entry in
genesis.bin from the identity (founder) pubkey to the vote pubkey, leaving
node_pubkey and authorized_withdrawer untouched (the founder keeps
withdrawal authority). It verifies the on-chain VoteState layout before
touching anything and aborts if any check fails.

VoteState layout (Agave 2.3, version 2 = Current), offsets relative to the
start of the vote account's data blob:
  [0:4]   version u32 (=2)
  [4:36]  node_pubkey
  [36:68] authorized_withdrawer
  [68]    commission u8
  [69:77] votes VecDeque length u64 (0 in a fresh genesis)
  [77]    root_slot Option<u8> (0 = None in a fresh genesis)
  [78:86] authorized_voters map length u64 (=1)
  [86:94] epoch u64 (=0)
  [94:126] authorized voter pubkey  <-- THIS is what we replace
"""
import json
import struct
import sys


def load_pubkey(path):
    """Return the 32-byte pubkey for a keypair JSON file."""
    data = json.load(open(path))
    return bytes(data[32:])


def main():
    genesis_path = sys.argv[1] if len(sys.argv) > 1 else "ledger/genesis.bin"
    founder = load_pubkey("keys/founder.json")
    vote = load_pubkey("keys/vote.json")

    blob = bytearray(open(genesis_path, "rb").read())

    # Find the vote account entry: the vote pubkey used as a key in the
    # accounts BTreeMap, followed by an Account struct whose data begins
    # with VoteState version 2 and node_pubkey == founder.
    data_start = None
    start = 0
    while True:
        i = blob.find(vote, start)
        if i < 0:
            break
        # Account struct after the 32-byte map key:
        # lamports u64, data length u64, then data.
        p = i + 32
        if p + 16 <= len(blob):
            lamports = struct.unpack_from("<Q", blob, p)[0]
            data_len = struct.unpack_from("<Q", blob, p + 8)[0]
            d = p + 16
            if (
                d + 126 <= len(blob)
                and data_len > 126
                and struct.unpack_from("<I", blob, d)[0] == 2
                and bytes(blob[d + 4 : d + 36]) == founder
            ):
                data_start = d
                print(f"vote account data at file offset {d} "
                      f"(lamports={lamports}, data_len={data_len})")
                break
        start = i + 1

    if data_start is None:
        print("ERROR: could not locate the vote account VoteState", file=sys.stderr)
        sys.exit(1)

    d = data_start
    # Verify the layout before patching.
    checks = [
        ("version", struct.unpack_from("<I", blob, d)[0] == 2),
        ("node_pubkey == founder", bytes(blob[d + 4 : d + 36]) == founder),
        ("withdrawer == founder", bytes(blob[d + 36 : d + 68]) == founder),
        ("commission == 100", blob[d + 68] == 100),
        ("votes length == 0", struct.unpack_from("<Q", blob, d + 69)[0] == 0),
        ("root_slot is None", blob[d + 77] == 0),
        ("authorized_voters len == 1", struct.unpack_from("<Q", blob, d + 78)[0] == 1),
        ("voter epoch == 0", struct.unpack_from("<Q", blob, d + 86)[0] == 0),
        ("current voter == founder", bytes(blob[d + 94 : d + 126]) == founder),
    ]
    for name, ok in checks:
        print(f"  check {name}: {'OK' if ok else 'FAIL'}")
        if not ok:
            print("ERROR: layout check failed, aborting without changes",
                  file=sys.stderr)
            sys.exit(1)

    blob[d + 94 : d + 126] = vote
    open(genesis_path, "wb").write(bytes(blob))
    print(f"patched authorized voter: founder -> vote keypair "
          f"({vote.hex()[:8]}...) in {genesis_path}")


if __name__ == "__main__":
    main()
