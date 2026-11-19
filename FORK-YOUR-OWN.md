# Fork Your Own Chain

A recipe for making your own Solana-derived L1 the way Machine was
made: stock Agave 2.3.13 binaries, **your own keys**, **your own
genesis**, **your own token name and supply** — ending with a chain
that boots, votes, confirms, and finalizes.

Everything below is the repo's actual flow. The multi-validator path
is `livenet/generate-keys.sh` + `livenet/generate-genesis.sh`,
committed and verified against Agave 2.3.13. The single-node path
adapts that same script to one bootstrap validator — Machine's
shipped localnet genesis predates the script, and its parameters are
recorded in `GENESIS.md`.

Background reading: `FORK.md` (how deep the fork is, honestly),
`GENESIS.md` (Machine's chain identity and every genesis parameter),
`STATUS.md` and `LAUNCH_CHECKLIST.md` (what is proven vs next).

## What "your own chain" means here — and what it doesn't

- Your chain identity **is** your genesis: a new genesis hash, your
  own shred version, your own bootstrap validator(s). That is what
  makes it a separate chain, not a rebrand of someone else's.
- Your token is the **native gas asset** created at genesis. It has
  no separate mint and no on-chain name string — "renaming the token"
  means choosing its name, supply, and distribution in your genesis
  and your docs, the same way $MACHINE is defined in `GENESIS.md`.
  1 token = 1,000,000,000 lamports, always.
- The validator binary is **stock Agave 2.3.13** and identifies as
  `client:Agave`. A source-patched build that reports
  `client:Machine` exists as a verified patch set in `patches/` (see
  `FORK.md` §2 and `build-from-source.sh`) but has not been built or
  run — don't claim a deeper fork than you have.

## Prerequisites

- Linux host, ~500 MB free for the Agave release.
- A host where **UDP send works** and the file-descriptor limit can
  be raised toward 1,000,000 (`ulimit -n`). Machine's build VM blocks
  UDP send and caps the limit at 4,096: the chain still votes and
  finalizes there, but user transfers don't confirm and only one
  validator runs. Verify transfers on a suitable host.
- This repo, cloned.

## Step 0 — Install the binaries (no source build)

```bash
./install.sh
```

Downloads the prebuilt Agave 2.3.13 release and extracts `solana`,
`solana-keygen`, `solana-genesis`, `solana-test-validator`,
`agave-validator`, and `agave-ledger-tool` into `bin/`.

## Path A — Single-node chain (fastest to a booting chain)

### Step 1 — Generate your own keys

Never reuse Machine's `keys/` — they are published throwaway dev
keys. Generate four fresh keypairs:

```bash
mkdir -p mychain/keys
for k in founder vote stake faucet; do
  ./bin/solana-keygen new --no-bip39-passphrase \
    --outfile mychain/keys/$k.json
done
```

- `founder` — the identity keypair; holds most of the supply and the
  vote account's withdraw authority.
- `vote` / `stake` — the bootstrap validator's vote and stake
  accounts.
- `faucet` — test airdrops.

### Step 2 — Decide your supply and distribution

Supply is not a single flag. Your total supply is the **sum** of:

- primordial account balances (your distribution — founder, funds,
  community, whatever split you choose),
- `--bootstrap-validator-lamports` + `--bootstrap-validator-stake-lamports`,
- `--faucet-lamports`,

plus ~0.25 token of rent-exempt minimums the genesis adds for system
accounts (inherent to every Solana genesis; Machine's exact figures
are in `GENESIS.md`).

Write the distribution as a primordial-accounts YAML file — a map of
pubkey → `{balance, owner, data, executable}` with the System Program
(`11111111111111111111111111111111`) as owner for plain balance
accounts. Balances are in lamports: 1,000,000,000 tokens =
`1000000000000000000` lamports. Machine's livenet split (founder 15%,
ecosystem 25%, community 20%, validator rewards 15%, treasury
~24.4%, validators + faucet the rest) is written out in
`livenet/generate-genesis.sh` — copy its shape, change the numbers.

### Step 3 — Create the genesis

Adapted from `livenet/generate-genesis.sh` (the flags below are the
verified ones; `--target-tick-duration` takes **microseconds** in
this build despite the help text — 6250 = 6.25 ms):

```bash
FOUNDER=$(./bin/solana-keygen pubkey mychain/keys/founder.json)
VOTE=$(./bin/solana-keygen pubkey mychain/keys/vote.json)
STAKE=$(./bin/solana-keygen pubkey mychain/keys/stake.json)
FAUCET=$(./bin/solana-keygen pubkey mychain/keys/faucet.json)

./bin/solana-genesis \
    --ledger mychain/ledger \
    --cluster-type development \
    --hashes-per-tick sleep \
    --ticks-per-slot 64 \
    --target-tick-duration 6250 \
    --slots-per-epoch 8192 \
    --enable-warmup-epochs \
    --inflation none \
    --fee-burn-percentage 50 \
    --target-lamports-per-signature 10000 \
    --target-signatures-per-slot 20000 \
    --lamports-per-byte-year 3480 \
    --rent-burn-percentage 50 \
    --rent-exemption-threshold 2 \
    --vote-commission-percentage 100 \
    --bootstrap-validator "$FOUNDER" "$VOTE" "$STAKE" \
    --bootstrap-validator-lamports 1000000000000000 \
    --bootstrap-validator-stake-lamports 1000000000000000 \
    --faucet-pubkey "$FAUCET" \
    --faucet-lamports 1000000000000 \
    --primordial-accounts-file mychain/primordial.yaml
```

Change the fee flags, tick timing, and epoch length if you want a
different chain feel — every parameter Machine uses, and what it
means, is tabulated in `GENESIS.md`. `--inflation none` is what makes
the supply fixed.

Your chain's identity is now fixed. Record it:

```bash
./bin/agave-ledger-tool --ledger mychain/ledger genesis-hash
```

Regenerating always produces a **different hash** (new timestamp) —
a genesis is a one-time creation, not a build artifact.

### Step 4 — Patch the vote authority (test-validator only)

`solana-genesis` sets the vote account's authorized voter to the
**identity** keypair. `solana-test-validator` votes with the **vote**
keypair, so on a stock genesis it can never vote and no slot ever
confirms. Fix it with the repo's verified script:

```bash
python3 patch-vote-authority.py mychain/ledger/genesis.bin
```

The script reads the founder and vote pubkeys from
`keys/founder.json` and `keys/vote.json` in the current directory, so
run it from a working copy where *your* founder/vote keypairs sit at
those paths (it verifies the VoteState layout and aborts on any
mismatch before writing). The genesis hash changes again after
the patch — the patched hash is your live hash. Full explanation:
`GENESIS.md`, "Vote-authority patch". Skip this step if you boot
with `agave-validator` instead (Path B) — it votes with the identity
keypair, which already matches.

### Step 5 — Boot it

Mirror what `start-validator.sh` does for Machine's shipped genesis,
with your ledger and keys:

```bash
cp -r mychain/ledger mychain/ledger-test
cp mychain/keys/founder.json mychain/ledger-test/validator-keypair.json
cp mychain/keys/vote.json    mychain/ledger-test/vote-account-keypair.json
cp mychain/keys/stake.json   mychain/ledger-test/stake-account-keypair.json
cp mychain/keys/faucet.json  mychain/ledger-test/faucet-keypair.json

./bin/solana-test-validator \
    --ledger mychain/ledger-test --rpc-port 8899 --faucet-port 9900
```

### Step 6 — Verify it

```bash
./bin/solana slot --url http://127.0.0.1:8899        # advancing
./bin/solana balance "$FOUNDER" --url http://127.0.0.1:8899
```

Slots advancing with confirmed/finalized slots tracking behind them
means your chain is voting and finalizing. That — not the boot log —
is the proof.

## Path B — Multi-validator chain (the repo's livenet package)

For three validators with `agave-validator` and systemd units, use
the package as shipped:

```bash
cd livenet
./generate-keys.sh      # fresh keys for 3 validators + allocation buckets
./generate-genesis.sh   # refuses to overwrite an existing genesis
```

Then follow `livenet/DEPLOY.md` and `livenet/PORTS.md` for the
per-validator start scripts and firewall notes. No vote-authority
patch is needed on this path. Status, stated plainly: this package
is **written and script-verified but not yet run** — no host has been
deployed. Machine's only running net is the single-node localnet.

## Rename checklist

To make the chain *yours* rather than a Machine clone:

1. **Keys** — fresh keypairs (Step 1). Non-negotiable.
2. **Token name & supply** — your name in your docs/genesis record;
   your supply from your primordial distribution + validator +
   faucet lamports (Step 2).
3. **Genesis parameters** — fees, timing, epochs (Step 3) — or keep
   Machine's; they're tabulated in `GENESIS.md`.
4. **Identity strings in your copy of the tooling** — chain name in
   start scripts and docs. (The binary still says `client:Agave`
   until you build the patched source — `patches/`,
   `build-from-source.sh`.)

## Honest limits

- **Dev keys are dev keys.** Keys generated for a local test chain
  are for that chain. Generate production keys on the deployment
  host and never commit them — see the warning in
  `livenet/generate-keys.sh`.
- **Host limits are real.** UDP send blocked → transfers don't
  confirm; fd limit capped → one validator. Both are host problems,
  not chain problems, and both are documented in `STATUS.md`.
- **Unaudited, local-first.** This recipe gets you a running chain,
  not a launched one. Machine's own launch is staged and audit-gated
  (`STAGED-LAUNCH.md`); do the same before real assets touch yours.
- **Nothing here is a token sale.** A genesis allocation is a test
  distribution. Giving it value is a separate, regulated step this
  recipe does not cover.
