//! Regression tests for self-audit L-1 (2026-11-16): the relayer's
//! source-transfer verification is bound to the record being
//! processed.
//!
//! Scenario: ONE source transaction contains TWO Deposit
//! instructions — one genuine (a real transfer of 500 into the
//! vault), one whose transfer never happens (e.g. a substituted
//! token program, rejected on-chain today by spl-token's checked
//! builders — see the K-1 note in REVIEW-1). Before the fix, the
//! relayer scanned the whole transaction for a matching transfer,
//! so the single genuine transfer satisfied the check for EITHER
//! record: the check was bound to (amount, destination) only, not
//! to the record or to the instruction that created it. Now the
//! transfer must sit in the inner-instruction group of the very
//! instruction that created the record under verification — the
//! second record fails closed, and batched records with genuine
//! transfers each resolve to their own sender.

use relayer::rpc::{sender_from_verified_source_transfer, SourceTransferCheck};
use relayer::RelayerError;
use serde_json::json;
use solana_program::pubkey::Pubkey;

fn deposit_check(vault: Pubkey, amount: u64) -> SourceTransferCheck {
    SourceTransferCheck {
        destination: Some(vault),
        mint: None,
        amount,
    }
}

/// Two Deposit instructions in one transaction; only the first has
/// a backing transfer (in its own inner group, index 0). The second
/// Deposit's group is empty.
fn two_deposits_one_transfer(
    program: &Pubkey,
    record_a: &Pubkey,
    record_b: &Pubkey,
    vault: &Pubkey,
    authority_a: &Pubkey,
) -> serde_json::Value {
    json!({
        "transaction": {"message": {"instructions": [
            {"programId": program.to_string(),
             "accounts": [Pubkey::new_unique().to_string(), record_a.to_string()],
             "data": "deposit-nonce-5"},
            {"programId": program.to_string(),
             "accounts": [Pubkey::new_unique().to_string(), record_b.to_string()],
             "data": "deposit-nonce-6"}
        ]}},
        "meta": {"innerInstructions": [
            {"index": 0, "instructions": [
                {"program": "spl-token", "parsed": {"type": "transfer", "info": {
                    "source": Pubkey::new_unique().to_string(),
                    "destination": vault.to_string(),
                    "authority": authority_a.to_string(),
                    "amount": "500"
                }}}
            ]},
            {"index": 1, "instructions": []}
        ]}
    })
}

#[test]
fn second_same_amount_record_in_one_tx_fails_verification() {
    let program = Pubkey::new_unique();
    let record_a = Pubkey::new_unique();
    let record_b = Pubkey::new_unique();
    let vault = Pubkey::new_unique();
    let attacker = Pubkey::new_unique();
    let tx = two_deposits_one_transfer(&program, &record_a, &record_b, &vault, &attacker);

    // The genuine record verifies against its own transfer.
    let sender_a = sender_from_verified_source_transfer(
        &tx,
        "transfer",
        &deposit_check(vault, 500),
        &program,
        &record_a,
    )
    .unwrap();
    assert_eq!(sender_a, attacker);

    // The second record — same amount, same transaction, no transfer
    // of its own — must NOT be verified by the first record's
    // transfer. Before the fix this returned `attacker`.
    let err = sender_from_verified_source_transfer(
        &tx,
        "transfer",
        &deposit_check(vault, 500),
        &program,
        &record_b,
    )
    .unwrap_err();
    assert!(
        matches!(err, RelayerError::SourceTransferNotVerified(_)),
        "expected SourceTransferNotVerified, got {err:?}"
    );
}

#[test]
fn batched_records_each_bind_their_own_transfer() {
    // Two genuine deposits batched in one transaction, by two
    // different users, same amount: each record must resolve to
    // ITS OWN transfer's authority — a whole-transaction
    // first-match scan would attribute both to the first sender.
    let program = Pubkey::new_unique();
    let record_a = Pubkey::new_unique();
    let record_b = Pubkey::new_unique();
    let vault = Pubkey::new_unique();
    let user_a = Pubkey::new_unique();
    let user_b = Pubkey::new_unique();
    let tx = json!({
        "transaction": {"message": {"instructions": [
            {"programId": program.to_string(),
             "accounts": [Pubkey::new_unique().to_string(), record_a.to_string()],
             "data": "deposit-nonce-5"},
            {"programId": program.to_string(),
             "accounts": [Pubkey::new_unique().to_string(), record_b.to_string()],
             "data": "deposit-nonce-6"}
        ]}},
        "meta": {"innerInstructions": [
            {"index": 0, "instructions": [
                {"program": "spl-token", "parsed": {"type": "transfer", "info": {
                    "source": Pubkey::new_unique().to_string(),
                    "destination": vault.to_string(),
                    "authority": user_a.to_string(),
                    "amount": "500"
                }}}
            ]},
            {"index": 1, "instructions": [
                {"program": "spl-token", "parsed": {"type": "transfer", "info": {
                    "source": Pubkey::new_unique().to_string(),
                    "destination": vault.to_string(),
                    "authority": user_b.to_string(),
                    "amount": "500"
                }}}
            ]}
        ]}
    });

    let sender_a = sender_from_verified_source_transfer(
        &tx,
        "transfer",
        &deposit_check(vault, 500),
        &program,
        &record_a,
    )
    .unwrap();
    let sender_b = sender_from_verified_source_transfer(
        &tx,
        "transfer",
        &deposit_check(vault, 500),
        &program,
        &record_b,
    )
    .unwrap();
    assert_eq!(sender_a, user_a);
    assert_eq!(sender_b, user_b);
}
