# Fork-script tests

Tests for MemeMachine's own scripts — the shell scripts that boot and
build the chain, the vote-authority patcher, and the genesis config.
They do **not** test the bridge programs (those live in `bridge/`:
`cargo test --release`, 34/34) and they do **not** test the live
validator (that is `STATUS.md`, verified against the running RPC).

## Run

```sh
./tests/run.sh
```

or

```sh
python3 -m unittest tests.test_fork_scripts -v
```

## What is covered

| Area | How it is tested |
|---|---|
| Shell syntax | `bash -n` on all 7 fork scripts (`start-validator.sh`, `install.sh`, `run-in-ns.sh`, `livenet/start-validator.sh`, `livenet/generate-genesis.sh`, `livenet/generate-keys.sh`, `livenet/check-cluster.sh`); `py_compile` on `patch-vote-authority.py` |
| `start-validator.sh` args | Unknown argument exits 1; `--help` is rejected (no help flag exists — documented, not assumed); the real argument-parsing block is extracted from the script and executed in a subshell for `[]`, `[--background]`, `[--fresh]`, `[--background --fresh]` |
| `livenet/start-validator.sh` args | Bad index (`9`) and missing index exit 1 with Usage; validator 1 exits 1 because `livenet/genesis-info.env` does not exist yet; the 1,000,000 fd-limit guard is present in the source |
| `livenet/generate-genesis.sh` args | Unknown argument exits 1; the refuse-to-overwrite guard is present in the source |
| `livenet/validators.conf` | Gossip ports, RPC ports and dynamic port ranges are all distinct / non-overlapping across the 3 validators |
| Livenet distribution | The lamport amounts in `generate-genesis.sh` sum to exactly 1,000,000,000 MACHINE (five primordial buckets — founder/team 150M, ecosystem 250M, community 200M, validator rewards 150M, treasury 243,999,000 — plus 3×1,000,000 identity, 3×1,000,000 stake, 1,000 faucet) |
| `patch-vote-authority.py` | Against synthetic genesis files in a temp dir only: rewrites the authorized voter and nothing else; rejects a bad layout (wrong commission), an already-patched voter, and a file with no vote account — leaving the file byte-identical in every failure case |
| Genesis config sanity | `ledger/genesis.bin` exists and `agave-ledger-tool genesis-hash` returns `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`; ledger-tool genesis output matches the documented shred version, ticks, slots/epoch, capitalization and rent; `GENESIS.md` contains the live hash, shred version, founder address and fee values; all four dev keypairs are 64-byte JSON arrays; `install.sh` pins Agave 2.3.13 |

## What is NOT covered (deliberately)

- Starting a validator (any script, any argument) — the live chain is
  verified separately against its RPC in `STATUS.md`.
- `--fresh` — never used by these tests.
- The real `ledger/genesis.bin` with the patcher — the patcher is only
  ever run on synthetic files in a temp dir.
- `livenet/check-cluster.sh` end-to-end — it needs a running 3-node
  livenet, which does not exist yet.
- `livenet/generate-genesis.sh` end-to-end — it would create a new
  genesis (and a new hash). Its argument handling and distribution
  arithmetic are tested; generation itself is a manual step.
