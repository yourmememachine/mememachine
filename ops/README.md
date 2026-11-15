# MemeMachine Ops

Operational tooling and runbooks for running the chain and (later)
the bridge in public. Everything here is preparation until the
livenet exists — see each file's own status line.

| File | What it is | Status |
|---|---|---|
| `check-host.sh` | Host readiness gate: UDP send, 1,000,000 fd limit, CPU/RAM/disk, tools. Run this before paying for a host. | Ready; correctly FAILs on restricted VMs |
| `PUBLIC-RPC.md` | How the public RPC is published (validator 1 + TLS proxy + rate limits) once the livenet passes its checks. | Plan — needs livenet |
| `network.json` | Wallet network-addition metadata (name, RPC, genesis hash, shred version, MACHINE). | Template — values filled at livenet launch |
| `reconcile.py` | Bridge reconciliation: locked in the Solana vault vs. wrapped supply on Machine; exit 1 on any mismatch. | Ready — needs a deployed bridge |
| `mememachine-reconcile.service` / `.timer` | systemd units running reconciliation every minute. | Ready — needs a deployed bridge |
| `INCIDENT-RESPONSE.md` | Roles, severity, and first actions for chain halts, reconciliation mismatches, relayer stalls/disagreement, and key compromise. | Ready |

Order of operations for going public:
`check-host.sh` → `livenet/DEPLOY.md` → `livenet/check-cluster.sh
--with-transfer` → `PUBLIC-RPC.md` → bridge deploy runbook
(`bridge/deploy/DEPLOY-BRIDGE.md`) with reconciliation and incident
response live from day one.
