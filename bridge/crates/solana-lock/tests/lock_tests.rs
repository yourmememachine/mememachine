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

/// Fixed source-event timestamp for attestation fixtures (A-F1 fix,
/// 2026-11-20: expiry is timestamp-based). Tests pin the Clock
/// sysvar's unix_timestamp with `set_clock_ts`; `warp_to_slot` is
/// still used for the slot-based cap windows and does not move the
/// timestamp reliably.
const TEST_SOURCE_TS: i64 = 1_700_000_000;
/// The destination "now" the suites pin before attestation-bearing
/// sends: one minute after the fixture source event, far inside the
/// fixtures' 40,000-second expiry window.
const TEST_NOW: i64 = TEST_SOURCE_TS + 60;

/// Set the Clock sysvar's unix_timestamp (slot untouched) and prove
/// it landed by reading the sysvar back. If this assert ever fires,
/// `set_sysvar` is not taking effect in this program-test version —
/// stop and escalate; do not fall back to warp_to_slot silently.
async fn set_clock_ts(env: &mut Env, ts: i64) {
    let mut clock: solana_program::clock::Clock =
        env.context.banks_client.get_sysvar().await.unwrap();
    clock.unix_timestamp = ts;
    env.context.set_sysvar(&clock);
    let readback: solana_program::clock::Clock =
        env.context.banks_client.get_sysvar().await.unwrap();
    assert_eq!(
        readback.unix_timestamp, ts,
        "harness clock probe: set_sysvar did not take effect"
    );
}

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
        expiry_timestamp: TEST_SOURCE_TS + 40_000,
        source_timestamp: TEST_SOURCE_TS,
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

/// Native (builtin) wrapper for the SPL Token program, as a fn pointer.
///
/// `ProgramTest::add_builtin_program` takes a bare fn pointer, while the
/// `processor!` macro yields an `Option<closure>` whose lifetime is not
/// higher-ranked, so the macro cannot be used here directly. This is the
/// same body the macro generates.
fn spl_token_builtin<'a>(
    vm: *mut solana_program_test::EbpfVm<'a, solana_program_test::InvokeContext<'static>>,
    _arg0: u64,
    _arg1: u64,
    _arg2: u64,
    _arg3: u64,
    _arg4: u64,
) {
    let vm = unsafe {
        &mut *((vm as *mut u64)
            .offset(-(solana_program_test::get_runtime_environment_key() as isize))
            as *mut solana_program_test::EbpfVm<solana_program_test::InvokeContext>)
    };
    vm.program_result = solana_program_test::invoke_builtin_function(
        spl_token::processor::Processor::process,
        vm.context_object_pointer,
    )
    .map_err(|err| solana_program_test::EbpfError::SyscallError(err))
    .into();
}

/// The program's ProgramData address under the BPF upgradeable
/// loader — where `verify_upgrade_authority` looks for the upgrade
/// authority `Initialize` is bound to (self-audit F-1).
#[allow(deprecated)]
fn programdata_address() -> Pubkey {
    solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id())
}

/// Seed a ProgramData account naming `authority` as the program's
/// upgrade authority. The bytes are the loader's bincode layout —
/// variant tag 3 (ProgramData), slot, option tag, authority — the
/// layout bridge-common parses. Native program-test has no real
/// loader state for a `processor!` program, so the account is
/// constructed directly (the harness CAN supply it; the check is
/// fully exercised).
#[allow(deprecated)]
fn seed_programdata(test: &mut ProgramTest, authority: &Pubkey) {
    let mut data = vec![0u8; 45];
    data[0..4].copy_from_slice(&3u32.to_le_bytes());
    data[12] = 1;
    data[13..45].copy_from_slice(authority.as_ref());
    test.add_account(
        programdata_address(),
        Account {
            lamports: 10_000_000,
            data,
            owner: solana_program::bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
}

/// Build the Initialize instruction (account list per the F-1
/// layout: payer, config, vault, system program, programdata).
fn initialize_ix(
    payer: &Pubkey,
    relayers: [Pubkey; 5],
    admins: [Pubkey; 3],
    source_mint: Pubkey,
    vault: Pubkey,
    per_tx_cap: u64,
    daily_cap: u64,
    vault_ceiling: u64,
) -> Instruction {
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Initialize {
            relayers,
            admins,
            source_mint,
            vault,
            per_tx_cap,
            daily_cap,
            vault_ceiling,
        },
        vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(config_pda, false),
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(programdata_address(), false),
        ],
    )
}

async fn setup(per_tx_cap: u64, daily_cap: u64, vault_ceiling: u64) -> Env {
    let program_id = program_id();
    let deployer = Keypair::new();
    let mut program_test = ProgramTest::new(
        "solana_lock",
        program_id,
        solana_program_test::processor!(solana_lock::processor::process),
    );
    // SPL Token is a builtin (native) program on-chain, so register it as a
    // builtin: `add_program` would panic in SBF mode (BPF_OUT_DIR set) because
    // no spl_token.so exists to load.
    program_test.add_builtin_program("spl_token", spl_token::id(), spl_token_builtin);
    // The deployer is the program's upgrade authority; Initialize
    // is bound to it (self-audit F-1).
    seed_programdata(&mut program_test, &deployer.pubkey());
    let mut context = program_test.start_with_context().await;
    let recent_blockhash = context.last_blockhash;

    let relayers = relayer_keypairs();
    let admins = admin_keypairs();
    let user = Keypair::new();
    let mint_authority = Keypair::new();
    let (vault_authority, _) = authority_pda(&program_id);

    // Fund the deployer so it can pay the config account's rent.
    let fund_tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &context.payer.pubkey(),
            &deployer.pubkey(),
            1_000_000_000,
        )],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        recent_blockhash,
    );
    context.banks_client.process_transaction(fund_tx).await.unwrap();

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

    let init_ix = initialize_ix(
        &deployer.pubkey(),
        relayer_pubkeys(&relayers),
        admin_pubkeys(&admins),
        source_mint,
        vault,
        per_tx_cap,
        daily_cap,
        vault_ceiling,
    );
    let tx = Transaction::new_signed_with_payer(
        &[init_ix],
        Some(&context.payer.pubkey()),
        &[&context.payer, &deployer],
        recent_blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();

    let mut env = Env {
        context,
        relayers,
        admins,
        source_mint,
        user,
        user_token_account,
        vault,
    };
    // Pin the destination clock for every attestation-bearing test
    // (see TEST_NOW). The harness clock starts at wall-clock time,
    // which is years past the fixtures' expiry window.
    set_clock_ts(&mut env, TEST_NOW).await;
    env
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
    let (receipt_pda, _) = receipt_pda(&program_id, DIR_MACHINE_TO_SOLANA, message.nonce);
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
async fn unlock_at_expiry_boundary() {
    // War room 2026-11-21 (F-TEST-4): the lock side's expiry wiring
    // (processor.rs Clock unix_timestamp) had no negative test —
    // a slot-for-timestamp mutant there survived. Mirrors
    // mint_at_expiry_boundary on the Machine side. The suite clock
    // is pinned at TEST_NOW (see setup).
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    // At the exact expiry timestamp the attestation is still valid
    // (rejection is current_timestamp > expiry_timestamp).
    let mut msg = burn_message(1, 500, env.user.pubkey(), env.source_mint);
    msg.expiry_timestamp = TEST_NOW;
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_unlock(&mut env, msg, sigs).await.unwrap();
    // One second past expiry, the same-shaped attestation is dead.
    let mut msg2 = burn_message(2, 500, env.user.pubkey(), env.source_mint);
    msg2.expiry_timestamp = TEST_NOW - 1;
    let sigs2 = sign_attestation(&msg2, &env.relayers, 3);
    let result = send_unlock(&mut env, msg2, sigs2).await;
    assert_custom_error(result, BridgeError::ExpiredAttestation);
}

#[tokio::test]
async fn relayer_rotation_requires_admin_multisig_and_takes_effect() {
    // Rotation path on the release side (added 2026-11-25, daily
    // build): the Cyber bridge post-mortem (24 Nov 2026) turned on
    // revoking one compromised verifier key — and this side is the
    // one that releases locked reserves, the lockbox analogue. The
    // mint side has had this test since the rotation path shipped;
    // the lock side's SetRelayers handler had none. The set can be
    // replaced only by the admin multisig — never by a single
    // admin, never by relayers themselves — and the new set is the
    // only one that counts from that moment on.
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let new_relayers: [Keypair; 5] = std::array::from_fn(|_| Keypair::new());
    let new_pubkeys = relayer_pubkeys(&new_relayers);

    fn set_relayers_ix(
        program_id: Pubkey,
        config: Pubkey,
        signer_pubs: &[Pubkey],
        relayers: [Pubkey; 5],
    ) -> Instruction {
        let mut metas = vec![AccountMeta::new(config, false)];
        for p in signer_pubs {
            metas.push(AccountMeta::new_readonly(*p, true));
        }
        Instruction::new_with_borsh(
            program_id,
            &LockInstruction::SetRelayers { relayers },
            metas,
        )
    }
    async fn send_admin_ix(
        env: &mut Env,
        ix: Instruction,
        signer_idx: &[usize],
        relayers_sign: bool,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let blockhash = env
            .context
            .banks_client
            .get_latest_blockhash()
            .await
            .unwrap();
        let mut all: Vec<&Keypair> = vec![&env.context.payer];
        for &i in signer_idx {
            all.push(if relayers_sign {
                &env.relayers[i]
            } else {
                &env.admins[i]
            });
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
    let relayer_pubs: Vec<Pubkey> = env.relayers.iter().map(|k| k.pubkey()).collect();

    // One admin is below the 2-of-3 threshold.
    let ix = set_relayers_ix(program_id, config_pda, &admin_pubs[..1], new_pubkeys);
    let result = send_admin_ix(&mut env, ix, &[0], false).await;
    assert_custom_error(result, BridgeError::Unauthorized);

    // Two relayers are not admins at any count: the set cannot
    // rotate itself (the compromised-key case — the key's owner
    // must not be able to entrench or replace the set).
    let ix = set_relayers_ix(program_id, config_pda, &relayer_pubs[..2], new_pubkeys);
    let result = send_admin_ix(&mut env, ix, &[0, 1], true).await;
    assert_custom_error(result, BridgeError::Unauthorized);

    // Two admins, but the proposed set repeats a key.
    let mut dup = new_pubkeys;
    dup[4] = dup[0];
    let ix = set_relayers_ix(program_id, config_pda, &admin_pubs[..2], dup);
    let result = send_admin_ix(&mut env, ix, &[0, 1], false).await;
    assert_custom_error(result, BridgeError::DuplicateRelayerSignature);

    // Two admins and a clean set: rotation succeeds.
    let ix = set_relayers_ix(program_id, config_pda, &admin_pubs[..2], new_pubkeys);
    send_admin_ix(&mut env, ix, &[0, 1], false)
        .await
        .unwrap();

    // The old set's attestations no longer count — a revoked
    // (compromised) key's quorum releases nothing.
    let msg = burn_message(1, 500, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_unlock(&mut env, msg.clone(), sigs).await;
    assert_custom_error(result, BridgeError::UnknownRelayer);
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 50_000);

    // The new set's attestations unlock.
    let sigs = sign_attestation(&msg, &new_relayers, 3);
    send_unlock(&mut env, msg, sigs).await.unwrap();
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 49_500);
    assert_eq!(total_locked(&mut env).await, 49_500);
}

#[tokio::test]
async fn rotation_while_paused_takes_effect_without_unpausing() {
    // The full incident-response sequence (added 2026-11-26, daily
    // build): pause first, rotate the compromised set out while
    // paused, and the bridge must stay paused until a separate
    // admin unpause — rotation must neither be blocked by the
    // pause (or a compromised set could never be removed during
    // an incident) nor silently end it. The Cyber post-mortem
    // follow-up (Bitquery, 22 Nov 2026) is the cautionary shape:
    // a third forged message was approved the morning after the
    // first rotation because both keys the thief had used were
    // still accepted — so here the old set must stay dead after
    // the sequence completes, not just immediately after rotation.
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let new_relayers: [Keypair; 5] = std::array::from_fn(|_| Keypair::new());
    let new_pubkeys = relayer_pubkeys(&new_relayers);

    async fn send_ix(
        env: &mut Env,
        ix: Instruction,
        signer_idx: &[usize],
        relayers_sign: bool,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let blockhash = env
            .context
            .banks_client
            .get_latest_blockhash()
            .await
            .unwrap();
        let mut all: Vec<&Keypair> = vec![&env.context.payer];
        for &i in signer_idx {
            all.push(if relayers_sign {
                &env.relayers[i]
            } else {
                &env.admins[i]
            });
        }
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&env.context.payer.pubkey()),
            &all,
            blockhash,
        );
        env.context.banks_client.process_transaction(tx).await
    }
    fn admin_ix(
        program_id: Pubkey,
        config: Pubkey,
        signer_pubs: &[Pubkey],
        data: &LockInstruction,
    ) -> Instruction {
        let mut metas = vec![AccountMeta::new(config, false)];
        for p in signer_pubs {
            metas.push(AccountMeta::new_readonly(*p, true));
        }
        Instruction::new_with_borsh(program_id, data, metas)
    }
    let admin_pubs: Vec<Pubkey> = env.admins.iter().map(|k| k.pubkey()).collect();
    let relayer_pubs: Vec<Pubkey> = env.relayers.iter().map(|k| k.pubkey()).collect();

    // Pause with 2 of the (still current) relayers.
    let ix = admin_ix(
        program_id,
        config_pda,
        &relayer_pubs[..2],
        &LockInstruction::Pause,
    );
    send_ix(&mut env, ix, &[0, 1], true).await.unwrap();

    // Rotation succeeds while paused: 2-of-3 admins replace the set.
    let ix = admin_ix(
        program_id,
        config_pda,
        &admin_pubs[..2],
        &LockInstruction::SetRelayers {
            relayers: new_pubkeys,
        },
    );
    send_ix(&mut env, ix, &[0, 1], false).await.unwrap();

    // Rotation did not unpause: even the new set's quorum is
    // refused while paused, and nothing leaves the vault.
    // (Amount 100 stays under the tightened post-unpause dynamic
    // per-tx cap — 25 bps of the 50,000 locked is 125 — so the
    // later successful unlock isolates the set change, not caps.)
    let msg = burn_message(1, 100, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &new_relayers, 3);
    let result = send_unlock(&mut env, msg.clone(), sigs).await;
    assert_custom_error(result, BridgeError::BridgePaused);
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 50_000);

    // Separate admin unpause.
    let ix = admin_ix(
        program_id,
        config_pda,
        &admin_pubs[..2],
        &LockInstruction::Unpause,
    );
    send_ix(&mut env, ix, &[0, 1], false).await.unwrap();

    // The old set is still dead after the full sequence.
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_unlock(&mut env, msg.clone(), sigs).await;
    assert_custom_error(result, BridgeError::UnknownRelayer);
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 50_000);

    // The new set unlocks.
    let sigs = sign_attestation(&msg, &new_relayers, 3);
    send_unlock(&mut env, msg, sigs).await.unwrap();
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 49_900);
    assert_eq!(total_locked(&mut env).await, 49_900);
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
    let (receipt_pda, _) = receipt_pda(&program_id, DIR_MACHINE_TO_SOLANA, msg.nonce);
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
    println!("CU Unlock (3-of-5, execution CU; +7,200 precompile charge on-chain): {cu}");
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

#[tokio::test]
async fn unpause_tightens_dynamic_caps_for_one_window() {
    // Automatic cap tightening: a pause/unpause cycle reopens the
    // bridge at a quarter of its normal dynamic limits (0.25% per
    // tx instead of 1%) for one full cap window, then relaxes on
    // its own. Vault holds 2,000,000, so the normal per-tx dynamic
    // cap is 20,000 and the tightened one is 5,000.
    let mut env = setup(2_000_000, 4_000_000, 2_000_000).await;
    send_deposit(&mut env, 2_000_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);

    // Pause with 2 relayers, unpause with 2 admins.
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

    // The unpause recorded a tightening deadline one window out.
    let acc = env.context.banks_client.get_account(config_pda).await.unwrap().unwrap();
    let cfg = LockConfig::try_from_slice(&acc.data).unwrap();
    let clock: solana_sdk::clock::Clock =
        env.context.banks_client.get_sysvar().await.unwrap();
    assert!(cfg.tightened_until_slot > clock.slot);
    assert!(cfg.tightened_until_slot <= clock.slot + 216_000);

    // 10,000 is 0.5% of the locked total: fine at normal caps,
    // too much while tightened.
    let msg = burn_message(1, 10_000, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_unlock(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::AmountExceedsDynamicPerTxCap);

    // 5,000 is exactly the tightened 0.25%: it goes through, and
    // the unlock leaves its crossing receipt.
    let msg = burn_message(1, 5_000, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_unlock(&mut env, msg.clone(), sigs).await.unwrap();
    let (receipt_addr, _) = receipt_pda(&program_id, DIR_MACHINE_TO_SOLANA, 1);
    let acc = env
        .context
        .banks_client
        .get_account(receipt_addr)
        .await
        .unwrap()
        .expect("receipt PDA must exist after a successful unlock");
    let receipt = CrossingReceipt::try_from_slice(&acc.data).unwrap();
    assert_eq!(receipt.discriminator, DISCRIMINATOR_RECEIPT);
    assert_eq!(receipt.direction, DIR_MACHINE_TO_SOLANA);
    assert_eq!(receipt.nonce, 1);
    assert_eq!(receipt.message_hash, msg.hash());
    assert_eq!(receipt.source_tx, msg.source_tx);
    assert_eq!(receipt.amount, 5_000);
    assert_eq!(receipt.recipient, env.user.pubkey());

    // Warp past the tightening deadline: the same 10,000 unlock
    // that was rejected now passes under the normal 1% cap.
    env.context.warp_to_slot(cfg.tightened_until_slot + 10).unwrap();
    let msg = burn_message(2, 10_000, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_unlock(&mut env, msg, sigs).await.unwrap();
    assert_eq!(total_locked(&mut env).await, 2_000_000 - 5_000 - 10_000);
}

// ---------------------------------------------------------------------------
// Self-audit regression tests (2026-11-16, REVIEW-1)
//
// F-1: Initialize is bound to the program's upgrade authority.
// F-2: pre-funded PDAs are adopted, not permanent blocks.
// F-3: Initialize applies SetRelayers' member-set validation.
// K-1: the token program is pinned explicitly in the handlers.
// ---------------------------------------------------------------------------

/// A started context with the programdata account seeded for a
/// deployer, plus a mint and a vault — but NOT initialized, so a
/// test can drive Initialize itself.
struct InitFixture {
    context: solana_program_test::ProgramTestContext,
    deployer: Keypair,
    source_mint: Pubkey,
    vault: Pubkey,
}

async fn init_fixture() -> InitFixture {
    let program_id = program_id();
    let deployer = Keypair::new();
    let mut program_test = ProgramTest::new(
        "solana_lock",
        program_id,
        solana_program_test::processor!(solana_lock::processor::process),
    );
    program_test.add_builtin_program("spl_token", spl_token::id(), spl_token_builtin);
    seed_programdata(&mut program_test, &deployer.pubkey());
    let mut context = program_test.start_with_context().await;
    let recent_blockhash = context.last_blockhash;
    let fund_tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &context.payer.pubkey(),
            &deployer.pubkey(),
            1_000_000_000,
        )],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        recent_blockhash,
    );
    context.banks_client.process_transaction(fund_tx).await.unwrap();
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
    let vault = create_token_account(
        &mut context.banks_client,
        &context.payer,
        &recent_blockhash,
        &source_mint,
        &vault_authority,
    )
    .await;
    InitFixture {
        context,
        deployer,
        source_mint,
        vault,
    }
}

async fn send_init(
    fx: &mut InitFixture,
    payer: &Keypair,
    relayers: [Pubkey; 5],
    admins: [Pubkey; 3],
) -> Result<(), solana_program_test::BanksClientError> {
    let ix = initialize_ix(
        &payer.pubkey(),
        relayers,
        admins,
        fx.source_mint,
        fx.vault,
        1_000_000,
        10_000_000,
        1_000_000_000,
    );
    let blockhash = fx.context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&fx.context.payer.pubkey()),
        &[&fx.context.payer, payer],
        blockhash,
    );
    fx.context.banks_client.process_transaction(tx).await
}

/// Initialize with the fixture's own deployer (the upgrade
/// authority) as payer.
async fn send_init_as_deployer(
    fx: &mut InitFixture,
    relayers: [Pubkey; 5],
    admins: [Pubkey; 3],
) -> Result<(), solana_program_test::BanksClientError> {
    let ix = initialize_ix(
        &fx.deployer.pubkey(),
        relayers,
        admins,
        fx.source_mint,
        fx.vault,
        1_000_000,
        10_000_000,
        1_000_000_000,
    );
    let blockhash = fx.context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&fx.context.payer.pubkey()),
        &[&fx.context.payer, &fx.deployer],
        blockhash,
    );
    fx.context.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn stranger_cannot_initialize_but_upgrade_authority_can() {
    // Self-audit F-1: before the fix, the first caller after
    // deployment — anyone — set the relayers, admins, and caps.
    let mut fx = init_fixture().await;
    let relayers = relayer_keypairs();
    let admins = admin_keypairs();
    let stranger = Keypair::new();
    let blockhash = fx.context.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &fx.context.payer.pubkey(),
            &stranger.pubkey(),
            1_000_000_000,
        )],
        Some(&fx.context.payer.pubkey()),
        &[&fx.context.payer],
        blockhash,
    );
    fx.context.banks_client.process_transaction(fund_tx).await.unwrap();

    let result = send_init(
        &mut fx,
        &stranger,
        relayer_pubkeys(&relayers),
        admin_pubkeys(&admins),
    )
    .await;
    assert_custom_error(result, BridgeError::Unauthorized);
    // No config was created by the stranger's attempt.
    let (config_pda, _) = config_pda(&program_id());
    assert!(fx
        .context
        .banks_client
        .get_account(config_pda)
        .await
        .unwrap()
        .is_none());

    // The upgrade authority's Initialize goes through and stores
    // exactly its sets.
    send_init_as_deployer(
        &mut fx,
        relayer_pubkeys(&relayers),
        admin_pubkeys(&admins),
    )
    .await
    .unwrap();
    let acc: Account = fx
        .context
        .banks_client
        .get_account(config_pda)
        .await
        .unwrap()
        .unwrap();
    let cfg = LockConfig::try_from_slice(&acc.data).unwrap();
    assert_eq!(cfg.relayers.relayers, relayer_pubkeys(&relayers));
    assert_eq!(cfg.admins.admins, admin_pubkeys(&admins));
}

#[tokio::test]
async fn initialize_rejects_duplicate_and_default_members() {
    // Self-audit F-3: SetRelayers always rejected these; Initialize
    // only rejected default keys, so a "3-of-5" could silently be
    // 3-of-3-distinct.
    let mut fx = init_fixture().await;

    let mut dup_relayers = relayer_pubkeys(&relayer_keypairs());
    dup_relayers[1] = dup_relayers[0];
    dup_relayers[2] = dup_relayers[0];
    let result = send_init_as_deployer(
        &mut fx,
        dup_relayers,
        admin_pubkeys(&admin_keypairs()),
    )
    .await;
    assert_custom_error(result, BridgeError::DuplicateRelayerSignature);

    let mut dup_admins = admin_pubkeys(&admin_keypairs());
    dup_admins[2] = dup_admins[0];
    let result = send_init_as_deployer(
        &mut fx,
        relayer_pubkeys(&relayer_keypairs()),
        dup_admins,
    )
    .await;
    assert_custom_error(result, BridgeError::DuplicateRelayerSignature);

    let mut default_relayer = relayer_pubkeys(&relayer_keypairs());
    default_relayer[4] = Pubkey::default();
    let result = send_init_as_deployer(
        &mut fx,
        default_relayer,
        admin_pubkeys(&admin_keypairs()),
    )
    .await;
    assert_custom_error(result, BridgeError::InvalidState);
}

#[tokio::test]
async fn initialize_adopts_prefunded_config_pda() {
    // Self-audit F-2, config variant: a dust transfer to the config
    // PDA before deployment used to block Initialize forever. (The
    // amount is the rent-exempt minimum for an empty account — the
    // smallest transfer that can create the shell.)
    let mut fx = init_fixture().await;
    let (config_pda, _) = config_pda(&program_id());
    let blockhash = fx.context.banks_client.get_latest_blockhash().await.unwrap();
    let grief_tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &fx.context.payer.pubkey(),
            &config_pda,
            1_000_000,
        )],
        Some(&fx.context.payer.pubkey()),
        &[&fx.context.payer],
        blockhash,
    );
    fx.context.banks_client.process_transaction(grief_tx).await.unwrap();

    send_init_as_deployer(
        &mut fx,
        relayer_pubkeys(&relayer_keypairs()),
        admin_pubkeys(&admin_keypairs()),
    )
    .await
    .unwrap();
    let acc: Account = fx
        .context
        .banks_client
        .get_account(config_pda)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acc.owner, program_id());
}

#[tokio::test]
async fn unlock_succeeds_with_prefunded_nonce_and_receipt_pdas() {
    // Self-audit F-2: pre-funding the destination nonce PDA (and
    // the receipt PDA) used to brick the crossing permanently —
    // the source-side burn had already happened.
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let program_id = program_id();
    let (nonce_addr, _) = nonce_pda(&program_id, DIR_MACHINE_TO_SOLANA, 1);
    let (receipt_addr, _) = receipt_pda(&program_id, DIR_MACHINE_TO_SOLANA, 1);
    for target in [nonce_addr, receipt_addr] {
        let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
        let grief_tx = Transaction::new_signed_with_payer(
            &[system_instruction::transfer(
                &env.context.payer.pubkey(),
                &target,
                1_000_000,
            )],
            Some(&env.context.payer.pubkey()),
            &[&env.context.payer],
            blockhash,
        );
        env.context.banks_client.process_transaction(grief_tx).await.unwrap();
    }
    let msg = burn_message(1, 500, env.user.pubkey(), env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    send_unlock(&mut env, msg, sigs).await.unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.vault).await,
        49_500
    );
    assert_eq!(total_locked(&mut env).await, 49_500);
}

#[tokio::test]
async fn deposit_succeeds_with_prefunded_record_pda() {
    // Self-audit F-2, deposit-record variant.
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    let (record_addr, _) = nonce_pda(&program_id(), DIR_SOLANA_TO_MACHINE, 1);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let grief_tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &env.context.payer.pubkey(),
            &record_addr,
            1_000_000,
        )],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer],
        blockhash,
    );
    env.context.banks_client.process_transaction(grief_tx).await.unwrap();
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.vault).await,
        50_000
    );
}

#[tokio::test]
async fn deposit_with_substitute_token_program_fails() {
    // Self-audit K-1 hardening: the token program is pinned in the
    // handler itself now, not only inside spl-token's CPI builders.
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (record_pda, _) = nonce_pda(&program_id, DIR_SOLANA_TO_MACHINE, 1);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Deposit {
            amount: 50_000,
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
            AccountMeta::new_readonly(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer, &env.user],
        blockhash,
    );
    let result = env.context.banks_client.process_transaction(tx).await;
    assert_custom_error(result, BridgeError::InvalidAccountData);
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.vault).await,
        0
    );
}


// ---------------------------------------------------------------------------
// Debate round 2026-11-17 (scope 1): regression tests for the findings
// that survived the two-reviewer debate.
// ---------------------------------------------------------------------------

async fn send_unlock_to(
    env: &mut Env,
    message: BridgeMessage,
    signatures: Vec<RelayerSignature>,
    destination: Pubkey,
) -> Result<(), solana_program_test::BanksClientError> {
    let program_id = program_id();
    let (config_pda, _) = config_pda(&program_id);
    let (nonce_pda, _) = nonce_pda(&program_id, DIR_MACHINE_TO_SOLANA, message.nonce);
    let (receipt_pda, _) = receipt_pda(&program_id, DIR_MACHINE_TO_SOLANA, message.nonce);
    let (authority_pda, _) = authority_pda(&program_id);
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    let ed25519_ix = build_ed25519_instruction(&message.hash(), &signatures);
    let ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Unlock { message },
        vec![
            AccountMeta::new(config_pda, false),
            AccountMeta::new(env.vault, false),
            AccountMeta::new(destination, false),
            AccountMeta::new(nonce_pda, false),
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
async fn debate_unlock_with_wrong_source_mint_fails() {
    // B1/A4: an attestation naming a different source mint than the
    // one this deployment custodies must not release vault tokens,
    // even with a valid quorum.
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let wrong_mint = Pubkey::new_unique();
    let msg = burn_message(1, 500, env.user.pubkey(), wrong_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let result = send_unlock(&mut env, msg, sigs).await;
    assert_custom_error(result, BridgeError::InvalidAccountData);
    assert_eq!(
        token_balance(&mut env.context.banks_client, &env.vault).await,
        50_000
    );
    assert_eq!(total_locked(&mut env).await, 50_000);
}

#[tokio::test]
async fn debate_unlock_to_vault_itself_fails() {
    // A5: the vault as its own unlock destination would debit
    // total_locked while the token transfer moves nothing.
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    send_deposit(&mut env, 50_000, Pubkey::new_unique(), 1)
        .await
        .unwrap();
    let (authority_pda, _) = authority_pda(&program_id());
    let msg = burn_message(1, 500, authority_pda, env.source_mint);
    let sigs = sign_attestation(&msg, &env.relayers, 3);
    let vault = env.vault;
    let result = send_unlock_to(&mut env, msg, sigs, vault).await;
    assert_custom_error(result, BridgeError::InvalidAccountData);
    assert_eq!(total_locked(&mut env).await, 50_000);
}

#[tokio::test]
async fn debate_initialize_rejects_non_token_program_vault() {
    // B4/A9: a vault account whose bytes parse as a token account
    // (owner = authority PDA, mint = source mint, state initialized)
    // but which is NOT owned by the token program must be rejected
    // at Initialize. Seeded directly, because no normal instruction
    // can write those bytes into a system-owned account.
    let program_id = program_id();
    let deployer = Keypair::new();
    let (vault_authority, _) = authority_pda(&program_id);
    let source_mint = Pubkey::new_unique();
    let fake_vault = Pubkey::new_unique();
    let mut vault_data = vec![0u8; spl_token::state::Account::LEN];
    let fake_state = spl_token::state::Account {
        mint: source_mint,
        owner: vault_authority,
        amount: 0,
        delegate: solana_program::program_option::COption::None,
        state: spl_token::state::AccountState::Initialized,
        is_native: solana_program::program_option::COption::None,
        delegated_amount: 0,
        close_authority: solana_program::program_option::COption::None,
    };
    solana_program::program_pack::Pack::pack_into_slice(&fake_state, &mut vault_data[..]);
    let mut program_test = ProgramTest::new(
        "solana_lock",
        program_id,
        solana_program_test::processor!(solana_lock::processor::process),
    );
    program_test.add_builtin_program("spl_token", spl_token::id(), spl_token_builtin);
    seed_programdata(&mut program_test, &deployer.pubkey());
    program_test.add_account(
        fake_vault,
        Account {
            lamports: 10_000_000,
            data: vault_data,
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut context = program_test.start_with_context().await;
    let recent_blockhash = context.last_blockhash;
    let fund_tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &context.payer.pubkey(),
            &deployer.pubkey(),
            1_000_000_000,
        )],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        recent_blockhash,
    );
    context.banks_client.process_transaction(fund_tx).await.unwrap();
    let ix = initialize_ix(
        &deployer.pubkey(),
        relayer_pubkeys(&relayer_keypairs()),
        admin_pubkeys(&admin_keypairs()),
        source_mint,
        fake_vault,
        100_000,
        500_000,
        2_000_000,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&context.payer.pubkey()),
        &[&context.payer, &deployer],
        blockhash,
    );
    let result = context.banks_client.process_transaction(tx).await;
    assert_custom_error(result, BridgeError::InvalidAccountData);
}

#[tokio::test]
async fn debate_pause_with_non_pda_config_fails() {
    // A6/B5: admin handlers must re-derive the config PDA. Seed a
    // fake LockConfig (attacker's admins) at a non-PDA address owned
    // by the program; a Pause signed by its admins must fail.
    let program_id = program_id();
    let admins = admin_keypairs();
    let fake_config = LockConfig::new(
        relayer_pubkeys(&relayer_keypairs()),
        admin_pubkeys(&admins),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        255,
        100_000,
        500_000,
        2_000_000,
        254,
    );
    let fake_addr = Pubkey::new_unique();
    let mut data = vec![0u8; LockConfig::LEN];
    borsh::to_writer(&mut data[..], &fake_config).unwrap();
    let deployer = Keypair::new();
    let mut program_test = ProgramTest::new(
        "solana_lock",
        program_id,
        solana_program_test::processor!(solana_lock::processor::process),
    );
    program_test.add_builtin_program("spl_token", spl_token::id(), spl_token_builtin);
    seed_programdata(&mut program_test, &deployer.pubkey());
    program_test.add_account(
        fake_addr,
        Account {
            lamports: 10_000_000,
            data,
            owner: program_id,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut context = program_test.start_with_context().await;
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let pause_ix = Instruction::new_with_borsh(
        program_id,
        &LockInstruction::Pause,
        vec![
            AccountMeta::new(fake_addr, false),
            AccountMeta::new_readonly(admins[0].pubkey(), true),
            AccountMeta::new_readonly(admins[1].pubkey(), true),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&context.payer.pubkey()),
        &[&context.payer, &admins[0], &admins[1]],
        blockhash,
    );
    let result = context.banks_client.process_transaction(tx).await;
    assert_custom_error(result, BridgeError::InvalidAccountData);
}

// ---------------------------------------------------------------------------
// Debate round 2026-11-20 (scope 1, rotation 2): regression tests.
// ---------------------------------------------------------------------------

/// Build a vault owned first by a personal key, optionally tainted,
/// then handed to the vault-authority PDA, and attempt Initialize.
/// `preload` mints that many tokens into the vault before handover;
/// `close_auth` leaves a personal close authority on the vault.
async fn tainted_vault_init_result(
    preload: u64,
    close_auth: bool,
) -> Result<(), solana_program_test::BanksClientError> {
    let program_id = program_id();
    let deployer = Keypair::new();
    let mut program_test = ProgramTest::new(
        "solana_lock",
        program_id,
        solana_program_test::processor!(solana_lock::processor::process),
    );
    program_test.add_builtin_program("spl_token", spl_token::id(), spl_token_builtin);
    seed_programdata(&mut program_test, &deployer.pubkey());
    let mut context = program_test.start_with_context().await;
    let recent_blockhash = context.last_blockhash;
    let fund_tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &context.payer.pubkey(),
            &deployer.pubkey(),
            1_000_000_000,
        )],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        recent_blockhash,
    );
    context.banks_client.process_transaction(fund_tx).await.unwrap();
    let mint_authority = Keypair::new();
    let personal = Keypair::new();
    let (vault_authority, _) = authority_pda(&program_id);
    let payer = context.payer.insecure_clone();
    let source_mint = create_mint(
        &mut context.banks_client,
        &payer,
        &recent_blockhash,
        &mint_authority.pubkey(),
        None,
        6,
    )
    .await;
    let vault = create_token_account(
        &mut context.banks_client,
        &payer,
        &recent_blockhash,
        &source_mint,
        &personal.pubkey(),
    )
    .await;
    if preload > 0 {
        mint_to(
            &mut context.banks_client,
            &payer,
            &recent_blockhash,
            &source_mint,
            &vault,
            &mint_authority,
            preload,
        )
        .await;
    }
    if close_auth {
        let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
        let ix = spl_token::instruction::set_authority(
            &spl_token::id(),
            &vault,
            Some(&personal.pubkey()),
            spl_token::instruction::AuthorityType::CloseAccount,
            &personal.pubkey(),
            &[],
        )
        .unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer, &personal],
            blockhash,
        );
        context.banks_client.process_transaction(tx).await.unwrap();
    }
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let ix = spl_token::instruction::set_authority(
        &spl_token::id(),
        &vault,
        Some(&vault_authority),
        spl_token::instruction::AuthorityType::AccountOwner,
        &personal.pubkey(),
        &[],
    )
    .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer, &personal],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await.unwrap();
    let init_ix = initialize_ix(
        &deployer.pubkey(),
        relayer_pubkeys(&relayer_keypairs()),
        admin_pubkeys(&admin_keypairs()),
        source_mint,
        vault,
        1_000_000,
        10_000_000,
        1_000_000_000,
    );
    let blockhash = context.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[init_ix],
        Some(&context.payer.pubkey()),
        &[&context.payer, &deployer],
        blockhash,
    );
    context.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn debate_r2_initialize_rejects_vault_close_authority() {
    // B-F2: a personal close authority survives the AccountOwner
    // transfer (spl-token clears it only for native accounts) and
    // would let one key close an empty vault, bricking the program.
    let result = tainted_vault_init_result(0, true).await;
    assert_custom_error(result, BridgeError::InvalidAccountData);
}

#[tokio::test]
async fn debate_r2_initialize_rejects_preloaded_vault() {
    // B-F3: a vault that already holds tokens breaks the
    // balance == total_locked invariant from genesis and strands
    // the pre-load permanently.
    let result = tainted_vault_init_result(5_000, false).await;
    assert_custom_error(result, BridgeError::InvalidAccountData);
}

#[tokio::test]
async fn debate_r2_deposit_to_default_destination_fails() {
    // B-F4: destination == Pubkey::default() mints wrapped tokens
    // to a keyless account on the Machine side; reject at entry.
    let mut env = setup(100_000, 500_000, 2_000_000).await;
    let result = send_deposit(&mut env, 10_000, Pubkey::default(), 9).await;
    assert_custom_error(result, BridgeError::InvalidAccountData);
    assert_eq!(token_balance(&mut env.context.banks_client, &env.vault).await, 0);
    assert_eq!(total_locked(&mut env).await, 0);
}

