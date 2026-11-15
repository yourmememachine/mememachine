#![no_main]
//! Fuzz the cap arithmetic (DailyUsage::check_and_record) with
//! adversarial amounts, caps, and slots. The caps are the bridge's
//! value limits: an overflow or panic here is a safety bug. On
//! success the recorded usage must never exceed the daily cap,
//! must include the amount just recorded, and must carry the
//! current slot-window.

use libfuzzer_sys::fuzz_target;

fn u64_at(data: &[u8], i: usize) -> u64 {
    let start = i * 8;
    if data.len() < start + 8 {
        return 0;
    }
    u64::from_le_bytes(data[start..start + 8].try_into().unwrap())
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 48 {
        return;
    }
    let usage = bridge_common::DailyUsage {
        window_start: u64_at(data, 0),
        used: u64_at(data, 1),
    };
    let amount = u64_at(data, 2);
    let per_tx_cap = u64_at(data, 3);
    let daily_cap = u64_at(data, 4);
    let current_slot = u64_at(data, 5);
    if let Ok(new_usage) = usage.check_and_record(amount, per_tx_cap, daily_cap, current_slot) {
        assert!(
            new_usage.used <= daily_cap,
            "recorded usage {} exceeds daily cap {}",
            new_usage.used,
            daily_cap
        );
        assert!(
            new_usage.used >= amount,
            "recorded usage {} is less than the amount just recorded {}",
            new_usage.used,
            amount
        );
        assert_eq!(
            new_usage.window_start,
            current_slot / bridge_common::SLOTS_PER_DAY,
            "window must be the current slot-day"
        );
    }
});
