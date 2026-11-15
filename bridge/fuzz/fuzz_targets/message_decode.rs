#![no_main]
//! Fuzz BridgeMessage decoding and validation: arbitrary bytes
//! must either decode to a message whose canonical encoding is
//! exactly 195 bytes and round-trips, or fail cleanly. No panics,
//! no unbounded allocations beyond the input.

use borsh::BorshDeserialize;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = bridge_common::BridgeMessage::try_from_slice(data) {
        let canonical = message.canonical_bytes();
        assert_eq!(canonical.len(), 195, "canonical encoding must be 195 bytes");
        // Hashing and validation must be total over decoded values.
        let _ = message.hash();
        let _ = bridge_common::validate_message(
            &message,
            bridge_common::CHAIN_SOLANA,
            bridge_common::CHAIN_MACHINE,
            u64::MAX,
        );
        // Borsh round-trip: re-encoding the decoded message and
        // decoding again must give the same message.
        let bytes = borsh::to_vec(&message).expect("a decoded message must re-encode");
        let back = bridge_common::BridgeMessage::try_from_slice(&bytes)
            .expect("a re-encoded message must decode");
        assert_eq!(message, back);
    }
});
