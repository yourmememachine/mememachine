# MemeMachine public testnet — Phase 1 runbook

Implements Phase 1 of `STAGED-LAUNCH.md`: the whole system, live in
public, with **worthless tokens only**. One VPS (~$20/mo class),
three validators on the one host (livenet DEPLOY.md "Option A"),
a public RPC, a faucet, the bridge running against **Solana
devnet**, and LaunchGuard deployed for real registrations.

Nothing in this phase touches mainnet value, and the BRIDGE.md §5
gate is unchanged: it binds mainnet. Both sides of every crossing
in this phase are test networks whose assets are worthless by
definition — that is the only reason deployment happens here
before the audit, and the write-ups must say so.

## Labeling rules (non-negotiable)

- The cluster is named **MemeMachine Testnet** everywhere it
  appears: RPC responses, explorer, faucet page, announcements.
- Test $MACHINE is a *different genesis* from any future livenet:
  different keys, different genesis hash. It is not convertible,
  not redeemable, and will never be "swapped" for real $MACHINE.
  Say this in every public mention. Anyone selling test $MACHINE
  is scamming; say that too.

## 0. Host

One Ubuntu 24.04 VPS: 4+ GB RAM, 2+ vCPU, ~50 GB disk, public IPv4,
UDP allowed (Vultr $20/mo tier is the researched default). Run
`ops/check-host.sh` first — it must PASS (it fails on this repo's
build VM by design: fd limit + UDP).

## 1. Chain up

On the VPS, in a **dedicated checkout** (`~/mememachine-testnet`,
never the directory a future livenet will use):

```bash
./testnet/up.sh
```

`up.sh` sequences the existing, tested pieces and stops at the
first failure: host gate → `./install.sh` (Agave 2.3.13) →
`livenet/generate-keys.sh` → `livenet/generate-genesis.sh` →
systemd units from `livenet/` → `livenet/check-cluster.sh
--with-transfer`. Keys and genesis are generated **on the host**
and never leave it (test keys are still keys — the habit is the
point). Success means: 3 validators voting, slots advancing, and a
signed transfer confirmed — the proofs N1/N2 in STATUS.md have
never had on suitable hardware.

Then expose the public RPC per `ops/PUBLIC-RPC.md` (validator 1's
RPC, rate-limited, behind the documented reverse-proxy shape) and
start the faucet against the genesis faucet account (1,000
$MACHINE-test in the standard allocation — drip small amounts,
it only needs to let people transact).

## 2. Bridge crossings (test value only)

Following `bridge/deploy/DEPLOY-BRIDGE.md`, with test networks
substituted on both ends:

1. Deploy `solana_lock.so` to **Solana devnet** (devnet SOL from
   the public faucet), initialize with the test relayer set
   (3-of-5, keys generated on the host) and **tiny caps** from
   `bridge/deploy/caps.py` against a test mint's supply.
2. Deploy `machine_bridge.so` to the Machine testnet, initialize
   with the same relayer set.
3. Run 5 relayer instances (the relayer's file-exchange phase-1
   is acceptable on one host for testnet) with `submit_enabled`.
4. Execute `bridge/drills/test-crossing.md` in both directions
   with worthless amounts, then `pause-drill.md` and
   `relayer-outage.md`. Write up every result in `bridge/VERIFY.md`
   style — commands, tx signatures, timings, failures included.

## 3. LaunchGuard live (testnet)

1. Deploy `launch_guard.so` (SBF hash in `programs/VERIFY.md`).
2. Register one demo launch end-to-end: create a test mint, mint
   supply, revoke both authorities, register with a 10% lock, and
   publish the record address so anyone can verify the lock
   on-chain. This demo is the public proof of the standard.
3. Open registrations to any testnet project that wants to try it.

## 4. Done means

- [ ] `check-cluster.sh --with-transfer` output saved + linked
      from STATUS.md (N1/N2 testnet rows updated honestly — the
      livenet rows stay "not proven" until Phase 2/3 hardware).
- [ ] Public RPC answering from outside the host.
- [ ] Crossing write-ups published (both directions + 2 drills).
- [ ] LaunchGuard demo registration verifiable on-chain.
- [ ] A public postmortem-style summary: what broke, what was
      slow, what the numbers were. Failures included, or it
      didn't happen.
