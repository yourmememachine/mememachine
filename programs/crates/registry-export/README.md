# LaunchGuard registry export

LaunchGuard's locks live on-chain, but a checker — a wallet, a bot,
RugCheck-style tooling — should not have to know the program's
account layout to answer "is this mint protected?" This crate is the
off-chain half of the standard: it reads every `LaunchRecord` and
`CreatorReputation` account from RPC and emits one public JSON
registry.

## Usage

```sh
# Build (from the repo root, or cd programs first)
cd programs && cargo build --release -p registry-export

# Export the registry from the local validator
./target/release/launchguard-registry \
  --url http://127.0.0.1:8899 \
  --program-id <DEPLOYED_LAUNCHGUARD_PROGRAM_ID>

# With verification: re-check each vault balance and mint authority
./target/release/launchguard-registry \
  --url http://127.0.0.1:8899 \
  --program-id <ID> --verify --out registry.json
```

`--program-id` is required and has no default: no LaunchGuard
program is deployed yet (local only — see `STATUS.md`), so the tool
refuses to guess which program to read.

## Output

One JSON document, entries sorted by mint:

```json
{
  "program_id": "…",
  "generated_at_slot": 123,
  "total_launches": 1,
  "total_locked": 100000000,
  "launches": [
    {
      "mint": "…",
      "creator": "…",
      "record": "…",
      "vault": "…",
      "total_supply": 1000000000,
      "locked_amount": 100000000,
      "locked_pct_bps": 1000,
      "lock_until_slot": 6480003,
      "registered_slot": 3,
      "status": "locked",
      "reputation": {
        "launches_registered": 1,
        "locks_completed": 0,
        "first_launch_slot": 3,
        "last_completed_slot": 0
      },
      "vault_balance": 100000000,
      "vault_verified": true,
      "authorities_revoked": true
    }
  ]
}
```

- `status`: `locked` (before expiry), `expired_unreleased` (lock
  elapsed, creator has not called Release yet — tokens are still in
  the vault), or `released`.
- `locked_pct_bps`: locked share of supply in basis points
  (1,000 = the 10% floor).
- `reputation`: `null` when the creator has no reputation account.
  Registration always creates one, so a missing reputation on a real
  launch would be a red flag — the registry reports the absence
  instead of inventing a record.
- `vault_balance` / `vault_verified` / `authorities_revoked` are
  `null` unless `--verify` was passed. With `--verify`, the tool
  fetches each vault token account and mint account and re-checks the
  registration invariants: the vault holds at least the locked amount
  (or is empty after release), and both mint and freeze authorities
  are revoked. The registry never claims a check it did not run.

## How a checker uses it

- "Is this mint LaunchGuard-protected?" — look the mint up in
  `launches`; `status == "locked"` and `vault_verified == true`
  is the strong answer.
- "Has this creator finished locks before?" —
  `reputation.locks_completed`.
- "How much is locked across all launches?" — `total_locked`.

## Trust model (honest limits)

- The export reads whatever the RPC endpoint returns. A lying RPC
  can lie about the registry. What the export guarantees is that,
  given the accounts, parsing is strict (owner + discriminator +
  exact length — foreign accounts are ignored, never misread) and
  verification re-checks the vault and mint accounts instead of
  trusting the record's word.
- The registry is a snapshot at `generated_at_slot`. Locks are
  slot-based, so a checker should re-fetch before acting on a
  near-expiry lock.
- Token-2022 mints are out of scope: LaunchGuard v1 rejects them,
  so they never appear in a registry.

## Tests

```sh
cd programs && cargo test --release -p registry-export
```

Unit tests cover the strict parser (wrong owner / discriminator /
truncation / trailing bytes), status derivation, reputation joining,
and the verify logic. The integration test registers two real
launches in program-test (10% / 30 days and 25% / 60 days, two
creators), builds the registry from the actual account bytes the
program wrote, then warps past expiry, releases, and re-exports to
prove the `released` status and the empty-vault check.
