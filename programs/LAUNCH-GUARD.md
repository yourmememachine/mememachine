# LaunchGuard — MemeMachine's native anti-rug launch standard

## Why this exists

Every chain has launchpads. Launches still rug the same way: the
creator keeps the mint authority (prints more supply), keeps most of
the supply unlocked (dumps it), or both. Detection tools — including
our own Rug-Farm Registry — can only *warn* about this after the fact,
because on other chains the launch rules live in apps, not in the
chain's own program set.

LaunchGuard moves the two rules that stop the classic rug into a
program that ships **with MemeMachine itself**:

1. **Fixed supply, provably.** A launch can only register if the
   mint's mint authority **and** freeze authority are already revoked
   (`None` on-chain). Nobody — not the creator, not us — can ever
   print more of that token.
2. **Creator skin locked, by the program.** At registration the
   creator must lock at least **10% of total supply** in a vault owned
   by a program PDA for at least **30 days**. There is no admin key,
   no early-release instruction, and no instruction that shortens a
   lock. The creator can only *extend* a lock, never reduce it.

Anyone can read a launch's record on-chain and verify both facts
without trusting the creator, a launchpad, or us. That is the whole
point: **the guarantee is the program, not a promise.**

## Instructions (v1)

| Instruction | What it does |
|---|---|
| `RegisterAndLock { lock_amount, lock_slots }` | Verifies revoked authorities + fixed supply, creates the launch record and vault PDAs, transfers `lock_amount` from the creator into the vault. Fails if: authorities not revoked, supply is zero, lock < 10% of supply, lock period < 30 days or > 5 years, or a record already exists for the mint. |
| `ExtendLock { additional_slots }` | Creator only, while locked. Pushes the release slot further out (never past +5 years from now). There is deliberately no instruction that moves it earlier. |
| `Release {}` | Creator only, only after the lock expires. Returns the full locked amount from the vault to the creator and marks the record Released. Before expiry it always fails. |

Locks are counted in **slots, not wall-clock time** — the same
determinism rule the bridge uses. Slot counts cannot drift or be
nudged the way validator timestamps can, and a lock's expiry is
exactly computable from the registration slot.

## Parameters (v1, compile-time constants)

| Parameter | Value |
|---|---|
| Minimum lock share | 10% of total supply (1000 bps) |
| Minimum lock period | 30 days = 6,480,000 slots (400 ms target slot time) |
| Maximum lock period | 5 years = 394,200,000 slots |

## What LaunchGuard does NOT do (honest limits)

- It does not lock **liquidity-pool** positions, team wallets beyond
  the registered lock, or any tokens the creator holds in other
  wallets. A creator can still sell whatever they didn't lock.
- It does not judge token quality, team identity, or intent. A
  registered launch is *provably fixed-supply with a locked creator
  stake* — nothing more. Marketing it as more than that would be a lie.
- It is **not audited and not deployed**. It joins the audit scope
  (AUDIT.md) and the same BRIDGE.md §5-style gates before any real
  launch is pointed at it.
- v1 has one record per mint and one lock tranche. Multiple tranches,
  LP-position locking, and third-party "release trigger" (so anyone,
  not just the creator, can execute an expired release) are v2
  candidates — deliberately out of v1 to keep the audited surface
  small, the way Bitcoin kept its script small.

## How it fits the chain

- `programs/crates/launch-guard/` — the program (native Rust,
  solana-program 2.3, same conventions as the bridge programs).
- Rug-Farm Registry stays the *detection* layer for foreign launches
  (e.g. pump.fun). LaunchGuard is the *prevention* layer for native
  MemeMachine launches. Detection warns; prevention constrains.
