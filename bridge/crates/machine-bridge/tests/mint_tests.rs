//! Integration tests for the Machine-side bridge program, run against a
//! real program-test validator with the real SPL token program.
//!
//! These tests exercise the full instruction path (not just the pure
//! validation functions in bridge-common): actual SPL minting, nonce PDA
//! creation, cap enforcement, pause, and the kill-switch.

use borsh::BorshDeserialize;
use bridge_common::*;
use machine_bridge::processor;
use solana_program_test::ProgramTest;
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use solana_program::program_pack::Pack;

const PROGRAM_ID_SEED: u8 = 42;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn program_id() -> Pubkey {
    Pubkey::new_from_array([PROGRAM_ID_SEED; 32])
}

fn relayer_keypairs() -> [Keypair; 5] {
    [
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
    ]
}

fn admin_keypairs() -> [Keypair; 3] {
    [Keypair::new(), Keypair::new(), Keypair::new()]
}

fn relayer_pubkeys(keys: &[Keypair; 5]) -> [Pubkey; 5] {
    let mut out = [Pubkey::default(); 5];
    for (i, k) in keys.iter().enumerate() {
        out[i] = k.pubkey();
    }
    out
}

fn admin_pubkeys(keys: &[Keypair; 3]) -> [Pubkey; 3] {
    let mut out = [Pubkey::default(); 3];
    for (i, k) in keys.iter().enumerate() {
        out[i] = k.pubkey();
    }
    out
}

fn sign_attestation(msg: &BridgeMessage, keys: &[Keypair], n: usize) -> Vec<RelayerSignature> {
    let hash = msg.hash();
    keys.iter()
        .take(n)
        .map(|k| {
            let sig = k.sign_message(&hash);
            let mut bytes = [0u8; 64];
            bytes.copy_from_slice(sig.as_ref());
            RelayerSignature {
                relayer: k.pubkey(),
                signature: bytes,
            }
        })
        .collect()
}

fn test_message(nonce: u64, amount: u64, recipient: Pubkey, source_mint: Pubkey) -> BridgeMessage {
    BridgeMessage {
        version: MESSAGE_VERSION,
        source_chain: CHAIN_SOLANA,
        dest_chain: CHAIN_MACHINE,
        nonce,
        source_mint,
        sender: Pubkey::new_unique(),
        recipient,
        amount,
        source_tx: [7u8; 64],
        source_slot: 100,
        expiry_slot: 1_000_000,
    }
}

struct Env {
    context: solana_program_test::ProgramTestContext,
    relayers: [Keypair; 5],
    admins: [Keypair; 3],
    wrapped_mint: Pubkey,
    recipient: Keypair,
    recipient_token_account: Pubkey,
}

async fn setup(per_tx_cap: u64, daily_cap: u64) -> Env {
    let program_id = program_id();
    let mut program_test = ProgramTest::new(
        "machine_bridge",
        program_id,
        solana_program_test::processor!(processor::process),
    );
    program_test.add_program(
        "spl_token",
        spl_token::id(),
        solana_program_test::processor!(spl_token::processor::Processor::process),
    );
    let mut context = program_test.start_with_context().await;
    let recent_blockhash = context.last_blockhash;

    let relayers = relayer_keypairs();
    let admins = admin_keypairs();
    let recipient = Keypair::new();
    let (authority_pda, _) = authority_pda(&program_id);

    // Wrapped mint, mint authority = bridge-authority PDA.
    let wrapped_mint = create_mint(
        &mut context.banks_client,
        &context.payer,
        &recent_blockhash,
        &authority_pda,
        Some(&authority_pda),
        6,
    )
    .await;
    let recipient_token_account = create_token_account(
        &mut context.banks_client,
        &context.payer,
        &recent_blockhash,
        &wrapped_mint,
        &recipient.pubkey(),
    )
    .await;

    // Initialize the bridge config.
    let (config_pda, _) = config_pda(&program_id);
    let init_ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::Initialize {
            relayers: relayer_pubkeys(&relayers),
            admins: admin_pubkeys(&admins),
            wrapped_mint,
            per_tx_cap,
            daily_cap,
        },
        vec![
            AccountMeta::new(context.payer.pubkey(), true),
            AccountMeta::new(config_pda, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[init_ix],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        recent_blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    Env {
        context,
        relayers,
        admins,
        wrapped_mint,
        recipient,
        recipient_token_account,
    }
}

async fn create_mint(
    banks_client: &mut solana_program_test::BanksClient,
    payer: &Keypair,
    recent_blockhash: &solana_sdk::hash::Hash,
    mint_authority: &Pubkey,
    freeze_authority: Option<&Pubkey>,
    decimals: u8,
) -> Pubkey {
    let mint = Keypair::new();
    let rent = banks_client.get_rent().await.unwrap();
    let lamports = rent.minimum_balance(spl_token::state::Mint::LEN);
    let tx = Transaction::new_signed_with_payer(
        &[
            system_instruction::create_account(
                &payer.pubkey(),
                &mint.pubkey(),
                lamports,
                spl_token::state::Mint::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_mint(
                &spl_token::id(),
                &mint.pubkey(),
                mint_authority,
                freeze_authority,
                decimals,
            )
            .unwrap(),
        ],
        Some(&payer.pubkey()),
        &[&payer, &mint],
        *recent_blockhash,
    );
    banks_client.process_transaction(tx).await.unwrap();
    mint.pubkey()
}

async fn create_token_account(
    banks_client: &mut solana_program_test::BanksClient,
    payer: &Keypair,
    recent_blockhash: &solana_sdk::hash::Hash,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Pubkey {
    let account = Keypair::new();
    let rent = banks_client.get_rent().await.unwrap();
    let lamports = rent.minimum_balance(spl_token::state::Account::LEN);
    let tx = Transaction::new_signed_with_payer(
        &[
            system_instruction::create_account(
                &payer.pubkey(),
                &account.pubkey(),
                lamports,
                spl_token::state::Account::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_account(
                &spl_token::id(),
                &account.pubkey(),
                mint,
                owner,
            )
            .unwrap(),
        ],
        Some(&payer.pubkey()),
        &[&payer, &account],
        *recent_blockhash,
    );
    banks_client.process_transaction(tx).await.unwrap();
    account.pubkey()
}

async fn token_balance(banks_client: &mut solana_program_test::BanksClient, token_account: &Pubkey) -> u64 {
    let acc: Account = banks_client
        .get_account(*token_account)
        .await
        .unwrap()
        .unwrap();
    let state = spl_token::state::Account::unpack(&acc.data).unwrap();
    state.amount
}

async fn send_mint(
    env: &mut Env,
    message: BridgeMessage,
    signatures: Vec<RelayerSignature>,
) -> Result<(), solana_program_test::BanksClientError> {
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (nonce_pda, _) = nonce_pda(&program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (receipt_pda, _) = receipt_pda(&program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (authority_pda, _) = authority_pda(&program_id);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ed25519_ix = build_ed25519_instruction(&message.hash(), &signatures);
    let ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::MintWrapped { message },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(nonce_pda, false),
            AccountMeta::new(env.wrapped_mint, false),
            AccountMeta::new(env.recipient_token_account, false),
            AccountMeta::new_readonly(authority_pda, false),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
            AccountMeta::new(receipt_pda, false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ed25519_ix, ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer],
        blockhash,
    );
    env.context.banks_client.process_transaction(tx).await
}

async fn warp(env: &mut Env) {
    // Advance the slot so the next transaction gets a fresh blockhash.
    // program-test reuses the blockhash within a slot, and the bank
    // returns the cached success for a transaction with an identical
    // signature instead of re-executing it — a replay test would pass
    // vacuously without this.
    env.context.warp_to_slot(100).unwrap();
}

fn assert_custom_error(
    result: Result<(), solana_program_test::BanksClientError>,
    expected: BridgeError,
) {
    let err = result.expect_err("expected the transaction to fail");
    let msg = format!("{:?}", err);
    assert!(
        msg.contains(&format!("Custom({})", expected as u32)),
        "expected Custom({}) for {:?}, got: {}",
        expected as u32,
        expected,
        msg
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn valid_3_of_5_claim_mints() {
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_mint(&mut env, msg, sigs).await.unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        50_000
    );
}

#[tokio::test]
async fn replay_of_used_nonce_fails() {
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_mint(&mut env, msg.clone(), sigs.clone()).await.unwrap();
    warp(&mut env).await;
    let again = send_mint(&mut env, msg, sigs).await;
    assert_custom_error(again, BridgeError::NonceAlreadyProcessed);
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        50_000
    );
}

#[tokio::test]
async fn two_of_5_claim_fails() {
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 2);
    let result = send_mint(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::QuorumNotReached);
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        0
    );
}

#[tokio::test]
async fn over_per_tx_cap_fails() {
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 100_001, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_mint(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::AmountExceedsPerTxCap);
}

#[tokio::test]
async fn over_daily_cap_fails() {
    let mut env = setup(400_000, 500_000).await;
    let msg1 = test_message(1, 400_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs1 = sign_attestation(&msg1, &env.relayers, 3);
    send_mint(&mut env, msg1, sigs1).await.unwrap();
    // Second mint is within the per-tx cap but pushes the day over 500k.
    let msg2 = test_message(2, 200_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs2 = sign_attestation(&msg2, &env.relayers, 3);
    let result = send_mint(&mut env, msg2, sigs2).await;
    assert_custom_error(result, BridgeError::AmountExceedsDailyCap);
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        400_000
    );
}

#[tokio::test]
async fn paused_bridge_rejects_mints() {
    let mut env = setup(100_000, 500_000).await;
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    // Pause with 2 relayer signers.
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let pause_ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::Pause,
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new_readonly(env.relayers[0].pubkey(), true),
            AccountMeta::new_readonly(env.relayers[1].pubkey(), true),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer, &env.relayers[0], &env.relayers[1]],
        blockhash,
    );
    env.context.banks_client.process_transaction(tx).await.unwrap();

    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_mint(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::BridgePaused);

    // Relayers cannot unpause (BRIDGE.md 4.3: multisig-only).
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let unpause_ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::Unpause,
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new_readonly(env.relayers[0].pubkey(), true),
            AccountMeta::new_readonly(env.relayers[1].pubkey(), true),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer, &env.relayers[0], &env.relayers[1]],
        blockhash,
    );
    let result = env.context.banks_client.process_transaction(tx).await;
    assert_custom_error(result, BridgeError::Unauthorized);

    // Two admins can unpause; minting works again.
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let unpause_ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::Unpause,
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new_readonly(env.admins[0].pubkey(), true),
            AccountMeta::new_readonly(env.admins[1].pubkey(), true),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer, &env.admins[0], &env.admins[1]],
        blockhash,
    );
    env.context.banks_client.process_transaction(tx).await.unwrap();

    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_mint(&mut env, msg, sigs).await.unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        50_000
    );
}

#[tokio::test]
async fn kill_switch_is_permanent_and_burns_stay_open() {
    let mut env = setup(100_000, 500_000).await;
    // Mint some tokens first so there is something to burn after the kill.
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_mint(&mut env, msg, sigs).await.unwrap();

    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let kill_ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::KillSwitch,
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new_readonly(env.admins[0].pubkey(), true),
            AccountMeta::new_readonly(env.admins[1].pubkey(), true),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[kill_ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer, &env.admins[0], &env.admins[1]],
        blockhash,
    );
    env.context.banks_client.process_transaction(tx).await.unwrap();

    // Minting is dead forever.
    let msg2 = test_message(2, 10_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs2 = sign_attestation(&msg2, &env.relayers, 3);
    let result = send_mint(&mut env, msg2, sigs2).await;
    assert_custom_error(result, BridgeError::BridgeKilled);

    // Burning (the exit path) still works.
    send_burn(&mut env, 1, 20_000).await.unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        30_000
    );
}

#[tokio::test]
async fn admin_signatures_cannot_mint() {
    // There is no admin mint instruction. An attestation "signed" by
    // admins instead of relayers must fail as unknown relayers.
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.admins, 3);
    let result = send_mint(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::UnknownRelayer);
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        0
    );
}

async fn send_burn(
    env: &mut Env,
    nonce: u64,
    amount: u64,
) -> Result<(), solana_program_test::BanksClientError> {
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (record_pda, _) = nonce_pda(&program_id, DIR_MACHINE_TO_SOLANA, nonce);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let burn_ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::Burn {
            amount,
            destination: Pubkey::new_unique(),
            nonce,
        },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(env.recipient_token_account, false),
            AccountMeta::new(env.wrapped_mint, false),
            AccountMeta::new(record_pda, false),
            AccountMeta::new_readonly(env.recipient.pubkey(), true),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[burn_ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer, &env.recipient],
        blockhash,
    );
    env.context.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn burn_records_event_and_rejects_replay() {
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_mint(&mut env, msg, sigs).await.unwrap();

    send_burn(&mut env, 1, 10_000).await.unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        40_000
    );
    warp(&mut env).await;
    let again = send_burn(&mut env, 1, 10_000).await;
    assert_custom_error(again, BridgeError::NonceAlreadyProcessed);
}

#[tokio::test]
async fn reordered_signatures_still_mint() {
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let mut sigs = sign_attestation(&msg, &env.relayers, 3);
    sigs.reverse();
    send_mint(&mut env, msg, sigs).await.unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        50_000
    );
}

#[tokio::test]
async fn duplicate_relayer_signature_counts_once() {
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    // Relayer 0 twice plus relayer 1 once: three signature entries but
    // only two distinct relayers — quorum (3) must not be reached.
    let mut sigs = sign_attestation(&msg, &env.relayers[0..1], 1);
    sigs.extend(sign_attestation(&msg, &env.relayers[0..2], 2));
    assert_eq!(sigs.len(), 3);
    let result = send_mint(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::QuorumNotReached);
}

#[tokio::test]
async fn mint_at_expiry_boundary() {
    let mut env = setup(100_000, 500_000).await;
    // At the exact expiry slot the attestation is still valid
    // (rejection is current_slot > expiry_slot).
    let mut msg = test_message(1, 1_000, env.recipient.pubkey(), env.wrapped_mint);
    msg.expiry_slot = 500;
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    env.context.warp_to_slot(500).unwrap();
    send_mint(&mut env, msg, sigs).await.unwrap();
    // One slot past expiry, the same-shaped attestation is dead.
    let mut msg2 = test_message(2, 1_000, env.recipient.pubkey(), env.wrapped_mint);
    msg2.expiry_slot = 500;
    let sigs2 = sign_attestation(&msg2, &env.relayers, 3);
    env.context.warp_to_slot(501).unwrap();
    let result = send_mint(&mut env, msg2, sigs2).await;
    assert_custom_error(result, BridgeError::ExpiredAttestation);
}

#[tokio::test]
async fn large_nonce_gap_mints() {
    let mut env = setup(100_000, 500_000).await;
    // Nonces are PDA-keyed, not sequential: a jump from 1 to 1,000,000
    // is fine (the source chain's relayers own nonce assignment).
    let msg = test_message(1, 1_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_mint(&mut env, msg, sigs).await.unwrap();
    let msg2 = test_message(1_000_000, 1_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs2 = sign_attestation(&msg2, &env.relayers, 3);
    send_mint(&mut env, msg2, sigs2).await.unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        2_000
    );
}

#[tokio::test]
async fn compute_units_mint_wrapped() {
    // Measurement test: prints the compute units a full MintWrapped
    // (ed25519 precompile + bridge instruction) consumes, for the
    // record in bridge/VERIFY.md. The bound is a sanity ceiling, not
    // a target.
    let env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (nonce_pda, _) = nonce_pda(&program_id, DIR_SOLANA_TO_MACHINE, msg.nonce);
    let (receipt_pda, _) = receipt_pda(&program_id, DIR_SOLANA_TO_MACHINE, msg.nonce);
    let (authority_pda, _) = authority_pda(&program_id);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ed25519_ix = build_ed25519_instruction(&msg.hash(), &sigs);
    let ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::MintWrapped { message: msg },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(nonce_pda, false),
            AccountMeta::new(env.wrapped_mint, false),
            AccountMeta::new(env.recipient_token_account, false),
            AccountMeta::new_readonly(authority_pda, false),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
            AccountMeta::new(receipt_pda, false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ed25519_ix, ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer],
        blockhash,
    );
    let result = env
        .context
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    result.result.unwrap();
    let cu = result.metadata.unwrap().compute_units_consumed;
    println!("CU MintWrapped (3-of-5, incl. ed25519 precompile): {cu}");
    assert!(cu < 200_000, "MintWrapped CU unexpectedly high: {cu}");
}

async fn send_mint_to(
    env: &mut Env,
    message: BridgeMessage,
    signatures: Vec<RelayerSignature>,
    destination: Pubkey,
) -> Result<(), solana_program_test::BanksClientError> {
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (nonce_pda, _) = nonce_pda(&program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (receipt_pda, _) = receipt_pda(&program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (authority_pda, _) = authority_pda(&program_id);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ed25519_ix = build_ed25519_instruction(&message.hash(), &signatures);
    let ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::MintWrapped { message },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(nonce_pda, false),
            AccountMeta::new(env.wrapped_mint, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(authority_pda, false),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
            AccountMeta::new(receipt_pda, false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ed25519_ix, ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer],
        blockhash,
    );
    env.context.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn mint_to_wrong_owner_account_fails_and_nonce_survives() {
    // The relayer supplies the destination token account. If it is not
    // owned by the message recipient, the mint must fail on-chain —
    // and the failure must not consume the nonce, so the crossing can
    // be retried with the correct account.
    let mut env = setup(100_000, 500_000).await;
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let payer = env.context.payer.insecure_clone();
    let stranger = Keypair::new();
    let wrong_account = create_token_account(
        &mut env.context.banks_client,
        &payer,
        &blockhash,
        &env.wrapped_mint,
        &stranger.pubkey(),
    )
    .await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_mint_to(&mut env, msg.clone(), sigs.clone(), wrong_account).await;
    assert_custom_error(result, BridgeError::Unauthorized);
    // Retry with the correct destination account: succeeds.
    send_mint(&mut env, msg, sigs).await.unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        50_000
    );
}

#[tokio::test]
async fn mint_to_wrong_mint_account_fails() {
    // Destination account owned by the recipient but for a different
    // mint: also rejected on-chain.
    let mut env = setup(100_000, 500_000).await;
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let payer = env.context.payer.insecure_clone();
    let other_mint = create_mint(
        &mut env.context.banks_client,
        &payer,
        &blockhash,
        &payer.pubkey(),
        None,
        6,
    )
    .await;
    let wrong_account = create_token_account(
        &mut env.context.banks_client,
        &payer,
        &blockhash,
        &other_mint,
        &env.recipient.pubkey(),
    )
    .await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_mint_to(&mut env, msg, sigs, wrong_account).await;
    assert_custom_error(result, BridgeError::InvalidAccountData);
}

#[tokio::test]
async fn compute_units_burn() {
    // Measurement test: prints compute units for a Burn, for the
    // record in bridge/VERIFY.md.
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_mint(&mut env, msg, sigs).await.unwrap();
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (record_pda, _) = nonce_pda(&program_id, DIR_MACHINE_TO_SOLANA, 1);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let burn_ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::Burn {
            amount: 10_000,
            destination: Pubkey::new_unique(),
            nonce: 1,
        },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(env.recipient_token_account, false),
            AccountMeta::new(env.wrapped_mint, false),
            AccountMeta::new(record_pda, false),
            AccountMeta::new_readonly(env.recipient.pubkey(), true),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[burn_ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer, &env.recipient],
        blockhash,
    );
    let result = env
        .context
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    result.result.unwrap();
    let cu = result.metadata.unwrap().compute_units_consumed;
    println!("CU Burn: {cu}");
    assert!(cu < 200_000, "Burn CU unexpectedly high: {cu}");
}

fn sign_hash(hash: &[u8; 32], keys: &[Keypair], n: usize) -> Vec<RelayerSignature> {
    keys.iter()
        .take(n)
        .map(|k| {
            let sig = k.sign_message(hash);
            let mut bytes = [0u8; 64];
            bytes.copy_from_slice(sig.as_ref());
            RelayerSignature {
                relayer: k.pubkey(),
                signature: bytes,
            }
        })
        .collect()
}

async fn send_mint_with_precompile_hash(
    env: &mut Env,
    message: BridgeMessage,
    signatures: Vec<RelayerSignature>,
    precompile_hash: &[u8; 32],
) -> Result<(), solana_program_test::BanksClientError> {
    // Like send_mint, but the ed25519 precompile instruction carries
    // `precompile_hash` as its message instead of the real message
    // hash — the shape of an attestation for a *different* message.
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (nonce_pda, _) = nonce_pda(&program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (receipt_pda, _) = receipt_pda(&program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (authority_pda, _) = authority_pda(&program_id);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ed25519_ix = build_ed25519_instruction(precompile_hash, &signatures);
    let ix = Instruction::new_with_borsh(
        program_id,
        &MachineInstruction::MintWrapped { message },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(nonce_pda, false),
            AccountMeta::new(env.wrapped_mint, false),
            AccountMeta::new(env.recipient_token_account, false),
            AccountMeta::new_readonly(authority_pda, false),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
            AccountMeta::new(receipt_pda, false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ed25519_ix, ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer],
        blockhash,
    );
    env.context.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn attestation_with_wrong_domain_separator_fails() {
    // A quorum of valid signatures over a hash built with the wrong
    // domain separator attests some other message, not this one:
    // the program finds no precompile entries over the real hash.
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let wrong_hash = solana_sdk::hash::hashv(&[b"WRONG-DOMAIN", &msg.canonical_bytes()]).to_bytes();
    let sigs = sign_hash(&wrong_hash, &env.relayers, 3);
    let result = send_mint_with_precompile_hash(&mut env, msg, sigs, &wrong_hash).await;
    assert_custom_error(result, BridgeError::MissingAttestation);
}

#[tokio::test]
async fn relayer_rotation_requires_admin_multisig_and_takes_effect() {
    // Rotation path (BRIDGE.md section 4.3): the relayer set can be
    // replaced, but only by the admin multisig — never by relayers
    // themselves, never by a single admin — and the new set is the
    // only one that counts from that moment on.
    let mut env = setup(100_000, 500_000).await;
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let new_relayers: [Keypair; 5] = std::array::from_fn(|_| Keypair::new());
    let new_pubkeys = relayer_pubkeys(&new_relayers);

    fn set_relayers_ix(
        program_id: Pubkey,
        config: Pubkey,
        admin_pubs: &[Pubkey],
        relayers: [Pubkey; 5],
    ) -> Instruction {
        let mut metas = vec![AccountMeta::new(config, false)];
        for p in admin_pubs {
            metas.push(AccountMeta::new_readonly(*p, true));
        }
        Instruction::new_with_borsh(
            program_id,
            &MachineInstruction::SetRelayers { relayers },
            metas,
        )
    }
    async fn send_admin_ix(
        env: &mut Env,
        ix: Instruction,
        admin_idx: &[usize],
    ) -> Result<(), solana_program_test::BanksClientError> {
        let blockhash = env
            .context
            .banks_client
            .get_latest_blockhash()
            .await
            .unwrap();
        let mut all: Vec<&Keypair> = vec![&env.context.payer];
        for &i in admin_idx {
            all.push(&env.admins[i]);
        }
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&env.context.payer.pubkey()),
            &all,
            blockhash,
        );
        env.context.banks_client.process_transaction(tx).await
    }
    let admin_pubs: Vec<Pubkey> = env.admins.iter().map(|k| k.pubkey()).collect();

    // One admin is below the 2-of-3 threshold.
    let ix = set_relayers_ix(program_id, config_pda, &admin_pubs[..1], new_pubkeys);
    let result = send_admin_ix(&mut env, ix, &[0]).await;
    assert_custom_error(result, BridgeError::Unauthorized);

    // Two admins, but the proposed set repeats a key.
    let mut dup = new_pubkeys;
    dup[4] = dup[0];
    let ix = set_relayers_ix(program_id, config_pda, &admin_pubs[..2], dup);
    let result = send_admin_ix(&mut env, ix, &[0, 1]).await;
    assert_custom_error(result, BridgeError::DuplicateRelayerSignature);

    // Two admins and a clean set: rotation succeeds.
    let ix = set_relayers_ix(program_id, config_pda, &admin_pubs[..2], new_pubkeys);
    send_admin_ix(&mut env, ix, &[0, 1]).await.unwrap();

    // The old set's attestations no longer count.
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_mint(&mut env, msg.clone(), sigs).await;
    assert_custom_error(result, BridgeError::UnknownRelayer);

    // The new set's attestations mint.
    let sigs = sign_attestation(&msg, &new_relayers, 3);
    send_mint(&mut env, msg, sigs).await.unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.recipient_token_account).await,
        50_000
    );
}

#[tokio::test]
async fn mint_writes_a_crossing_receipt() {
    // Every completed crossing leaves a permanent receipt PDA,
    // derived from (direction, nonce) so anyone can find and verify
    // it: if this account exists with these fields, this exact
    // message was minted, for this recipient, at this slot.
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_mint(&mut env, msg.clone(), sigs).await.unwrap();

    let (receipt_addr, _) = receipt_pda(&program_id(), DIR_SOLANA_TO_MACHINE, 1);
    let acc = env
        .context
        .banks_client
        .get_account(receipt_addr)
        .await
        .unwrap()
        .expect("receipt PDA must exist after a successful mint");
    assert_eq!(acc.data.len(), CrossingReceipt::LEN);
    let receipt = CrossingReceipt::try_from_slice(&acc.data).unwrap();
    assert_eq!(receipt.discriminator, DISCRIMINATOR_RECEIPT);
    assert_eq!(receipt.direction, DIR_SOLANA_TO_MACHINE);
    assert_eq!(receipt.nonce, 1);
    assert_eq!(receipt.message_hash, msg.hash());
    assert_eq!(receipt.source_tx, msg.source_tx);
    assert_eq!(receipt.amount, 50_000);
    assert_eq!(receipt.recipient, env.recipient.pubkey());
    assert!(receipt.slot > 0);
}

#[tokio::test]
async fn attestation_with_wrong_message_length_fails() {
    // Same for signatures over a hash of a truncated canonical
    // encoding (100 of 195 bytes): not an attestation for this
    // message.
    let mut env = setup(100_000, 500_000).await;
    let msg = test_message(1, 50_000, env.recipient.pubkey(), env.wrapped_mint);
    let canonical = msg.canonical_bytes();
    let wrong_hash =
        solana_sdk::hash::hashv(&[DOMAIN_SEPARATOR, &canonical[..100]]).to_bytes();
    let sigs = sign_hash(&wrong_hash, &env.relayers, 3);
    let result = send_mint_with_precompile_hash(&mut env, msg, sigs, &wrong_hash).await;
    assert_custom_error(result, BridgeError::MissingAttestation);
}
