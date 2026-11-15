//! Integration tests for the Solana-side lock program, run against a real
//! program-test validator with the real SPL token program.

use borsh::BorshDeserialize;
use bridge_common::*;
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

const PROGRAM_ID_SEED: u8 = 43;

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

/// A burn attestation from Machine (source) to Solana (destination).
fn burn_message(nonce: u64, amount: u64, recipient: Pubkey, source_mint: Pubkey) -> BridgeMessage {
    BridgeMessage {
        version: MESSAGE_VERSION,
        source_chain: CHAIN_MACHINE,
        dest_chain: CHAIN_SOLANA,
        nonce,
        source_mint,
        sender: Pubkey::new_unique(),
        recipient,
        amount,
        source_tx: [11u8; 64],
        source_slot: 200,
        expiry_slot: 1_000_000,
    }
}

struct Env {
    context: solana_program_test::ProgramTestContext,
    relayers: [Keypair; 5],
    admins: [Keypair; 3],
    source_mint: Pubkey,
    user: Keypair,
    user_token_account: Pubkey,
    vault: Pubkey,
}

async fn setup(per_tx_cap: u64, daily_cap: u64, vault_ceiling: u64) -> Env {
    let program_id = program_id();
    let mut program_test = ProgramTest::new(
        "solana_lock",
        program_id,
        solana_program_test::processor!(solana_lock::processor::process),
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
    let user = Keypair::new();
    let mint_authority = Keypair::new();
    let (vault_authority, _) = authority_pda(&program_id);

    let source_mint = create_mint(
        &mut context.banks_client,
        &context.payer,
        &recent_blockhash,
        &mint_authority.pubkey(),
        None,
        6,
    )
    .await;
    let user_token_account = create_token_account(
        &mut context.banks_client,
        &context.payer,
        &recent_blockhash,
        &source_mint,
        &user.pubkey(),
    )
    .await;
    // Vault owned by the vault-authority PDA.
    let vault = create_token_account(
        &mut context.banks_client,
        &context.payer,
        &recent_blockhash,
        &source_mint,
        &vault_authority,
    )
    .await;
    // Fund the user with 2,000,000 source tokens.
    mint_to(
        &mut context.banks_client,
        &context.payer,
        &recent_blockhash,
        &source_mint,
        &user_token_account,
        &mint_authority,
        2_000_000,
    )
    .await;

    let (config_pda, _) = config_pda(&program_id);
    let init_ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Initialize {
            relayers: relayer_pubkeys(&relayers),
            admins: admin_pubkeys(&admins),
            source_mint,
            vault,
            per_tx_cap,
            daily_cap,
            vault_ceiling,
        },
        vec![
            AccountMeta::new(context.payer.pubkey(), true),
            AccountMeta::new(config_pda, false),
            AccountMeta::new_readonly(vault, false),
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
        source_mint,
        user,
        user_token_account,
        vault,
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

async fn mint_to(
    banks_client: &mut solana_program_test::BanksClient,
    payer: &Keypair,
    recent_blockhash: &solana_sdk::hash::Hash,
    mint: &Pubkey,
    destination: &Pubkey,
    mint_authority: &Keypair,
    amount: u64,
) {
    let ix = spl_token::instruction::mint_to(
        &spl_token::id(),
        mint,
        destination,
        &mint_authority.pubkey(),
        &[],
        amount,
    )
    .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer, mint_authority],
        *recent_blockhash,
    );
    banks_client.process_transaction(tx).await.unwrap();
}

async fn token_balance(banks_client: &mut solana_program_test::BanksClient, token_account: &Pubkey) -> u64 {
    let acc: Account = banks_client
        .get_account(*token_account)
        .await
        .unwrap()
        .unwrap();
    spl_token::state::Account::unpack(&acc.data).unwrap().amount
}

async fn total_locked(env: &mut Env) -> u64 {
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let acc: Account = env
        .context
        .banks_client
        .get_account(config_pda)
        .await
        .unwrap()
        .unwrap();
    LockConfig::try_from_slice(&acc.data).unwrap().total_locked
}

async fn send_deposit(
    env: &mut Env,
    amount: u64,
    destination: Pubkey,
    nonce: u64,
) -> Result<(), solana_program_test::BanksClientError> {
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (record_pda, _) = nonce_pda(&program_id, DIR_SOLANA_TO_MACHINE, nonce);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Deposit {
            amount,
            destination,
            nonce,
        },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(env.user_token_account, false),
            AccountMeta::new(env.vault, false),
            AccountMeta::new(record_pda, false),
            AccountMeta::new_readonly(env.user.pubkey(), true),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer, &env.user],
        blockhash,
    );
    env.context.banks_client.process_transaction(tx).await
}

async fn send_unlock(
    env: &mut Env,
    message: BridgeMessage,
    signatures: Vec<RelayerSignature>,
) -> Result<(), solana_program_test::BanksClientError> {
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (nonce_pda, _) = nonce_pda(&program_id, DIR_MACHINE_TO_SOLANA, message.nonce);
    let (authority_pda, _) = authority_pda(&program_id);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ed25519_ix = build_ed25519_instruction(&message.hash(), &signatures);
    let ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Unlock { message },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(env.vault, false),
            AccountMeta::new(env.user_token_account, false),
            AccountMeta::new(nonce_pda, false),
            AccountMeta::new_readonly(authority_pda, false),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
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
async fn deposit_locks_tokens_in_vault() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 50_000);
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.user_token_account).await,
        1_950_000
    );
    assert_eq!(total_locked(&mut env).await, 50_000);
}

#[tokio::test]
async fn deposit_same_nonce_twice_fails() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    warp(&mut env).await;
    let again = send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1).await;
    assert_custom_error(again, BridgeError::NonceAlreadyProcessed);
}

#[tokio::test]
async fn deposit_over_vault_ceiling_fails() {
    let mut env = setup(100_000, 500_000, 100_000).await;
    let result = send_deposit(&mut env, 100_001, Pubkey::new_unique(), 1).await;
    assert_custom_error(result, BridgeError::VaultCeilingExceeded);
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 0);
}

#[tokio::test]
async fn unlock_with_valid_3_of_5_burn_attestation() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    // 500 is exactly 1% of the 50,000 locked — the dynamic per-tx cap
    // boundary; it must pass.
    let msg = burn_message(1, 500, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_unlock(&mut env, msg, sigs).await.unwrap();
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 49_500);
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.user_token_account).await,
        1_950_500
    );
    assert_eq!(total_locked(&mut env).await, 49_500);
}

#[tokio::test]
async fn unlock_replay_fails() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let msg = burn_message(1, 500, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_unlock(&mut env, msg.clone(), sigs.clone())
        .await
        .unwrap();
    warp(&mut env).await;
    let again = send_unlock(&mut env, msg, sigs).await;
    assert_custom_error(again, BridgeError::NonceAlreadyProcessed);
}

#[tokio::test]
async fn unlock_over_dynamic_per_tx_cap_fails() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    // 501 is just over 1% of the 50,000 locked, and far below the
    // absolute per-tx cap of 100,000 — only the dynamic layer can
    // reject this.
    let msg = burn_message(1, 501, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_unlock(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::AmountExceedsDynamicPerTxCap);
}

#[tokio::test]
async fn unlock_with_reordered_signatures_succeeds() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let msg = burn_message(1, 500, env.user.pubkey(), env.source_mint);
    let mut sigs = sign_attestation(&msg, &env.relayers, 3);
    sigs.reverse();
    send_unlock(&mut env, msg, sigs).await.unwrap();
    assert_eq!(total_locked(&mut env).await, 49_500);
}

#[tokio::test]
async fn unlock_2_of_5_fails() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let msg = burn_message(1, 30_000, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 2);
    let result = send_unlock(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::QuorumNotReached);
}

#[tokio::test]
async fn unlock_over_per_tx_cap_fails() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 150_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let msg = burn_message(1, 100_001, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_unlock(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::AmountExceedsPerTxCap);
}

#[tokio::test]
async fn unlock_over_dynamic_daily_cap_fails() {
    // Absolute caps are set high (400k per-tx, 500k daily) so only the
    // dynamic layer can bind. Each 10,000 unlock is well under 1% of
    // the ~1.8M locked (per-tx dynamic passes throughout), but the
    // window total may not pass 5% of the locked total: eight unlocks
    // total 80,000 (allowance ~86,500); the ninth would reach 90,000
    // against an allowance of ~86,000.
    let mut env = setup(400_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 1_800_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    for nonce in 1..=8u64 {
        let msg = burn_message(nonce, 10_000, env.user.pubkey(), env.source_mint);
        let sigs = sign_attestation(&msg, &env.relayers, 3);
        send_unlock(&mut env, msg, sigs).await.unwrap();
    }
    let msg9 = burn_message(9, 10_000, env.user.pubkey(), env.source_mint);
    let sigs9 = sign_attestation(&msg9, &env.relayers, 3);
    let result = send_unlock(&mut env, msg9, sigs9).await;
    assert_custom_error(result, BridgeError::AmountExceedsDynamicDailyCap);
}

#[tokio::test]
async fn admin_signatures_cannot_unlock() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let msg = burn_message(1, 30_000, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.admins, 3);
    let result = send_unlock(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::UnknownRelayer);
}

#[tokio::test]
async fn pause_blocks_deposit_and_unlock() {
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);

    // Pause with 2 relayers.
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let pause_ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Pause,
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

    let result = send_deposit(&mut env, 10_000, Pubkey::new_unique(), 2).await;
    assert_custom_error(result, BridgeError::BridgePaused);
    let msg = burn_message(1, 10_000, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_unlock(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::BridgePaused);

    // Unpause by relayers fails (multisig-only, BRIDGE.md 4.3).
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let unpause_ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Unpause,
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

    // Unpause by 2 admins; deposit works again.
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let unpause_ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Unpause,
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
    send_deposit(&mut env, 10_000, Pubkey::new_unique(), 2)
        .await
        .unwrap();
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 60_000);
}

#[tokio::test]
async fn compute_units_unlock() {
    // Measurement test: prints compute units for a full Unlock
    // (ed25519 precompile + lock instruction), for bridge/VERIFY.md.
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let msg = burn_message(1, 500, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (nonce_pda, _) = nonce_pda(&program_id, DIR_MACHINE_TO_SOLANA, msg.nonce);
    let (authority_pda, _) = authority_pda(&program_id);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ed25519_ix = build_ed25519_instruction(&msg.hash(), &sigs);
    let ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Unlock { message: msg },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(env.vault, false),
            AccountMeta::new(env.user_token_account, false),
            AccountMeta::new(nonce_pda, false),
            AccountMeta::new_readonly(authority_pda, false),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
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
    println!("CU Unlock (3-of-5, incl. ed25519 precompile): {cu}");
    assert!(cu < 200_000, "Unlock CU unexpectedly high: {cu}");
}

#[tokio::test]
async fn compute_units_deposit() {
    // Measurement test: prints compute units for a Deposit, for
    // bridge/VERIFY.md.
    let env = setup(100_000, 500_000, 2_000_000).await;
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (record_pda, _) = nonce_pda(&program_id, DIR_SOLANA_TO_MACHINE, 1);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Deposit {
            amount: 10_000,
            destination: Pubkey::new_unique(),
            nonce: 1,
        },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(env.user_token_account, false),
            AccountMeta::new(env.vault, false),
            AccountMeta::new(record_pda, false),
            AccountMeta::new_readonly(env.user.pubkey(), true),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer, &env.user],
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
    println!("CU Deposit: {cu}");
    assert!(cu < 200_000, "Deposit CU unexpectedly high: {cu}");
}
