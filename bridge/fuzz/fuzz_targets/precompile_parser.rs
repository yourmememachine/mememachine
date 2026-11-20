#![no_main]
//! Fuzz the ed25519-precompile attestation parser
//! (bridge-common's `verify_precompile_attestation`) with arbitrary
//! instructions-sysvar contents. This parser reads offsets out of
//! attacker-influenced transaction data on-chain; it must return
//! Ok/Err for every input and never panic. AUDIT.md Scope A flags
//! it as the priority review target — this is its fuzz harness.

use libfuzzer_sys::fuzz_target;
use solana_program::{
    account_info::AccountInfo, pubkey::Pubkey, sysvar::instructions as ix_sysvar,
};
use std::{cell::RefCell, rc::Rc};

fuzz_target!(|data: &[u8]| {
    // A fixed, well-formed message and relayer set; the fuzzed
    // bytes are the sysvar account's data — the parser's real
    // attack surface.
    let relayers: [Pubkey; 5] = [
        Pubkey::new_from_array([11u8; 32]),
        Pubkey::new_from_array([22u8; 32]),
        Pubkey::new_from_array([33u8; 32]),
        Pubkey::new_from_array([44u8; 32]),
        Pubkey::new_from_array([55u8; 32]),
    ];
    let message = bridge_common::BridgeMessage {
        version: bridge_common::MESSAGE_VERSION,
        source_chain: bridge_common::CHAIN_SOLANA,
        dest_chain: bridge_common::CHAIN_MACHINE,
        nonce: 1,
        source_mint: Pubkey::new_from_array([66u8; 32]),
        sender: Pubkey::new_from_array([77u8; 32]),
        recipient: Pubkey::new_from_array([88u8; 32]),
        amount: 1_000,
        source_tx: [9u8; 64],
        source_slot: 100,
        expiry_timestamp: 1_700_040_000,
        source_timestamp: 1_700_000_000,
    };

    let key = ix_sysvar::ID;
    let owner = solana_program::sysvar::ID;
    let mut lamports = 0u64;
    let mut sysvar_data = data.to_vec();
    let account = AccountInfo {
        key: &key,
        is_signer: false,
        is_writable: false,
        lamports: Rc::new(RefCell::new(&mut lamports)),
        data: Rc::new(RefCell::new(&mut sysvar_data)),
        owner: &owner,
        executable: false,
        rent_epoch: 0,
    };

    // Any result is fine. A panic is a bug.
    let _ = bridge_common::verify_precompile_attestation(&message, &relayers, 3, &account);
});
