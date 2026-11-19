# Machine Livenet — Ports and Firewall

Every port the livenet needs, per validator. On a 3-VPS setup each host
opens only its own validator's ports. On a single host, open all three
sets. Ports below are the defaults in `validators.conf` — if you change
them there, change them here too.

## Port table

| Port(s) | Protocol | Validator | Purpose |
|---|---|---|---|
| 8001 | UDP | 1 | Gossip (validator 1) — how validators find each other |
| 10000–10999 | UDP | 1 | Dynamic range (1,000 ports): TPU, TPU-vote, TPU-forwards, repair, serve-repair, QUIC endpoints |
| 8899 | TCP | 1 | **Public JSON-RPC** — the endpoint wallets and apps use |
| 8011 | UDP | 2 | Gossip (validator 2) |
| 11000–11999 | UDP | 2 | Dynamic range (validator 2) |
| 8900 | TCP | 2 | RPC (validator 2, localhost-only by default) |
| 8021 | UDP | 3 | Gossip (validator 3) |
| 12000–12999 | UDP | 3 | Dynamic range (validator 3) |
| 8901 | TCP | 3 | RPC (validator 3, localhost-only by default) |
| 22 | TCP | all | SSH (your access — restrict to your IP if you can) |

Only validator 1's RPC (8899) is meant to be public. Validators 2 and
3 bind their RPC to 127.0.0.1, so even with the port open at the cloud
firewall, nothing answers on it from outside. If you want a second
public RPC, set `V2_RPC_BIND="0.0.0.0"` in `validators.conf` and open
8900.

UDP must be genuinely open end-to-end. This is the exact thing the
build VM blocks: gossip, voting, and transaction forwarding all ride
UDP. A host that blocks outbound UDP (some containers and restricted
VPS plans do) cannot run a validator — test it before you commit to a
provider (see DEPLOY.md step 2).

## Firewall — single host (all 3 validators)

```bash
sudo ufw allow 22/tcp
sudo ufw allow 8001/udp
sudo ufw allow 10000:10999/udp
sudo ufw allow 8011/udp
sudo ufw allow 11000:11999/udp
sudo ufw allow 8021/udp
sudo ufw allow 12000:12999/udp
sudo ufw allow 8899/tcp
sudo ufw enable
sudo ufw status numbered
```

## Firewall — 3-VPS setup (one validator per host)

On the validator 1 host:

```bash
sudo ufw allow 22/tcp
sudo ufw allow 8001/udp
sudo ufw allow 10000:10999/udp
sudo ufw allow 8899/tcp
sudo ufw enable
```

On the validator 2 host (replace 8011/11000–11999, no public RPC):

```bash
sudo ufw allow 22/tcp
sudo ufw allow 8011/udp
sudo ufw allow 11000:11999/udp
sudo ufw enable
```

On the validator 3 host (replace 8021/12000–12999, no public RPC):

```bash
sudo ufw allow 22/tcp
sudo ufw allow 8021/udp
sudo ufw allow 12000:12999/udp
sudo ufw enable
```

## Cloud firewall

If your provider has its own firewall outside the VM (Hetzner Cloud
Firewall, DigitalOcean Cloud Firewall, AWS security groups, etc.),
open the same ports there too — `ufw` alone is not enough when the
provider filters first. Both layers must allow the traffic.
