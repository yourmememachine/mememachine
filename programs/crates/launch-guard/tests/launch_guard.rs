//! Integration tests for LaunchGuard, run against a real program-test
//! validator with the real SPL token program.


use launch_guard::state::*;
use solana_program::{
    clock::Clock,
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    system_instruction,
};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};
use spl_token::{
    instruction::{
        initialize_account3, initialize_mint2, mint_to, set_authority, AuthorityType,
    },
    state::{Account as TokenAccount, Mint},
};

const SUPPLY: u64 = 1_000_000;
const LOCK: u64 = 100_000; // exactly 10%

fn program_id() -> Pubkey {
    Pubkey::new_from_array([77; 32])
}

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &program_id()).0
}

async fn start() -> ProgramTestContext {
    let test = ProgramTest::new(
        "launch_guard",
        program_id(),
        processor!(launch_guard::process_instruction),
    );
    test.start_with_context().await
}

async fn send(
    ctx: &mut ProgramTestContext,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> Result<(), BanksClientError> {
    let payer = ctx.payer.pubkey();
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&payer),
        &all,
        ctx.last_blockhash,
    );
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

/// Create a mint + creator token account holding SUPPLY. If `revoke`,
/// revoke both authorities (the only registrable state).
async fn make_mint(
    ctx: &mut ProgramTestContext,
    creator: &Keypair,
    revoke: bool,
) -> (Pubkey, Pubkey) {
    let mint = Keypair::new();
    let token_acct = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let mut ixs = vec![
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
            rent.minimum_balance(TokenAccount::LEN),
            TokenAccount::LEN as u64,
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
            SUPPLY,
        )
        .unwrap(),
    ];
    if revoke {
        ixs.push(
            set_authority(
                &spl_token::id(),
                &mint.pubkey(),
                None,
                AuthorityType::MintTokens,
                &creator.pubkey(),
                &[],
            )
            .unwrap(),
        );
        ixs.push(
            set_authority(
                &spl_token::id(),
                &mint.pubkey(),
                None,
                AuthorityType::FreezeAccount,
                &creator.pubkey(),
                &[],
            )
            .unwrap(),
        );
    }
    // creator needs lamports for the accounts it creates
    let fund = system_instruction::transfer(&ctx.payer.pubkey(), &creator.pubkey(), 5_000_000_000);
    let mut all = vec![fund];
    all.extend(ixs);
    send(ctx, &all, &[creator, &mint, &token_acct])
        .await
        .expect("mint setup");
    (mint.pubkey(), token_acct.pubkey())
}

fn register_ix(
    creator: &Pubkey,
    mint: &Pubkey,
    creator_token: &Pubkey,
    lock_amount: u64,
    lock_slots: u64,
) -> Instruction {
    let data = borsh::to_vec(&LaunchInstruction::RegisterAndLock {
        lock_amount,
        lock_slots,
    })
    .unwrap();
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
        ],
        data,
    }
}

fn release_ix(creator: &Pubkey, mint: &Pubkey, creator_token: &Pubkey) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*creator, true),
            AccountMeta::new(pda(&[SEED_LAUNCH, mint.as_ref()]), false),
            AccountMeta::new(pda(&[SEED_VAULT, mint.as_ref()]), false),
            AccountMeta::new(*creator_token, false),
            AccountMeta::new_readonly(pda(&[SEED_AUTHORITY, mint.as_ref()]), false),
            AccountMeta::new_readonly(spl_token::id(), false),
        ],
        data: borsh::to_vec(&LaunchInstruction::Release {}).unwrap(),
    }
}

fn extend_ix(creator: &Pubkey, mint: &Pubkey, additional_slots: u64) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*creator, true),
            AccountMeta::new(pda(&[SEED_LAUNCH, mint.as_ref()]), false),
        ],
        data: borsh::to_vec(&LaunchInstruction::ExtendLock { additional_slots }).unwrap(),
    }
}

async fn token_balance(ctx: &mut ProgramTestContext, acct: &Pubkey) -> u64 {
    let a = ctx.banks_client.get_account(*acct).await.unwrap().unwrap();
    TokenAccount::unpack(&a.data).unwrap().amount
}

async fn record(ctx: &mut ProgramTestContext, mint: &Pubkey) -> LaunchRecord {
    let a = ctx
        .banks_client
        .get_account(pda(&[SEED_LAUNCH, mint.as_ref()]))
        .await
        .unwrap()
        .unwrap();
    LaunchRecord::try_from_slice(&a.data).unwrap()
}

use borsh::BorshDeserialize;

#[tokio::test]
async fn register_locks_ten_percent_for_thirty_days() {
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .expect("register");
    let rec = record(&mut ctx, &mint).await;
    assert_eq!(rec.total_supply, SUPPLY);
    assert_eq!(rec.locked_amount, LOCK);
    assert_eq!(rec.state, STATE_LOCKED);
    assert_eq!(
        token_balance(&mut ctx, &pda(&[SEED_VAULT, mint.as_ref()])).await,
        LOCK
    );
    assert_eq!(token_balance(&mut ctx, &token).await, SUPPLY - LOCK);
}

#[tokio::test]
async fn register_rejected_if_mint_authority_kept() {
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, false).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::MintAuthorityNotRevoked as u32);
}

#[tokio::test]
async fn register_rejected_if_lock_too_small_or_too_short() {
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK / 2, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);

    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, 100_000)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockPeriodTooShort as u32);
}

#[tokio::test]
async fn release_before_expiry_fails_after_expiry_succeeds() {
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap();

    let err = send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint, &token)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::StillLocked as u32);

    // Move past the lock: locks are slot-based, and warp_to_slot
    // advances the slot reliably in program-test (timestamps do not
    // advance with warps there — measured during development).
    let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    ctx.warp_to_slot(clock.slot + MIN_LOCK_SLOTS + 100).unwrap();

    send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint, &token)],
        &[&creator],
    )
    .await
    .expect("release after expiry");
    assert_eq!(token_balance(&mut ctx, &token).await, SUPPLY);
    assert_eq!(record(&mut ctx, &mint).await.state, STATE_RELEASED);

    // Warp one slot so the repeat release is a distinct transaction
    // (same blockhash + same instruction would be a duplicate).
    let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    ctx.warp_to_slot(clock.slot + 1).unwrap();
    let err = send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint, &token)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::AlreadyReleased as u32);
}

#[tokio::test]
async fn extend_lock_pushes_release_later() {
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap();
    let before = record(&mut ctx, &mint).await.lock_until_slot;
    send(
        &mut ctx,
        &[extend_ix(&creator.pubkey(), &mint, 100_000)],
        &[&creator],
    )
    .await
    .expect("extend");
    let after = record(&mut ctx, &mint).await.lock_until_slot;
    assert_eq!(after, before + 100_000);
}

#[tokio::test]
async fn stranger_cannot_release() {
    let mut ctx = start().await;
    let creator = Keypair::new();
    let stranger = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap();
    // The creator check fires before the time check, so no time warp
    // is needed here: a stranger is rejected regardless of the clock.
    let err = send(
        &mut ctx,
        &[release_ix(&stranger.pubkey(), &mint, &token)],
        &[&stranger],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::NotCreator as u32);
}
