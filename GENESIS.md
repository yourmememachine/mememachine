# MemeMachine — Genesis

The canonical identity of the MemeMachine chain. Everything needed to
verify this chain is real, and to join it, is on this page.

## Chain identity

| Field | Value |
|---|---|
| Chain name | **MemeMachine** |
| Native token | **$MACHINE** (the gas token; 1 MACHINE = 1,000,000,000 lamports) |
| Genesis hash | `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb` |
| Shred version | `35836` |
| Cluster type | Development |
| Agave version | 2.3.13 (commit `5466f4592b1983adb13ba0a5d53f41ea2de69fba`) |
| Genesis created | 2026-11-14 |

> The genesis hash above is the **live** hash — the hash of the genesis
> actually running. The vote account's authorized voter was patched from
> the identity keypair to the vote keypair (see "Vote-authority patch"
> below) so `solana-test-validator` can vote; the patch changes the hash.
> The pre-patch creation hash was
> `7xzo2mxBzmZY4bbqHHyU3YUVqJxfUV6KvWyviuZzoRuE`. Do not use the
> pre-patch hash to join the chain — use the live hash above.

## Supply and distribution

Total supply: **1,000,000,000 MACHINE**.
Capitalization at genesis: **1,000,000,000.2535667 MACHINE** in 241
accounts — the extra ~0.2536 MACHINE is the rent-exempt minimum for the
system accounts, inherent to every Solana genesis.

| Holder | Address | Amount (MACHINE) | Notes |
|---|---|---|---|
| Founder (identity) | `7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz` | 998,999,000 | Spendable. The founder's keypair is `keys/founder.json`. |
| Stake account | `CxRRi4HZqJoG3fWugwF5wT2bqVwyMgPXxfKDRWCfe87e` | 1,000,000 | Staked to the vote account. Keypair `keys/stake.json`. |
| Faucet | `FVFXT54BR2W6qmbnMuoCcHiufxX47LjXh1Nhissku7KC` | 1,000 | Test airdrops. Keypair `keys/faucet.json`. |
| System accounts | — | ~0.2535667 | Rent-exempt minimums across 241 accounts. |

MACHINE is the native gas token from genesis: every transaction fee on
MemeMachine is paid in MACHINE. There is no separate wrapped or bridged
token at genesis. (An approved SPL asset bridged in from Solana mainnet
would be a separate asset — see `BRIDGE.md`.)

## Validator keys (bootstrap validator)

| Role | Address | Keypair |
|---|---|---|
| Identity / founder | `7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz` | `keys/founder.json` |
| Vote account | `GMe5eK5iefNXFN3NbFUx7ge27JQQDi1irsnCkYJdzcDA` | `keys/vote.json` |
| Stake account | `CxRRi4HZqJoG3fWugwF5wT2bqVwyMgPXxfKDRWCfe87e` | `keys/stake.json` |
| Faucet | `FVFXT54BR2W6qmbnMuoCcHiufxX47LjXh1Nhissku7KC` | `keys/faucet.json` |

Vote account state (live):

| Field | Value |
|---|---|
| Validator identity (node) | `7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz` |
| Vote authority | `GMe5eK5iefNXFN3NbFUx7ge27JQQDi1irsnCkYJdzcDA` (the vote keypair) |
| Withdraw authority | `7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz` (the founder keeps it) |
| Commission | 100% |
| Vote account balance | 0.0270744 MACHINE (rent-exempt) |

> **Dev keys only.** The keypairs in `keys/` are throwaway development
> keys for this test chain. They hold no mainnet value and must be
> replaced before any public or production use. Never use them on Solana
> mainnet, and never reuse them for a real deployment.

## Fee schedule (FeeRateGovernor, from genesis)

| Parameter | Value |
|---|---|
| Lamports per signature (current) | 9,500 (= 0.0000095 MACHINE) |
| Target lamports per signature | 10,000 |
| Minimum lamports per signature | 5,000 (= 0.000005 MACHINE) |
| Maximum lamports per signature | 100,000 (= 0.0001 MACHINE) |
| Target signatures per slot | 20,000 |
| Fee burn percentage | 50% (half of each fee is burned, half goes to the validator) |
| Rent burn percentage | 50% |
| Rent lamports per byte-year | 3,480 |
| Rent exemption threshold | 2.0 years |

A simple transfer costs the minimum 5,000 lamports (0.000005 MACHINE)
when the network is quiet, and adjusts up toward the target as demand
rises. There are no hidden fees.

## Consensus and timing (from genesis)

| Parameter | Value |
|---|---|
| Ticks per slot | 64 |
| Target tick duration | 6.25 ms |
| Hashes per tick | `sleep` (development: sleep instead of hashing, so slots tick fast) |
| Slots per epoch | 8,192 |
| Warmup epochs | enabled (stake activates quickly) |
| Slots per year | 78,892,314.984 |
| Inflation | none (0.0 initial / terminal / taper — fixed 1B supply, no new issuance) |

Observed on the live localnet: ~3 slots/second, blocks confirming and
finalizing continuously. That is in line with Solana's ~400 ms target.

## Vote-authority patch (why the live hash differs from creation)

`solana-genesis` sets a bootstrap vote account's authorized voter to the
validator **identity** keypair. `agave-validator` uses the identity as
its voter, so it matches. But `solana-test-validator` uses the **vote**
keypair as its voter, so on a stock genesis it can never vote and the
chain never confirms a slot.

Fix, applied to `ledger/genesis.bin` with `patch-vote-authority.py`
(committed in this repo, reproducible):

- In the vote account's `VoteState`, the authorized-voter entry
  (offset 94 in the account data) was changed from the founder pubkey
  to the vote pubkey `GMe5eK5i…`.
- `node_pubkey` and `authorized_withdrawer` were left as the founder —
  the founder keeps withdrawal authority.
- The patch is verified by the script before writing (it checks the
  version, node, withdrawer, commission, votes length, root slot, voter
  map length, epoch, and current voter, and aborts if any check fails).

After the patch the validator votes, confirms, and finalizes. The live
genesis hash above is the hash of the patched genesis.

## Live verification (localnet, 2026-11-14)

| Check | Result |
|---|---|
| Slot (confirmed) | 1,180 and advancing |
| Confirmed slot | 1,179 (tracking processed) |
| Finalized slot | 1,149 |
| Vote credits | 17,184 and growing |
| Root slot | 1,074+ |
| Founder balance | 998,998,999.99411 MACHINE (decreases only by vote-transaction fees) |
| Faucet balance | 1,000 MACHINE |
| Stake balance | 1,000,000 MACHINE |
| Snapshots | Full snapshot at slot 1,100 |

User transfers are **not yet verified on this VM** — see
`LAUNCH_CHECKLIST.md` ("Transfer between two fresh keypairs: NEXT").
The container blocks UDP send, which breaks the RPC → TPU forwarding
path that user transactions need. Voting and finalization are unaffected
(votes are created in-process), and the transfer is the first thing to
verify on a host where UDP works.
