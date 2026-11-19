# Fork-script tests

Tests for Machine's own scripts — the shell scripts that boot and
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
python3 -m unittest tests.test_fork_scripts tests.test_ops_scripts -v
```

`test_ops_scripts.py` (added 2026-11-19, code-review debate round 5)
holds behaviour tests for the ops/deploy/livenet scripts: the
scripts are executed in temp directories against stub binaries
(`solana`, `solana-keygen`, `solana-genesis`, `agave-ledger-tool`,
`cargo`, `curl`) and a fake JSON-RPC server, and the tests assert
on the verdicts the scripts produce — e.g. a failed transfer must
not be reported as confirmed, an unverified release tarball must
not be installed, caps above 100% must be refused.

## What is covered

| Area | How it is tested |
|---|---|
| Shell syntax | `bash -n` on all 11 fork scripts (`start-validator.sh`, `install.sh`, `run-in-ns.sh`, `build-from-source.sh`, `testnet/up.sh`, `livenet/start-validator.sh`, `livenet/generate-genesis.sh`, `livenet/generate-keys.sh`, `livenet/check-cluster.sh`, `ops/check-host.sh`, `bridge/deploy/build-sbf.sh`); `py_compile` on `patch-vote-authority.py` |
| Ops/deploy behaviour (`test_ops_scripts.py`) | Executed against stubs: caps.py bounds/exactness, reconcile.py account+decimals binding (fake RPC), check-cluster transfer/delinquent/argument verdicts, generate-genesis empty-identity refusal, generate-keys `--force` genesis guard, install.sh sha256 refusal, start-validator entrypoint port / identity match / gossip-IP validation, build-sbf scratch-tree + pinned lockfile, systemd sandboxing presence, up.sh enable-after-proof ordering, run-in-ns sysctl content validation, root start-validator interrupted-seeding completion |
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
- `livenet/check-cluster.sh` against a real cluster — it needs a
  running 3-node livenet, which does not exist yet. Its verdict
  logic is tested against stub binaries (`test_ops_scripts.py`).
- `livenet/generate-genesis.sh` against the real toolchain — it
  would create a new genesis (and a new hash). Its argument
  handling, distribution arithmetic, and identity write-back are
  tested (the last against stub binaries); generation itself is a
  manual step.
