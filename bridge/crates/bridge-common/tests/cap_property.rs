//! Cap-accounting property tests (backlog #7, 2026-11-21).
//!
//! Randomized, seeded deposit/unlock sequences run against the same
//! pure cap functions the programs call — `DailyUsage::check_and_record`
//! and `check_dynamic_caps_with_bps`, in the same order the Solana-side
//! `Unlock` handler calls them (absolute caps first, then the dynamic
//! percentage layer; a failure at either layer commits nothing, exactly
//! as a failed transaction rolls its state back on-chain).
//!
//! Invariants asserted after every single operation, not just at the
//! end of a sequence:
//!   * the vault's locked total never goes negative and only moves by
//!     amounts an accepted operation recorded;
//!   * no accepted unlock exceeds the per-transaction absolute cap or
//!     the dynamic per-transaction percentage of the pre-unlock locked
//!     total;
//!   * the total unlocked inside any one window never exceeds the
//!     absolute daily cap or the dynamic daily percentage;
//!   * a rejected operation changes no state at all (usage, window,
//!     locked total, cumulative totals);
//!   * crossing into a new window restores the daily budget in full
//!     and never resurrects usage from an earlier window.
//!
//! The sequences are driven by `StdRng` with fixed seeds, so a failure
//! reproduces exactly from the seed printed in the assertion message.
//! Amounts are drawn from a mix that deliberately includes zero,
//! dust, cap-boundary values, and near-`u64::MAX` values, because the
//! interesting cap bugs live at the edges, not in the middle.

use bridge_common::{
    check_dynamic_caps_with_bps, BridgeError, DailyUsage, DYNAMIC_DAILY_BPS, DYNAMIC_PER_TX_BPS,
    SLOTS_PER_DAY, TIGHTENED_DAILY_BPS, TIGHTENED_PER_TX_BPS,
};
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;

/// Draw an amount biased toward the edges the caps care about.
fn draw_amount(rng: &mut StdRng, per_tx_cap: u64, locked: u64) -> u64 {
    match rng.gen_range(0..8) {
        0 => 0,
        1 => 1,
        2 => per_tx_cap,
        3 => per_tx_cap.saturating_add(1),
        4 => rng.gen_range(0..=locked.max(1)),
        5 => u64::MAX - rng.gen_range(0..1_000),
        6 => rng.gen_range(0..=per_tx_cap.saturating_mul(2).max(1)),
        _ => rng.gen_range(0..10_000),
    }
}

/// One modelled unlock, mirroring `solana-lock`'s `Unlock` handler:
/// amount-vs-locked check, absolute caps, then dynamic caps against
/// the pre-unlock locked total. Returns the error on rejection; on
/// success the caller commits the returned usage and deducts the vault.
fn try_unlock(
    usage: &DailyUsage,
    amount: u64,
    locked: u64,
    per_tx_cap: u64,
    daily_cap: u64,
    slot: u64,
    tightened: bool,
) -> Result<DailyUsage, BridgeError> {
    if amount > locked {
        return Err(BridgeError::InvalidAmount);
    }
    let new_usage = usage.check_and_record(amount, per_tx_cap, daily_cap, slot)?;
    let (ptx_bps, daily_bps) = if tightened {
        (TIGHTENED_PER_TX_BPS, TIGHTENED_DAILY_BPS)
    } else {
        (DYNAMIC_PER_TX_BPS, DYNAMIC_DAILY_BPS)
    };
    check_dynamic_caps_with_bps(amount, new_usage.used, locked, ptx_bps, daily_bps)?;
    Ok(new_usage)
}

#[test]
fn unlock_sequences_never_break_the_vault_or_cap_invariants() {
    for seed in 0..64u64 {
        let mut rng = StdRng::seed_from_u64(0xCA9_0000 + seed);
        let per_tx_cap = rng.gen_range(50..5_000);
        let daily_cap = per_tx_cap * rng.gen_range(1..10);
        let mut locked: u64 = rng.gen_range(10_000..1_000_000);
        let mut usage = DailyUsage::default();
        let mut slot: u64 = rng.gen_range(0..SLOTS_PER_DAY);
        // Post-unpause tightening is active for a random early stretch.
        let tightened_until = slot + rng.gen_range(0..SLOTS_PER_DAY);
        let mut window_unlocked: u64 = 0;
        let mut current_window = slot / SLOTS_PER_DAY;
        let mut total_deposited: u128 = locked as u128;
        let mut total_unlocked: u128 = 0;

        for step in 0..300 {
            // Time mostly creeps, sometimes jumps across windows.
            slot += match rng.gen_range(0..6) {
                0 => rng.gen_range(SLOTS_PER_DAY..3 * SLOTS_PER_DAY),
                _ => rng.gen_range(0..1_000),
            };
            let window = slot / SLOTS_PER_DAY;
            if window != current_window {
                current_window = window;
                window_unlocked = 0;
            }
            let tightened = slot < tightened_until;

            if rng.gen_bool(0.35) {
                // Deposit: not percentage-capped (BRIDGE.md 4.4); it
                // only grows the locked total the dynamic caps read.
                let amount = rng.gen_range(0..50_000);
                locked = locked.checked_add(amount).expect("model vault overflow");
                total_deposited += amount as u128;
                continue;
            }

            let amount = draw_amount(&mut rng, per_tx_cap, locked);
            let before = usage;
            let before_locked = locked;
            match try_unlock(&usage, amount, locked, per_tx_cap, daily_cap, slot, tightened) {
                Ok(new_usage) => {
                    // Per-transaction layers.
                    assert!(amount > 0, "seed {seed} step {step}: zero amount accepted");
                    assert!(
                        amount <= per_tx_cap,
                        "seed {seed} step {step}: {amount} over per-tx cap {per_tx_cap}"
                    );
                    let (ptx_bps, daily_bps) = if tightened {
                        (TIGHTENED_PER_TX_BPS, TIGHTENED_DAILY_BPS)
                    } else {
                        (DYNAMIC_PER_TX_BPS, DYNAMIC_DAILY_BPS)
                    };
                    assert!(
                        (amount as u128) * 10_000 <= (before_locked as u128) * ptx_bps as u128,
                        "seed {seed} step {step}: dynamic per-tx cap broken"
                    );
                    // Window layers.
                    window_unlocked += amount;
                    assert_eq!(
                        new_usage.used, window_unlocked,
                        "seed {seed} step {step}: recorded window usage diverged from accepted unlocks"
                    );
                    assert!(
                        new_usage.used <= daily_cap,
                        "seed {seed} step {step}: absolute daily cap broken"
                    );
                    assert!(
                        (new_usage.used as u128) * 10_000
                            <= (before_locked as u128) * daily_bps as u128,
                        "seed {seed} step {step}: dynamic daily cap broken"
                    );
                    assert_eq!(new_usage.window_start, window);
                    // Vault accounting: exactly the accepted amount left.
                    usage = new_usage;
                    locked -= amount;
                    total_unlocked += amount as u128;
                }
                Err(_) => {
                    // Rejection is atomic: nothing moved.
                    assert_eq!(usage, before, "seed {seed} step {step}: rejected op mutated usage");
                    assert_eq!(locked, before_locked, "seed {seed} step {step}: rejected op moved the vault");
                }
            }
            // Conservation: locked + everything ever unlocked == everything
            // ever deposited (the model starts with an initial deposit).
            assert_eq!(
                locked as u128 + total_unlocked,
                total_deposited,
                "seed {seed} step {step}: vault/wrapped conservation broken"
            );
        }
    }
}

#[test]
fn rejected_operations_never_mutate_usage() {
    for seed in 0..32u64 {
        let mut rng = StdRng::seed_from_u64(0xBAD_0000 + seed);
        let per_tx_cap = rng.gen_range(1..1_000);
        let daily_cap = per_tx_cap * rng.gen_range(1..5);
        let mut usage = DailyUsage::default();
        let mut slot = 0u64;
        for _ in 0..200 {
            slot += rng.gen_range(0..SLOTS_PER_DAY / 2);
            let amount = draw_amount(&mut rng, per_tx_cap, 0);
            let before = usage;
            match usage.check_and_record(amount, per_tx_cap, daily_cap, slot) {
                Ok(new_usage) => {
                    assert!(amount >= 1 && amount <= per_tx_cap);
                    assert!(new_usage.used <= daily_cap);
                    assert_eq!(new_usage.window_start, slot / SLOTS_PER_DAY);
                    usage = new_usage;
                }
                Err(_) => assert_eq!(usage, before, "seed {seed}: rejection mutated usage"),
            }
        }
    }
}

#[test]
fn new_window_restores_the_full_daily_budget() {
    for seed in 0..32u64 {
        let mut rng = StdRng::seed_from_u64(0x111D00 + seed);
        let per_tx_cap = rng.gen_range(10..1_000);
        let daily_cap = per_tx_cap * rng.gen_range(1..8);
        let mut usage = DailyUsage::default();
        let mut slot = rng.gen_range(0..SLOTS_PER_DAY);
        for _ in 0..50 {
            // Fill as much of this window as the caps allow.
            loop {
                match usage.check_and_record(per_tx_cap, per_tx_cap, daily_cap, slot) {
                    Ok(u) => usage = u,
                    Err(_) => break,
                }
            }
            assert!(usage.used <= daily_cap);
            // Jump at least one full window: the whole budget is back,
            // and the recorded window is the new one, never the old.
            slot += SLOTS_PER_DAY + rng.gen_range(0..SLOTS_PER_DAY);
            let fresh = usage
                .check_and_record(daily_cap.min(per_tx_cap), per_tx_cap, daily_cap, slot)
                .unwrap_or_else(|e| panic!("seed {seed}: first op of a new window rejected: {e:?}"));
            assert_eq!(fresh.window_start, slot / SLOTS_PER_DAY);
            assert_eq!(fresh.used, daily_cap.min(per_tx_cap));
            usage = fresh;
        }
    }
}

#[test]
fn extreme_amounts_never_overflow_the_window_accumulator() {
    // The accumulator is checked arithmetic: amounts near u64::MAX
    // must fail a cap or the overflow check — never wrap into a small
    // `used` that would let a window spend twice.
    for seed in 0..16u64 {
        let mut rng = StdRng::seed_from_u64(0x0E2F10 + seed);
        let mut usage = DailyUsage::default();
        let slot = rng.gen_range(0..SLOTS_PER_DAY);
        for _ in 0..100 {
            let amount = u64::MAX - rng.gen_range(0..3);
            let before = usage;
            match usage.check_and_record(amount, u64::MAX, u64::MAX, slot) {
                Ok(u) => {
                    assert!(u.used >= amount, "seed {seed}: accumulator wrapped");
                    usage = u;
                }
                Err(BridgeError::ArithmeticOverflow) | Err(BridgeError::AmountExceedsDailyCap) => {
                    assert_eq!(usage, before);
                }
                Err(other) => panic!("seed {seed}: unexpected error {other:?}"),
            }
        }
    }
}
