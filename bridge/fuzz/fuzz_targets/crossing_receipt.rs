#![no_main]
//! Fuzz the crossing-receipt surface of bridge-common (backlog #2):
//!
//! - `receipt_pda` derivation (seeds "receipt" || direction ||
//!   nonce.to_le_bytes()) must be total and deterministic for every
//!   (program_id, direction, nonce) — any direction byte, any nonce.
//! - `CrossingReceipt` borsh serialization must round-trip exactly
//!   at its declared `LEN`, for field values built from the input.
//! - Arbitrary bytes fed to the receipt decoder must either decode
//!   to a value that re-encodes and re-decodes identically, or fail
//!   cleanly. No panics anywhere.

use borsh::BorshDeserialize;
use libfuzzer_sys::fuzz_target;
use solana_program::pubkey::Pubkey;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    // 1. PDA derivation: pure function of (program_id, direction,
    //    nonce) — same inputs must give the same address and bump.
    if data.len() >= 41 {
        let program_id = Pubkey::new_from_array(data[0..32].try_into().unwrap());
        let direction = data[32];
        let nonce = u64::from_le_bytes(data[33..41].try_into().unwrap());
        let (a, bump_a) = bridge_common::receipt_pda(&program_id, direction, nonce);
        let (b, bump_b) = bridge_common::receipt_pda(&program_id, direction, nonce);
        assert_eq!((a, bump_a), (b, bump_b));
    }

    // 2. Build a receipt from the input (cycling the bytes so every
    //    field is exercised at any input length) and assert the
    //    serialize → decode round-trip at exactly `LEN`.
    let fill = |n: usize, offset: usize| -> Vec<u8> {
        (0..n).map(|i| data[(offset + i) % data.len()]).collect()
    };
    let receipt = bridge_common::CrossingReceipt {
        discriminator: data[0],
        direction: data[data.len() / 2],
        nonce: u64::from_le_bytes(fill(8, 1).try_into().unwrap()),
        message_hash: fill(32, 3).try_into().unwrap(),
        source_tx: fill(64, 5).try_into().unwrap(),
        amount: u64::from_le_bytes(fill(8, 7).try_into().unwrap()),
        recipient: Pubkey::new_from_array(fill(32, 11).try_into().unwrap()),
        slot: u64::from_le_bytes(fill(8, 13).try_into().unwrap()),
        timestamp: i64::from_le_bytes(fill(8, 17).try_into().unwrap()),
    };
    let bytes = borsh::to_vec(&receipt).expect("a receipt must serialize");
    assert_eq!(bytes.len(), bridge_common::CrossingReceipt::LEN);
    let back = bridge_common::CrossingReceipt::try_from_slice(&bytes)
        .expect("a serialized receipt must decode");
    assert_eq!(receipt, back);

    // 3. Arbitrary bytes into the decoder: a successful decode must
    //    re-encode at exactly `LEN` and decode again to the same
    //    value (canonical form, no second interpretation).
    if let Ok(decoded) = bridge_common::CrossingReceipt::try_from_slice(data) {
        let bytes = borsh::to_vec(&decoded).expect("a decoded receipt must re-encode");
        assert_eq!(bytes.len(), bridge_common::CrossingReceipt::LEN);
        let back = bridge_common::CrossingReceipt::try_from_slice(&bytes)
            .expect("a re-encoded receipt must decode");
        assert_eq!(decoded, back);
    }
});
