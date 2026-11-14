# MemeMachine — Livenet Package

Everything needed to run MemeMachine as a live public network: a
3-validator genesis, per-validator start scripts, systemd units, port
and firewall notes, and a step-by-step VPS deployment guide.

## Status: NOT RUNNING YET

The livenet is **not up**. Nothing here has been launched, and no
server has been paid for. This package is the complete, tested
preparation — the one thing missing is a host. See `DEPLOY.md` for the
exact host requirements and the one decision Dillu still has to make
(one VPS with all three validators, or three VPSs, one per validator).

Why it cannot run here: this VM cannot raise the open-file-descriptor
limit past 4,096 (Agave needs 1,000,000) and blocks UDP send, which
breaks gossip and transaction forwarding. The genesis-generation
commands in this package were tested on this VM and work — genesis
creation needs neither — but `agave-validator` itself cannot run here.

## Files

| File | What it is |
|---|---|
| `DEPLOY.md` | Step-by-step: fresh Ubuntu VPS → live 3-validator network |
| `PORTS.md` | Every port, what it does, and the firewall rules |
| `generate-keys.sh` | Generates fresh keypairs for 3 validators + faucet + treasury |
| `generate-genesis.sh` | Builds the 3-validator genesis (new hash — see below) |
| `validators.conf` | Per-validator ports, paths, and public IPs |
| `start-validator.sh` | Boots one validator: `./start-validator.sh 1` (or 2, or 3) |
| `mememachine-validator-1.service` | systemd unit for validator 1 |
| `mememachine-validator-2.service` | systemd unit for validator 2 |
| `mememachine-validator-3.service` | systemd unit for validator 3 |
| `check-cluster.sh` | Verifies the live cluster: RPCs, slots, 3 voting validators, a real transfer |

Generated at deploy time (never committed, never shipped pre-made):

| Path | What it is |
|---|---|
| `keys/` | Fresh validator/faucet/treasury keypairs — generated ON the VPS |
| `ledger/` | The canonical livenet genesis — generated ON the VPS |
| `ledgers/v1`, `v2`, `v3` | Per-validator working ledgers |
| `genesis-info.env` | Genesis hash, shred version, validator pubkeys — written by `generate-genesis.sh` |

## The livenet genesis is a NEW genesis

The localnet genesis (`32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`)
has **one** bootstrap validator baked in. A live network needs three
bootstrap validators, and bootstrap validators can only be set at
genesis creation — so the livenet gets a fresh genesis with a new hash.
`generate-genesis.sh` prints the new hash and shred version and writes
them to `genesis-info.env`; that hash is the one validators and wallets
use to join the livenet. Everything else matches the localnet: Agave
2.3.13, 1,000,000,000 MACHINE, same fee schedule, same timing
(64 ticks/slot, 6.25 ms target tick, 8,192 slots/epoch, warmup epochs,
no inflation).

Distribution at livenet genesis (1,000,000,000 MACHINE total):

| Holder | Amount (MACHINE) |
|---|---|
| Treasury (held by Dillu, key generated on the VPS) | 993,999,000 |
| Each validator identity (×3) | 1,000,000 |
| Each validator stake account (×3) | 1,000,000 |
| Faucet | 1,000 |

## Key rules

- **Keys are generated on the VPS, never shipped.** The dev keypairs
  in the repo root (`keys/`) are throwaway test keys for the localnet
  and must never be used for the livenet. `generate-keys.sh` refuses
  to overwrite existing keys without `--force`.
- **No vote-authority patch is needed.** The patch in
  `patch-vote-authority.py` exists only because `solana-test-validator`
  votes with the vote keypair. The livenet runs `agave-validator`,
  which votes with the identity keypair — the stock genesis matches.
- **Back up `keys/` before starting anything.** Lose the treasury key
  and the 993,999,000 MACHINE is gone. Lose a validator identity key
  and that validator cannot restart.
