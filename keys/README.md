# Keys — DEV ONLY

The keypairs in this directory are **throwaway development keys** for
the Machine test chain. They hold no mainnet value.

- `founder.json` — bootstrap validator identity / founder
  (`7ivcPQJ6C7RZAnprKkJHN9o1PrpSjE6iC2mm7RRS3DEz`)
- `vote.json` — vote account
  (`GMe5eK5iefNXFN3NbFUx7ge27JQQDi1irsnCkYJdzcDA`)
- `stake.json` — stake account
  (`CxRRi4HZqJoG3fWugwF5wT2bqVwyMgPXxfKDRWCfe87e`)
- `faucet.json` — faucet
  (`FVFXT54BR2W6qmbnMuoCcHiufxX47LjXh1Nhissku7KC`)

They are committed so the shipped genesis (`ledger/`) is runnable from
a fresh clone — the validator needs the matching private keys to boot
as the bootstrap validator.

**Do not use these keys on Solana mainnet. Do not reuse them for a
real deployment.** Before any public or production use, generate fresh
keypairs, regenerate the genesis, and update `GENESIS.md`.
