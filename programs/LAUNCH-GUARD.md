# LaunchGuard — Machine's native anti-rug launch standard

## Why this exists

Every chain has launchpads. Launches still rug the same way: the
creator keeps the mint authority (prints more supply), keeps most of
the supply unlocked (dumps it), or both. Detection tools can only
*warn* about this after the fact, because on other chains the launch
rules live in apps, not in the chain's own program set.

LaunchGuard moves the two rules that stop the classic rug into a
program that ships **with Machine itself**:

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

**Classic SPL Token only (v1).** Registration rejects Token-2022
mints outright (`UnsupportedTokenProgram`). Token-2022 extensions —
permanent delegate, transfer hooks, mint-close authority — can
reintroduce exactly the control this standard removes, and they get
their own analysis before any support. A mint whose owning program
is not the classic SPL Token program is not a LaunchGuard launch.

## Instructions (v1)

| Instruction | What it does |
|---|---|
| `RegisterAndLock { lock_amount, lock_slots }` | Verifies revoked authorities + fixed supply, creates the launch record and vault PDAs, transfers `lock_amount` from the creator into the vault. Fails if: authorities not revoked, supply is zero, lock < 10% of supply, lock period < 30 days or > 5 years, or a record already exists for the mint. |
| `ExtendLock { additional_slots }` | Creator only, while locked. Pushes the release slot further out (never past 5 years from the registration slot — the maximum is a total lock period, not a per-extension window). There is deliberately no instruction that moves it earlier. |
| `Release {}` | Creator only, only after the lock expires. Returns the full locked amount from the vault to the creator and marks the record Released. Before expiry it always fails. |
| `CloseVault {}` | Creator only, and only after `Release` has completed. Sweeps any residual balance out of the vault and closes the vault account, returning its rent to the creator. The vault is a normal token account, so anyone can send extra tokens into it; `Release` moves exactly the locked amount, and without this instruction the excess would be stuck forever. Because it is gated on the record's Released state — set only after the full locked amount has left the vault — it has no path to any part of the lock: while a launch is locked or merely expired-but-unreleased, it always fails. |

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

## Creator reputation (on-chain)

Every creator has a **reputation account**: a PDA derived from
`["reputation", creator]`, created by their first registration and
updated by the program only:

| Field | Meaning |
|---|---|
| `launches_registered` | Protected launches this creator has registered. Incremented only by a successful `RegisterAndLock`. |
| `locks_completed` | Locks this creator held for their **full** period and released. Incremented only by `Release`, which the program refuses before expiry — a completion is proof the wait happened. |
| `first_launch_slot` / `last_completed_slot` | When the record started / when the latest completion happened. |

Any wallet, explorer, or other program can read this account —
it's the creator's public track record, written by the chain, not
by a website. Three honest limits: reputation is **per key** (a
creator can always start over with a fresh keypair at zero, so a
long history is meaningful precisely because it can't be bought or
transferred); it counts only what LaunchGuard can see —
launches and completions inside this standard; and the counters
are **unweighted** — a completed lock on a dust-supply mint (a
19-token supply locking 2 tokens satisfies the 10% floor) counts
exactly the same as a completed lock on a large launch, so a
reputation count alone proves only that locks were waited out,
never how much value stood behind them. Anyone weighing a
creator's reputation should read it alongside each launch
record's `locked_amount` and `total_supply`.

## What LaunchGuard does NOT do (honest limits)

- The 10% lock is a **floor, not a guarantee**. The other 90% of
  supply is unlocked from the start: the creator can sell it
  immediately, and nothing in the program stops a "soft rug" that
  dumps unlocked supply. What the standard proves is narrower —
  supply can never grow, and at least 10% cannot move for at least
  30 days.
- It does not lock **liquidity-pool** positions (no Raydium / Orca /
  Meteora LP tokens are touched), team wallets beyond the
  registered lock, or any tokens the creator holds in other
  wallets. A creator can still pull liquidity or sell whatever
  they didn't lock.
- It does not judge token quality, team identity, or intent. A
  registered launch is *provably fixed-supply with a locked creator
  stake* — nothing more. Marketing it as more than that would be a lie.
- Registration is **permissionless and first-come**. The record's
  `creator` is the wallet that registered the mint, not a verified
  issuer: once a mint's authorities are revoked, no on-chain record
  of who issued it remains, so the program can only check that the
  registrant signs and locks at least 10% of supply. Any holder of
  ≥10% who registers first occupies the mint's one record —
  permanently, since there is no close or supersede instruction —
  and is recorded as its creator. Creators should therefore
  register **before** distributing supply; a mint distributed past
  the 10% line before registration can be squatted by a holder.
- Revoking the freeze authority prevents **future** freezes; it
  cannot undo existing ones. A token account frozen by the issuer
  *before* revocation stays frozen forever — thawing needs the
  freeze authority that revocation just destroyed — and the
  program cannot see it: holder accounts are not enumerable
  on-chain, and registration inspects only the mint and the
  registrant's own account. Holders of a registered launch should
  check their own account is not frozen rather than assume the
  revoked authority means no account is.
- It is **not audited and not deployed**. It joins the audit scope
  (AUDIT.md) and the same BRIDGE.md §5-style gates before any real
  launch is pointed at it.
- v1 has one record per mint and one lock tranche. Multiple tranches,
  LP-position locking, and third-party "release trigger" (so anyone,
  not just the creator, can execute an expired release) are v2
  candidates — deliberately out of v1 to keep the audited surface
  small, the way Bitcoin kept its script small.

## Verify a launch yourself (the exact accounts to inspect)

Don't trust a "LaunchGuard-protected" claim — check the chain.
For a mint `M` and the LaunchGuard program `P`, inspect:

1. **The mint account `M` itself.** `mint_authority` and
   `freeze_authority` must both be **None**. If either is set, the
   token is not protected, whatever anyone says — the creator can
   still mint or freeze.
2. **The launch record PDA** — seeds `["launch", M]` under `P`.
   It must exist and show: `state` = locked (or released, after a
   completed lock), `locked_amount` ≥ 10% of `total_supply`,
   `lock_until_slot` in the future for an active lock, and
   `creator` = the wallet that registered.
3. **The vault token account PDA** — seeds `["vault", M]` under
   `P`. Its balance must be at least the record's `locked_amount`
   (it can be higher — anyone can send extra tokens into the
   vault; the excess cannot move early either, and after release
   the creator can recover it and close the vault with
   `CloseVault`), and its owner must be the authority PDA (next
   item), never a wallet. After a completed release and close,
   the vault account no longer exists and the record shows
   Released.
4. **The authority PDA** — seeds `["launch-authority", M]` under
   `P`. This program-derived address is the only authority that
   can move vault funds, and the program moves them only to the
   recorded creator, only after `lock_until_slot`.

If all four check out, the lock is real: the supply cannot grow,
no account can be frozen by the creator from that point on (an
account frozen *before* revocation stays frozen — see the honest
limits above), and the locked tokens cannot move early — those
are SPL- and program-level facts, not promises.

## Registry export (machine-readable)

Checking one launch by hand is fine; a wallet or checker needs to
answer the same question for every launch at once. The off-chain
registry export (`programs/crates/registry-export/`, binary
`launchguard-registry`) reads every `LaunchRecord` and
`CreatorReputation` account via `getProgramAccounts` and emits one
public JSON registry: mint, creator, locked amount and share,
lock and expiry slots, status (`locked` / `expired_unreleased` /
`released`), and creator reputation per launch. With `--verify` it
also fetches each vault and mint account and re-checks the
invariants above instead of trusting the record. Parsing is strict
(owner + discriminator + exact length), so foreign accounts are
ignored, never misread — and the registry never claims a check it
did not run: the `vault_*` / `authorities_revoked` fields are null
without `--verify`. This is the piece that turns LaunchGuard from
a standard you can verify into a standard a checker can consume:
launch standards elsewhere are PDFs; ours is a feed.

## How it fits the chain

- `programs/crates/launch-guard/` — the program (native Rust,
  solana-program 2.3, same conventions as the bridge programs).
- `programs/crates/registry-export/` — the off-chain registry
  export described above.
- Off-chain checkers stay the *detection* layer for foreign launches.
  LaunchGuard is the *prevention* layer for native Machine launches.
  Detection warns; prevention constrains.
