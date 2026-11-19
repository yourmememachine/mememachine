//! Dust floor boundary tests — the configurable minimum lock size
//! `Initialize` sets in the config account, on top of the 10%
//! share rule.
//!
//! The floor exists because the share alone admits dust: a
//! 19-token supply satisfies it with a 2-token lock (self-audit
//! F-4's ceiling division made the share exact, not meaningful).
//! These tests pin the floor's own edges — exactly at, one below,
//! one above, zero — its interaction with the share rule and the
//! ceiling division, and the setup rules around it: registration
//! fails closed before Initialize, Initialize is bound to the
//! upgrade authority and runs exactly once, and the release path
//! never consults the config.
//!
//! Run with:
//!     cd programs && cargo test --release --test dust_floor

use borsh::BorshDeserialize;
use launch_guard::state::*;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    system_instruction,
};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};
use spl_token::{
    instruction::{initialize_account3, initialize_mint2, mint_to, set_authority, AuthorityType},
    state::Mint,
};

fn program_id() -> Pubkey {
    Pubkey::new_from_array([77; 32])
}

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &program_id()).0
}

/// The program's ProgramData address under the BPF upgradeable
/// loader — where `Initialize` looks for the upgrade authority.
#[allow(deprecated)]
fn programdata_address() -> Pubkey {
    solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id())
}

/// Seed a ProgramData account naming `authority` as the program's
/// upgrade authority (loader bincode layout: variant tag 3, slot,
/// option tag, authority — the layout the program parses).
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

fn initialize_ix(authority: &Pubkey, min_lock_amount: u64) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new(pda(&[SEED_CONFIG]), false),
            AccountMeta::new_readonly(programdata_address(), false),
            AccountMeta::new_readonly(solana_program::system_program::id(), false),
        ],
        data: borsh::to_vec(&LaunchInstruction::Initialize { min_lock_amount }).unwrap(),
    }
}

/// Start the harness with the upgrade authority seeded but the
/// config NOT yet initialized.
async fn start_uninitialized() -> (ProgramTestContext, Keypair) {
    let authority = Keypair::new();
    let mut test = ProgramTest::new(
        "launch_guard",
        program_id(),
        processor!(launch_guard::process_instruction),
    );
    seed_programdata(&mut test, &authority.pubkey());
    let ctx = test.start_with_context().await;
    (ctx, authority)
}

/// Start the harness and run Initialize with `floor`.
async fn start_with_floor(floor: u64) -> (ProgramTestContext, Keypair) {
    let (mut ctx, authority) = start_uninitialized().await;
    initialize(&mut ctx, &authority, floor)
        .await
        .expect("initialize");
    (ctx, authority)
}

async fn initialize(
    ctx: &mut ProgramTestContext,
    authority: &Keypair,
    floor: u64,
) -> Result<(), BanksClientError> {
    let fund = system_instruction::transfer(
        &ctx.payer.pubkey(),
        &authority.pubkey(),
        1_000_000_000,
    );
    send(ctx, &[fund], &[]).await?;
    send(ctx, &[initialize_ix(&authority.pubkey(), floor)], &[authority]).await
}

async fn send(
    ctx: &mut ProgramTestContext,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> Result<(), BanksClientError> {
    let payer = ctx.payer.pubkey();
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer), &all, ctx.last_blockhash);
    ctx.banks_client.process_transaction(tx).await
}

fn expect_custom(err: &BanksClientError, code: u32) {
    let BanksClientError::TransactionError(TransactionError::InstructionError(
        _,
        solana_program::instruction::InstructionError::Custom(c),
    )) = err
    else {
        panic!("expected Custom({code}), got {err:?}");
    };
    assert_eq!(*c, code);
}

/// A registrable mint (both authorities revoked) with a
/// caller-chosen supply, all of it in the creator's token account.
async fn make_mint_with_supply(
    ctx: &mut ProgramTestContext,
    creator: &Keypair,
    supply: u64,
) -> (Pubkey, Pubkey) {
    let mint = Keypair::new();
    let token_acct = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let ixs = vec![
        system_instruction::transfer(&ctx.payer.pubkey(), &creator.pubkey(), 5_000_000_000),
        system_instruction::create_account(
            &creator.pubkey(),
            &mint.pubkey(),
            rent.minimum_balance(Mint::LEN),
            Mint::LEN as u64,
            &spl_token::id(),
        ),
        initialize_mint2(
            &spl_token::id(),
            &mint.pubkey(),
            &creator.pubkey(),
            Some(&creator.pubkey()),
            0,
        )
        .unwrap(),
        system_instruction::create_account(
            &creator.pubkey(),
            &token_acct.pubkey(),
            rent.minimum_balance(spl_token::state::Account::LEN),
            spl_token::state::Account::LEN as u64,
            &spl_token::id(),
        ),
        initialize_account3(
            &spl_token::id(),
            &token_acct.pubkey(),
            &mint.pubkey(),
            &creator.pubkey(),
        )
        .unwrap(),
        mint_to(
            &spl_token::id(),
            &mint.pubkey(),
            &token_acct.pubkey(),
            &creator.pubkey(),
            &[],
            supply,
        )
        .unwrap(),
        set_authority(
            &spl_token::id(),
            &mint.pubkey(),
            None,
            AuthorityType::MintTokens,
            &creator.pubkey(),
            &[],
        )
        .unwrap(),
        set_authority(
            &spl_token::id(),
            &mint.pubkey(),
            None,
            AuthorityType::FreezeAccount,
            &creator.pubkey(),
            &[],
        )
        .unwrap(),
    ];
    send(ctx, &ixs, &[creator, &mint, &token_acct])
        .await
        .expect("mint setup");
    (mint.pubkey(), token_acct.pubkey())
}

fn register_ix_with_config(
    creator: &Pubkey,
    mint: &Pubkey,
    creator_token: &Pubkey,
    lock_amount: u64,
    config: Pubkey,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*creator, true),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*creator_token, false),
            AccountMeta::new(pda(&[SEED_LAUNCH, mint.as_ref()]), false),
            AccountMeta::new(pda(&[SEED_VAULT, mint.as_ref()]), false),
            AccountMeta::new_readonly(pda(&[SEED_AUTHORITY, mint.as_ref()]), false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_program::system_program::id(), false),
            AccountMeta::new(pda(&[SEED_REPUTATION, creator.as_ref()]), false),
            AccountMeta::new_readonly(config, false),
        ],
        data: borsh::to_vec(&LaunchInstruction::RegisterAndLock {
            lock_amount,
            lock_slots: MIN_LOCK_SLOTS,
        })
        .unwrap(),
    }
}

fn register_ix(
    creator: &Pubkey,
    mint: &Pubkey,
    creator_token: &Pubkey,
    lock_amount: u64,
) -> Instruction {
    register_ix_with_config(creator, mint, creator_token, lock_amount, pda(&[SEED_CONFIG]))
}

async fn read_record(ctx: &mut ProgramTestContext, mint: &Pubkey) -> Option<LaunchRecord> {
    let acc = ctx
        .banks_client
        .get_account(pda(&[SEED_LAUNCH, mint.as_ref()]))
        .await
        .unwrap()?;
    LaunchRecord::try_from_slice(&acc.data).ok()
}

async fn read_token_amount(ctx: &mut ProgramTestContext, token: &Pubkey) -> u64 {
    let acc = ctx
        .banks_client
        .get_account(*token)
        .await
        .unwrap()
        .expect("token account exists");
    spl_token::state::Account::unpack(&acc.data).unwrap().amount
}

async fn read_reputation(ctx: &mut ProgramTestContext, creator: &Pubkey) -> CreatorReputation {
    let acc = ctx
        .banks_client
        .get_account(pda(&[SEED_REPUTATION, creator.as_ref()]))
        .await
        .unwrap()
        .expect("reputation account exists");
    CreatorReputation::try_from_slice(&acc.data).unwrap()
}

async fn read_config(ctx: &mut ProgramTestContext) -> GuardConfig {
    let acc = ctx
        .banks_client
        .get_account(pda(&[SEED_CONFIG]))
        .await
        .unwrap()
        .expect("config account exists");
    GuardConfig::try_from_slice(&acc.data).unwrap()
}

// ---------------------------------------------------------------------------
// The floor's own boundaries. Supply 1,000: the 10% share is 100,
// so a floor of 500 is the binding minimum throughout.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lock_exactly_at_floor_accepted() {
    let (mut ctx, _authority) = start_with_floor(500).await;
    assert_eq!(read_config(&mut ctx).await.min_lock_amount, 500);
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 500)],
        &[&creator],
    )
    .await
    .expect("lock exactly at the floor registers");
    let rec = read_record(&mut ctx, &mint).await.unwrap();
    assert_eq!(rec.locked_amount, 500);
    assert_eq!(rec.total_supply, 1_000);
}

#[tokio::test]
async fn lock_one_below_floor_rejected() {
    let (mut ctx, _authority) = start_with_floor(500).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 499)],
        &[&creator],
    )
    .await
    .unwrap_err();
    // 499 passes the 10% share (100) — the rejection must name
    // the floor, not the share.
    expect_custom(&err, LaunchError::LockBelowDustFloor as u32);
    assert!(read_record(&mut ctx, &mint).await.is_none());
}

#[tokio::test]
async fn lock_one_above_floor_accepted() {
    let (mut ctx, _authority) = start_with_floor(500).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 501)],
        &[&creator],
    )
    .await
    .expect("lock one above the floor registers");
    assert_eq!(read_record(&mut ctx, &mint).await.unwrap().locked_amount, 501);
}

#[tokio::test]
async fn zero_lock_rejected() {
    let (mut ctx, _authority) = start_with_floor(500).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 0)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);
    assert!(read_record(&mut ctx, &mint).await.is_none());
}

// ---------------------------------------------------------------------------
// Interaction with the 10% share and the ceiling division: the
// effective minimum is max(share rounded up, floor) — whichever
// binds, its error names it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn share_dominates_when_larger_than_floor() {
    let (mut ctx, _authority) = start_with_floor(500).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000_000).await;
    // 10% of 1,000,000 is 100,000 — far above the 500 floor.
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 99_999)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 100_000)],
        &[&creator],
    )
    .await
    .expect("lock at the share registers");
}

#[tokio::test]
async fn floor_stacks_on_top_of_ceiling_division() {
    // Supply 19: the ceiling-divided share is 2. A floor of 3
    // raises the effective minimum past it — a 2-token lock now
    // passes the share and fails the floor.
    let (mut ctx, _authority) = start_with_floor(3).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 19).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 2)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockBelowDustFloor as u32);
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 3)],
        &[&creator],
    )
    .await
    .expect("lock at the floor registers");
    assert_eq!(read_record(&mut ctx, &mint).await.unwrap().locked_amount, 3);
}

#[tokio::test]
async fn ceiling_division_still_binds_under_a_small_floor() {
    // Supply 19, floor 1: the share (2, ceiling-divided) is the
    // binding minimum — a 1-token lock fails the share, not the
    // floor, exactly as it did before the floor existed (F-4).
    let (mut ctx, _authority) = start_with_floor(1).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 19).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 1)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 2)],
        &[&creator],
    )
    .await
    .expect("lock at the ceiling-divided share registers");
}

#[tokio::test]
async fn zero_floor_leaves_the_share_rule_alone() {
    // A floor of zero is a legitimate deployment choice: the
    // mechanism is present, the share alone binds.
    let (mut ctx, _authority) = start_with_floor(0).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 99)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 100)],
        &[&creator],
    )
    .await
    .expect("lock at exactly the share registers");
}

// ---------------------------------------------------------------------------
// Setup rules: fail closed before Initialize, authority-bound,
// exactly once, adoptable shell, and no substitute config account.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_before_initialize_fails_closed() {
    let (mut ctx, _authority) = start_uninitialized().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 500)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::ConfigNotInitialized as u32);
    assert!(read_record(&mut ctx, &mint).await.is_none());
}

#[tokio::test]
async fn initialize_by_non_authority_rejected() {
    let (mut ctx, authority) = start_uninitialized().await;
    let stranger = Keypair::new();
    let fund = system_instruction::transfer(
        &ctx.payer.pubkey(),
        &stranger.pubkey(),
        1_000_000_000,
    );
    send(&mut ctx, &[fund], &[]).await.expect("fund stranger");
    let err = send(
        &mut ctx,
        &[initialize_ix(&stranger.pubkey(), 500)],
        &[&stranger],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::NotUpgradeAuthority as u32);
    // The failed attempt sets nothing: the real authority can
    // still initialize afterwards.
    initialize(&mut ctx, &authority, 500)
        .await
        .expect("authority initializes after stranger's failure");
    assert_eq!(read_config(&mut ctx).await.min_lock_amount, 500);
}

#[tokio::test]
async fn initialize_runs_exactly_once() {
    let (mut ctx, authority) = start_with_floor(500).await;
    // A second Initialize — even from the upgrade authority, even
    // to "fix" the value — is refused, and the floor is unchanged.
    let err = initialize(&mut ctx, &authority, 10_000).await.unwrap_err();
    expect_custom(&err, LaunchError::ConfigAlreadyInitialized as u32);
    assert_eq!(read_config(&mut ctx).await.min_lock_amount, 500);
    // Proof the value did not move: a 500 lock (≥ old floor,
    // < attempted new floor) still registers.
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 500)],
        &[&creator],
    )
    .await
    .expect("lock at the original floor still registers");
}

#[tokio::test]
async fn initialize_adopts_a_prefunded_config_shell() {
    // Self-audit F-2 pattern: the config PDA is predictable, so
    // anyone can dust it before deployment. A bare transfer must
    // not brick setup forever — Initialize adopts the shell.
    let (mut ctx, authority) = start_uninitialized().await;
    let dust = system_instruction::transfer(
        &ctx.payer.pubkey(),
        &pda(&[SEED_CONFIG]),
        1_000_000,
    );
    send(&mut ctx, &[dust], &[]).await.expect("dust the config PDA");
    initialize(&mut ctx, &authority, 500)
        .await
        .expect("initialize adopts the shell");
    assert_eq!(read_config(&mut ctx).await.min_lock_amount, 500);
}

#[tokio::test]
async fn register_with_a_substitute_config_account_rejected() {
    let (mut ctx, _authority) = start_with_floor(500).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    // Any address other than the config PDA — a registrant cannot
    // point the check at an account of their own making.
    let err = send(
        &mut ctx,
        &[register_ix_with_config(
            &creator.pubkey(),
            &mint,
            &token,
            500,
            Pubkey::new_unique(),
        )],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::InvalidPda as u32);
    assert!(read_record(&mut ctx, &mint).await.is_none());
}

// ---------------------------------------------------------------------------
// War-room additions (2026-11-19): the both-fail ordering with a
// non-zero lock, the floor == share tie, floors at or above the
// whole supply, the legacy 9-account instruction form, and
// Initialize's own account checks. Each pins a behaviour the
// first fourteen tests left unobserved.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn both_fail_nonzero_lock_reports_the_share_error() {
    // Supply 1,000 (share 100), floor 500, lock 50: the lock
    // fails BOTH minimums. The share check runs first, so the
    // error must be LockTooSmall — a mutant that special-cases
    // zero or swaps the checks is caught here (the zero-lock
    // test alone cannot distinguish them).
    let (mut ctx, _authority) = start_with_floor(500).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 50)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);
    assert!(read_record(&mut ctx, &mint).await.is_none());
}

#[tokio::test]
async fn floor_equal_to_share_tie() {
    // Supply 1,000, floor 100 == the share: 99 fails the share,
    // 100 satisfies both at once.
    let (mut ctx, _authority) = start_with_floor(100).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 99)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 100)],
        &[&creator],
    )
    .await
    .expect("lock at the tied minimum registers");
}

#[tokio::test]
async fn floor_above_supply_blocks_registration() {
    // Floor 2,000 against a 1,000 supply: even locking the
    // ENTIRE supply passes the share and fails the floor. This
    // is the documented cost of an oversized floor value — it
    // makes the mint class unregistrable (see LAUNCH-GUARD.md).
    let (mut ctx, _authority) = start_with_floor(2_000).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 1_000)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockBelowDustFloor as u32);
    assert!(read_record(&mut ctx, &mint).await.is_none());
}

#[tokio::test]
async fn floor_u64_max_blocks_registration() {
    let (mut ctx, _authority) = start_with_floor(u64::MAX).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 1_000)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockBelowDustFloor as u32);
}

#[tokio::test]
async fn legacy_nine_account_register_rejected() {
    // The pre-floor instruction form (9 accounts, no config)
    // must fail — never silently register floor-less. The config
    // account was appended last precisely so old clients break
    // loudly instead of bypassing the check.
    let (mut ctx, _authority) = start_with_floor(500).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let mut ix = register_ix(&creator.pubkey(), &mint, &token, 500);
    ix.accounts.truncate(9);
    let err = send(&mut ctx, &[ix], &[&creator]).await.unwrap_err();
    let BanksClientError::TransactionError(TransactionError::InstructionError(
        _,
        solana_program::instruction::InstructionError::NotEnoughAccountKeys,
    )) = err
    else {
        panic!("expected NotEnoughAccountKeys, got {err:?}");
    };
    assert!(read_record(&mut ctx, &mint).await.is_none());
}

#[tokio::test]
async fn initialize_with_wrong_config_pda_rejected() {
    let (mut ctx, authority) = start_uninitialized().await;
    let fund = system_instruction::transfer(
        &ctx.payer.pubkey(),
        &authority.pubkey(),
        1_000_000_000,
    );
    send(&mut ctx, &[fund], &[]).await.expect("fund authority");
    let mut ix = initialize_ix(&authority.pubkey(), 500);
    ix.accounts[1] = AccountMeta::new(Pubkey::new_unique(), false);
    let err = send(&mut ctx, &[ix], &[&authority]).await.unwrap_err();
    expect_custom(&err, LaunchError::InvalidPda as u32);
}

#[tokio::test]
async fn initialize_by_non_signer_authority_rejected() {
    let (mut ctx, authority) = start_uninitialized().await;
    // The authority's account is present but not marked as a
    // signer, and the transaction is signed only by the payer:
    // the program's signer check must fire before anything else.
    let mut ix = initialize_ix(&authority.pubkey(), 500);
    ix.accounts[0] = AccountMeta::new_readonly(authority.pubkey(), false);
    let err = send(&mut ctx, &[ix], &[]).await.unwrap_err();
    let BanksClientError::TransactionError(TransactionError::InstructionError(
        _,
        solana_program::instruction::InstructionError::MissingRequiredSignature,
    )) = err
    else {
        panic!("expected MissingRequiredSignature, got {err:?}");
    };
}

#[tokio::test]
async fn second_registration_under_floor_counts_reputation_once_each() {
    // A floor rejection must not leak a reputation increment:
    // mint A registers (count 1), mint B at 499 is rejected by
    // the floor (count still 1), mint B at 500 registers (2).
    let (mut ctx, _authority) = start_with_floor(500).await;
    let creator = Keypair::new();
    let (mint_a, token_a) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint_a, &token_a, 500)],
        &[&creator],
    )
    .await
    .expect("mint A registers at the floor");
    let (mint_b, token_b) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint_b, &token_b, 499)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockBelowDustFloor as u32);
    assert_eq!(
        read_reputation(&mut ctx, &creator.pubkey())
            .await
            .launches_registered,
        1
    );
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint_b, &token_b, 500)],
        &[&creator],
    )
    .await
    .expect("mint B registers at the floor");
    assert_eq!(
        read_reputation(&mut ctx, &creator.pubkey())
            .await
            .launches_registered,
        2
    );
}

// ---------------------------------------------------------------------------
// The floor gates registration only: a lock created under it
// releases on the normal path, which never consults the config.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn release_after_floor_gated_registration() {
    let (mut ctx, _authority) = start_with_floor(500).await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 1_000).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 500)],
        &[&creator],
    )
    .await
    .expect("register at the floor");
    // Warp past the 30-day lock and release. The release
    // instruction's account list has no config account at all.
    let clock: solana_program::clock::Clock = ctx.banks_client.get_sysvar().await.unwrap();
    ctx.warp_to_slot(clock.slot + MIN_LOCK_SLOTS).unwrap();
    let release = Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(creator.pubkey(), true),
            AccountMeta::new(pda(&[SEED_LAUNCH, mint.as_ref()]), false),
            AccountMeta::new(pda(&[SEED_VAULT, mint.as_ref()]), false),
            AccountMeta::new(token, false),
            AccountMeta::new_readonly(pda(&[SEED_AUTHORITY, mint.as_ref()]), false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new(pda(&[SEED_REPUTATION, creator.pubkey().as_ref()]), false),
        ],
        data: borsh::to_vec(&LaunchInstruction::Release {}).unwrap(),
    };
    send(&mut ctx, &[release], &[&creator])
        .await
        .expect("release completes without the config");
    let rec = read_record(&mut ctx, &mint).await.unwrap();
    assert_eq!(rec.state, STATE_RELEASED);
    // The full locked amount is back with the creator, and the
    // release counted as a completed lock in the reputation.
    assert_eq!(read_token_amount(&mut ctx, &token).await, 1_000);
    let rep = read_reputation(&mut ctx, &creator.pubkey()).await;
    assert_eq!(rep.launches_registered, 1);
    assert_eq!(rep.locks_completed, 1);
}
