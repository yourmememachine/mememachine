# Machine — Public RPC and Wallet Support

Status: **NEXT — needs the 3-validator livenet on a suitable host.**
Nothing public answers today. This document is the ready-to-run plan
for the day `livenet/check-cluster.sh --with-transfer` passes.

## What "public RPC" means here

Validator 1 already binds its RPC to `0.0.0.0:8899` in
`livenet/validators.conf` (validators 2 and 3 stay localhost-only —
they are consensus nodes, not public endpoints). Publishing the RPC
is therefore mostly a hosting and hardening job, not new code:

1. Run `ops/check-host.sh` on the host; it must pass.
2. Bring the livenet up per `livenet/DEPLOY.md`. Validator 1's raw
   RPC (`http://<V1_PUBLIC_IP>:8899`) is now reachable, but do NOT
   publish or announce it in this state: it is unauthenticated,
   unrate-limited, plaintext HTTP served by the consensus node
   itself, and RPC abuse against it is a liveness attack on 1 of 3
   validators. It becomes the public RPC only after step 3.
3. Put a reverse proxy in front for TLS and rate limits, rebind
   validator 1 to localhost (end of this step), and only then
   announce the endpoint. Minimal nginx example — first, the rate-
   limit zone, which must be defined in the `http` context
   (e.g. `/etc/nginx/nginx.conf`), not inside the server block:

   ```nginx
   # http context:
   limit_req_zone $binary_remote_addr zone=rpc:10m rate=10r/s;
   ```

   ```nginx
   server {
       listen 443 ssl;
       server_name rpc.mememachine.example;   # replace with the real domain
       # ssl_certificate / ssl_certificate_key: your TLS cert paths

       # Basic abuse limits: public RPC is a free resource, protect it.
       limit_req zone=rpc burst=20 nodelay;

       location / {
           proxy_pass http://127.0.0.1:8899;
           proxy_set_header Host $host;
           # WebSocket support for subscriptions:
           proxy_http_version 1.1;
           proxy_set_header Upgrade $http_upgrade;
           proxy_set_header Connection "upgrade";
       }
   }
   ```

   With the proxy in place, bind validator 1's RPC to localhost as
   well (`V1_RPC_BIND="127.0.0.1"` in `livenet/validators.conf`) so
   the proxy is the only public path.
4. Monitor it: slot height vs. the other validators (a lagging
   public RPC is worse than none), request rate, and the
   reconciliation job in `ops/reconcile.py` once the bridge exists.

## Wallet network addition

Wallets add a Solana-fork network from its chain identity. Publish
these facts (from `livenet/genesis-info.env` after the livenet
genesis is generated) wherever the project publishes network info,
and in `ops/network.json` in this repo (a template today; the values
are filled at livenet launch and must match `genesis-info.env`
exactly):

- Network name: Machine
- RPC endpoint: the public RPC above
- Genesis hash: the livenet genesis hash (NOT the localnet hash
  `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`)
- Shred version: from `genesis-info.env`
- Native token: MACHINE, 9 decimals (lamports), fixed supply
  1,000,000,000

OS-layer wallet connect (Phantom / Solflare / Backpack in the Solana
OS) already works; this section is the chain-level step — wallets
adding Machine as a network — and it is only real once the
public RPC answers.
