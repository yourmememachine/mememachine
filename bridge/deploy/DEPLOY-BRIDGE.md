# Deploying the Machine Bridge — Runbook

Status: **NOT DEPLOYED.** This runbook is ready to execute, in order,
on the day every prerequisite in BRIDGE.md section 5 is met. Until
then, running any step against real assets is a violation of the
project's own rules, not initiative.

## 0. Prerequisites gate — all must be true, in writing

From BRIDGE.md section 5. Check each one off with evidence, not vibes:

- [ ] Live 3-validator net: `livenet/check-cluster.sh` passes on the
      deployment hosts.
- [ ] Public RPC answers on both chains (Solana mainnet RPC the
      relayers trust, and the Machine public RPC — ops/PUBLIC-RPC.md).
- [ ] User transfer verified on the live Machine net:
      `livenet/check-cluster.sh --with-transfer` prints a signature.
- [ ] Fresh production keys generated **on the deployment hosts**
      (step 2). The dev keys in this repo are never used.
- [ ] Independent audit of both programs **and the relayer** is
      published, with critical/high findings fixed (AUDIT.md).
- [ ] Monitoring live: `ops/reconcile.py` running every minute
      (ops/mememachine-reconcile.timer), incident roles filled
      (ops/INCIDENT-RESPONSE.md).

## 1. Build the SBF binaries

```bash
./bridge/deploy/build-sbf.sh
```

Record the printed sha256 hashes of `solana_lock.so` and
`machine_bridge.so` in the deployment notes.

## 2. Generate fresh production keys — on the deployment hosts only

On the deployment host (never on a build machine, never committed):

```bash
# Outside the repo, e.g. /home/mememachine/keys (mode 700):
mkdir -m 700 -p ~/keys && cd ~/keys
for i in 1 2 3 4 5; do solana-keygen new --no-bip39-passphrase -o relayer-$i.json; done
for i in 1 2 3; do solana-keygen new --no-bip39-passphrase -o admin-$i.json; done
solana-keygen new --no-bip39-passphrase -o deployer.json
solana-keygen new --no-bip39-passphrase -o payer.json
```

Back each key up offline under its operator's control. The 5 relayer
keys go to the 5 different relayer operators — a relayer key never
leaves its operator's host. Losing a key loses that role; there is
no recovery, by design.

## 3. Create the wrapped mint and the vault

- On Machine: create the wrapped SPL mint with mint authority **and**
  freeze authority set to the machine-bridge program's
  `bridge-authority` PDA. No human key may hold mint authority —
  ever (BRIDGE.md 4.2).
- On Solana: create the vault token account for the source mint with
  owner = the solana-lock program's `bridge-authority` PDA.

## 4. Deploy both programs

```bash
solana program deploy target/deploy/solana_lock.so \
    --url <solana-rpc> --keypair ~/keys/deployer.json
solana program deploy target/deploy/machine_bridge.so \
    --url <machine-rpc> --keypair ~/keys/deployer.json
```

Set the upgrade authority to the admin multisig immediately; program
upgrades additionally go through a 48-hour timelock (BRIDGE.md 4.2).

## 5. Initialize, with absolute caps

Compute the launch caps from the planned initial locked supply:

```bash
python3 bridge/deploy/caps.py --planned-locked-supply <raw base units>
```

Initialize each program with: the 5 production relayer pubkeys, the
3 admin pubkeys (threshold 2), the mint/vault from step 3, and the
caps from `caps.py`. (The Initialize instruction formats are
`LockInstruction::Initialize` / `MachineInstruction::Initialize` in
bridge-common; submit them with the operator tooling used for the
audit's test crossings so the exact transactions are rehearsed.)

Verify afterwards, by reading both config PDAs: relayers, admins,
caps, paused = false, killed = false, and the mint/vault pubkeys all
match the deployment notes.

## 6. Start the relayers

Each of the 5 operators runs the relayer (bridge/crates/relayer)
with their own keypair, the shared config values, and
`submit_enabled: false` first. Confirm all five sign the same
message hash for a probe event before enabling submission anywhere.

## 7. Worthless-amount test crossings, then drills

Run `bridge/drills/test-crossing.md` end to end (both directions,
worthless test mint), then `bridge/drills/pause-drill.md` and
`bridge/drills/relayer-outage-drill.md`. The Phase 1 gate in
BRIDGE.md section 6 is 100 test crossings, 0 lost or duplicated
tokens, both drills passed.

## 8. Only then: capped mainnet

One approved asset, caps as computed, ceiling low, reconciliation
matching every minute for 30 days before any cap raise — and every
cap raise is its own multisig + timelock decision (BRIDGE.md 6).
