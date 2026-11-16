//! The ed25519 precompile charge is NOT part of `compute_units_consumed`:
//! a transaction containing only a 3-signature ed25519 precompile instruction
//! succeeds and reports 0 CU. The charge lives in the cost model instead
//! (2,280 CU per signature non-strict, 2,400 CU strict, solana-cost-model
//! 2.3.13) and must be ADDED to the SBF execution CU the bridge CU tests
//! report to get the full on-chain cost of a crossing.
//!
//! This pins the assumption the bridge/VERIFY.md CU table relies on: if a
//! future runtime starts charging the precompile through the execution
//! meter, this test fails and the CU table must be recomputed.
use bridge_common::{build_ed25519_instruction, RelayerSignature};
use solana_program_test::*;
use solana_sdk::{signature::Signer, signer::keypair::Keypair, transaction::Transaction};

#[tokio::test]
async fn precompile_charge_excluded_from_execution_cu() {
    let program_test = ProgramTest::default();
    let mut context = program_test.start_with_context().await;
    let message_hash = [7u8; 32];
    let keys: Vec<Keypair> = (0..3).map(|_| Keypair::new()).collect();
    let sigs: Vec<RelayerSignature> = keys
        .iter()
        .map(|k| {
            let sig = k.sign_message(&message_hash);
            let mut bytes = [0u8; 64];
            bytes.copy_from_slice(sig.as_ref());
            RelayerSignature {
                relayer: k.pubkey(),
                signature: bytes,
            }
        })
        .collect();
    let ed25519_ix = build_ed25519_instruction(&message_hash, &sigs);
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ed25519_ix],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        blockhash,
    );
    let result = context
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    // The tx must have succeeded: if the signatures were bad, the runtime
    // precompile verification would fail the whole transaction.
    assert!(
        result.result.is_ok(),
        "precompile-only tx failed: {:?}",
        result.result
    );
    let cu = result.metadata.unwrap().compute_units_consumed;
    println!("PRECOMPILE-ONLY 3-sig ed25519 tx CU: {cu:?}");
    assert_eq!(
        cu, 0,
        "precompile charge leaked into execution CU; recompute VERIFY.md"
    );
    // Baseline: a tx with no instructions at all.
    let tx2 = Transaction::new_signed_with_payer(
        &[],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        blockhash,
    );
    let result2 = context
        .banks_client
        .process_transaction_with_metadata(tx2)
        .await
        .unwrap();
    println!("EMPTY tx CU: {:?}", result2.metadata.unwrap().compute_units_consumed);
}
