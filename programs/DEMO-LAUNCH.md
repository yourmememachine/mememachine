# LaunchGuard Demo Launch — Localnet Demonstration

**This is a localnet demonstration, run on 2026-11-16.** The demo
token is a fresh, worthless mint created inside a local program-test
ledger. It has no value, is not traded anywhere, and this is **not
a public launch**. Nothing here is deployed; $MACHINE is not
involved. The point of the demo is to show, with real on-chain
(program-test) accounts anyone can reproduce, exactly what a
LaunchGuard launch creates — and what happens when the rug paths
are attempted against it.

The demo is a test in the repo:
`programs/crates/launch-guard/tests/demo_launch.rs`. Reproduce it
with:

```
cd programs && cargo test --release --test demo_launch -- --nocapture
```

The demo uses fixed demo-only keypairs, so every run produces the
exact addresses below. (Those keypairs guard nothing and must never
hold anything.)

> **Hackathon note:** this demonstration was performed on
> 2026-11-16, before the Dev3pack Global Hackathon (27–29 Nov
> 2026). For that submission it therefore counts as **pre-existing
> work**, and is listed as such — the LaunchGuard program and this
> demo both predate the hackathon weekend.
>
> **Reproduction note (2026-11-19):** since the dust floor
> landed, reproducing this demo requires running `Initialize`
> first (the demo harness uses a floor of 1,000) — registration
> before Initialize now fails closed with
> `ConfigNotInitialized`, by design. The demo test performs the
> Initialize step itself; the recorded 2026-11-16 run above
> predates the instruction and is unchanged.

## What was created

| Thing | Value |
|---|---|
| Demo token mint | `AjTP23T2HhMryZVDPpzVYrTsWHgH6jKkMt9b8cLdn7hc` |
| Creator | `8V4wLMQTsD16bCX1BAT6fXHxikLMUYAqzuKXgiKTgmrd` |
| Creator token account | `Bkd3Br7wDuu466mqdqRcJ5RdeoRwyj5WKf6vbyp8YCEj` |
| Total supply minted | 1,000,000,000 (0 decimals) |
| Mint authority | **Revoked** |
| Freeze authority | **Revoked** |
| LaunchRecord PDA | `E6CttVSzy3Le4eHaxVWRyeR68HRqHnEkV6GSgUbBqRQi` |
| Vault PDA | `7AQtQMVajVMezY8xUutptYPqyyxo27rFcXLzpVMFim7f` |
| Creator reputation PDA | `4dnv3qqvVCwBibkpkTLrJr6AN5V6SekECNRofdiZNoH5` |
| Locked amount | 100,000,000 — exactly 10% of supply |
| Registered at slot | 3 |
| Unlock slot | 6,480,003 (registered slot + 6,480,000 slots = 30 days at the 400 ms target slot time) |
| Reputation after registration | 1 launch registered, 0 locks completed |

A reviewer verifying this demo should inspect: the mint (both
authorities `None`, supply 1,000,000,000), the LaunchRecord PDA
(locked amount, unlock slot, state = locked), the vault PDA token
account (balance 100,000,000, owned by the launch-authority PDA),
and the reputation PDA (1 registered, 0 completed).

## The rug attempts — all failed

Each attempt was submitted as a real transaction against the
registered launch. All four failed, and the final state check
confirmed nothing moved: vault still holds 100,000,000, the lock
still ends at slot 6,480,003, supply still 1,000,000,000.

| # | Attempt | Result |
|---|---|---|
| a | Mint 1,000,000 more tokens after revocation | **FAILED** — SPL Token rejects it: "the total supply of this token is fixed" (Token error `FixedSupply`, custom 5). A revoked mint authority can never sign again. |
| b | Release the lock early (creator, before slot 6,480,003) | **FAILED** — LaunchGuard error 8, `StillLocked`. There is no early-release path. |
| c | Shorten the lock (zero-slot `ExtendLock` — the only lock-moving instruction asked for no extension) | **FAILED** — LaunchGuard error 5, `LockPeriodTooShort`. Locks are extend-only by construction; no instruction moves the unlock slot earlier, and the recorded unlock slot was unchanged after the attempt. |
| d | Release the lock as a stranger (non-creator signer) | **FAILED** — LaunchGuard error 9, `NotCreator`. Release is creator-only, and only after expiry. |

## What this proves and what it does not

It proves the LaunchGuard program enforces its rules end to end in
a real program-test environment with the real SPL Token program:
fixed supply as a registration precondition, a 10%/30-day minimum
lock in a program-owned vault, and no path — for the creator or
anyone else — to mint more, exit early, shorten the lock, or take
the locked tokens.

It does not prove anything about a live network: this ran on
localnet only, the demo token is worthless, and LaunchGuard remains
unaudited and undeployed, under the same staged-launch gates as
everything else in this repo (see STATUS.md and STAGED-LAUNCH.md).
