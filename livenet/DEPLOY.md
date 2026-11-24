# Machine Livenet — Deployment Guide

Deploy the Machine live network on fresh Ubuntu VPS(s): three
validators reaching consensus, one public RPC endpoint.

## Status: NOT RUNNING YET

Nothing in this guide has been executed and no server has been paid
for. Follow the steps below to bring the network up. Total time once
the server exists: roughly 30–60 minutes, most of it waiting for the
first slots.

## 0. What you need (and the one decision)

**Host requirements (per VPS):** Ubuntu 24.04 LTS, 4+ GB RAM, 2+ vCPU,
~50 GB disk, a public IPv4 address, and a network that allows UDP send
and the ports in `PORTS.md`. Roughly $5–10/month per VPS at the small
end (Hetzner, DigitalOcean, Vultr and similar all have a suitable tier
in that range — check the current price on the provider's page before
you buy; this guide does not promise a price).

**The one hosting decision to make:**

| Option | Cost | What it proves |
|---|---|---|
| **A. One VPS, all 3 validators** | ~$5–10/month | 3 validators reaching consensus on one machine. Cheapest, fastest to set up. One machine going down takes the whole network down. |
| **B. Three VPSs, one validator each** | ~$15–30/month | 3 validators on 3 machines in (ideally) different locations. A real distributed network — one host failing does not stop the chain. |

Option B is the honest "live net". Option A is fine for proving the
3-validator setup works before spending more. Either way, all three
validators run the exact same steps below — on option A you run all
three on the one host, on option B you run one per host.

**Why this VM cannot host it:** the build VM has a file-descriptor
hard limit of 4,096 (Agave requires 1,000,000) and blocks UDP send
(`sendto` returns EPERM), which breaks gossip and transaction
forwarding. The genesis commands in this package were tested here and
work — genesis creation needs neither — but `agave-validator` itself
cannot run here.

## 1. Create the server(s)

Create one (option A) or three (option B) Ubuntu 24.04 VPS(s) with the
spec above. Note each server's public IPv4 address. Make sure the
provider's own firewall (Hetzner Cloud Firewall, DigitalOcean Cloud
Firewall, AWS security groups, etc.) will allow the ports in
`PORTS.md` — `ufw` inside the VM is not enough if the provider filters
first.

## 2. Prepare the system (on every host)

```bash
# Become root and update.
sudo -i
apt update && apt upgrade -y
apt install -y curl git ufw python3

# A dedicated user — never run validators as root.
useradd -m -s /bin/bash mememachine

# --- File-descriptor limit (THE critical setting) ---
# Agave refuses to start below 1,000,000. Set it in both places:
cat >> /etc/security/limits.conf <<'EOF'
mememachine soft nofile 1000000
mememachine hard nofile 1000000
mememachine soft nproc 8192
mememachine hard nproc 8192
EOF
mkdir -p /etc/systemd/system.conf.d
cat > /etc/systemd/system.conf.d/mememachine-limits.conf <<'EOF'
[Manager]
DefaultLimitNOFILE=1000000
DefaultLimitNPROC=8192
EOF

# --- Network buffers (Agave's recommended values) ---
cat >> /etc/sysctl.conf <<'EOF'
net.core.rmem_max=134217728
net.core.wmem_max=134217728
net.core.rmem_default=134217728
net.core.wmem_default=134217728
EOF
sysctl -p

# Reboot so the limits apply to every service, then verify:
reboot
```

After the reboot, verify before going further:

```bash
# Run as the mememachine user:
su - mememachine -c 'ulimit -Hn'     # must print 1000000
su - mememachine -c 'ulimit -Sn'     # must print 1000000

# UDP send must work (this is what the build VM blocks):
python3 -c "
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.sendto(b'test', ('8.8.8.8', 53))
print('UDP send OK')
"
# If that raises PermissionError/EPERM, this host cannot run a
# validator. Pick another host or provider and start over.
```

## 3. Get the code

```bash
su - mememachine
git clone https://github.com/yourmememachine/mememachine /opt/mememachine
cd /opt/mememachine
./install.sh        # downloads prebuilt Agave 2.3.13 into bin/ (no source build)
./bin/solana --version
```

The `livenet/` directory in the repo holds everything below.

## 4. Generate keys and genesis (once, on ONE host)

Do this on the validator 1 host (or any host — the outputs must be
copied to the other hosts if you chose option B).

```bash
cd /opt/mememachine/livenet
./generate-keys.sh      # fresh keypairs for 3 validators + faucet + 5 allocation accounts (treasury, founder, ecosystem, community, validator-rewards)
./generate-genesis.sh   # builds the 3-validator genesis, prints the new hash
```

`generate-genesis.sh` writes `genesis-info.env` with the new genesis
hash, shred version, and all validator pubkeys. **This is a new
genesis with a new hash** — it is NOT the localnet hash
(`32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`). The new hash is the
livenet's identity; publish it so others can join.

**Back up `livenet/keys/` NOW, off the server** (encrypted USB, password
manager, whatever you trust). Lose the treasury key and the 243,999,000
MACHINE in it is gone forever. The founder (150,000,000), ecosystem
(250,000,000), community (200,000,000), and validator-rewards
(150,000,000) keys hold their own buckets and are equally unrecoverable
if lost. Lose a validator identity key and that validator cannot
restart. Do not commit `keys/` to git.

For option B, copy to each other host ONLY what that host's validator
actually needs: the ledger, genesis-info.env, validators.conf, and
THAT validator's own identity and vote keypairs. `start-validator.sh`
reads nothing else from keys/. Never copy the whole `livenet/`
directory: it contains all 15 keypairs, including the treasury
(243,999,000), founder, ecosystem, community and validator-rewards
keys — one validator-host compromise would then hand over the entire
genesis allocation. The allocation/bucket keys (treasury, founder,
ecosystem, community, validator-rewards, faucet, and the stake keys)
never leave validator 1's host except into the offline backup above.

```bash
# On validator 1 host, after generating — for validator 2's host
# (repeat with v3-* for validator 3's host):
scp -r /opt/mememachine/livenet/ledger /opt/mememachine/livenet/genesis-info.env /opt/mememachine/livenet/validators.conf mememachine@<V2_IP>:/opt/mememachine/livenet/
scp /opt/mememachine/livenet/keys/v2-identity.json /opt/mememachine/livenet/keys/v2-vote.json mememachine@<V2_IP>:/opt/mememachine/livenet/keys/
```

(Each host still needs its own copy of the repo + binaries from step 3
first, and a `livenet/keys/` directory to receive its two keypairs.)

## 5. Set the public IPs

Edit `livenet/validators.conf` and fill in each validator's public IP:

```bash
V1_PUBLIC_IP="<validator 1 public IP>"
V2_PUBLIC_IP="<validator 2 public IP>"
V3_PUBLIC_IP="<validator 3 public IP>"
```

On a single host (option A), all three are the same IP. If a field is
left empty, `start-validator.sh` auto-detects the IP and refuses to
start if detection fails — setting it explicitly is better.

## 6. Open the firewall

Follow `PORTS.md` for the exact rules (ufw commands included). On a
single host open all three validators' ports; on three hosts open only
each host's own set. Remember the provider's cloud firewall too.

## 7. Start the validators (in order)

```bash
cd /opt/mememachine/livenet

# 1. Validator 1 first. Wait until it is producing slots:
./start-validator.sh 1 --background
sleep 30
../bin/solana slot --url http://127.0.0.1:8899
# Run the slot command twice, 10 seconds apart — the number must rise.

# 2. Then validators 2 and 3:
./start-validator.sh 2 --background
./start-validator.sh 3 --background
sleep 30
```

To run them under systemd instead (recommended — auto-restart on crash
or reboot), install the units:

```bash
sudo cp mememachine-validator-*.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now mememachine-validator-1
sleep 30
sudo systemctl enable --now mememachine-validator-2 mememachine-validator-3
sudo systemctl status mememachine-validator-1 mememachine-validator-2 mememachine-validator-3
```

Logs: `journalctl -u mememachine-validator-1 -f`, or the per-validator
log files in `livenet/ledgers/vN-validator.log`.

## 8. Verify the live network

```bash
cd /opt/mememachine/livenet
./check-cluster.sh --with-transfer
```

It checks: all RPCs answer, the slot is advancing, the genesis hash on
the RPC matches `genesis-info.env`, exactly 3 vote accounts are voting
with stake, and — with `--with-transfer` — a real transfer between two
fresh keypairs confirms on-chain (0.01 MACHINE, funded from validator
1's identity). If every check says OK, the livenet is live. If any
check says FAIL, read the line, fix that thing, re-run.

## 9. Publish the network

Once `check-cluster.sh` passes, the network is real and these are the
facts to publish (website, repo, X):

- **Public RPC:** `http://<V1_PUBLIC_IP>:8899`
- **Genesis hash:** the value in `livenet/genesis-info.env`
- **Shred version:** the value in `livenet/genesis-info.env`
- **Validators:** 3, with their identity pubkeys from `genesis-info.env`
- **Allocation:** founder/team 150,000,000 (vested, WHITEPAPER.md Section 3), ecosystem 250,000,000, community 200,000,000, validator rewards 150,000,000, treasury 243,999,000, validator identities + stakes 6,000,000, faucet 1,000 — total 1,000,000,000 MACHINE
- **Slot:** whatever `solana slot --url http://<V1_PUBLIC_IP>:8899` returns

Anyone can join with those three values (genesis hash + shred version
+ an entrypoint) or just read the chain through the public RPC.

## 10. What is still NOT done

Going live does not finish the roadmap. Still NEXT after this deploy:
transfer verified on a host where UDP works (proven by step 8, if the
transfer check passes — then it moves to PROVEN), public RPC (done by
step 9), bridge, audit, bug bounty, whitepaper. Do not claim any of
those until they exist — see `LAUNCH_CHECKLIST.md` in the repo root.

## Troubleshooting

- `agave-validator` exits with "failed to raise the open file
  descriptor limit" → the hard limit is still below 1,000,000. Redo
  step 2 and reboot; check `ulimit -Hn` as the mememachine user.
- Validators 2/3 cannot find validator 1 → check the entrypoint
  (`V1_PUBLIC_IP:8001`) is reachable: UDP 8001 and the dynamic range
  must be open on validator 1's host AND in the provider firewall.
- Slot not advancing on validator 1 → check the log for errors;
  confirm the genesis copied into `ledgers/v1/` matches
  `genesis-info.env` (a stale ledger from a previous genesis will not
  join). Wipe with `rm -rf ledgers/v1` and restart (the genesis is
  re-copied on first boot).
- `check-cluster.sh` reports the wrong genesis hash → a validator is
  running an old ledger. Stop it, wipe its `ledgers/vN`, restart.
