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
    account::Account,
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
/// Dust floor `start()` initializes the config with. Kept at 2 so
/// the F-4 ceiling-division test (supply 19, lock 2) still passes
/// on the 10% rule alone; the floor's own boundary behaviour is
/// covered in tests/dust_floor.rs with dedicated floors.
const TEST_FLOOR: u64 = 2;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([77; 32])
}

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &program_id()).0
}

async fn start() -> ProgramTestContext {
    let authority = Keypair::new();
    let mut test = ProgramTest::new(
        "launch_guard",
        program_id(),
        processor!(launch_guard::process_instruction),
    );
    seed_programdata(&mut test, &authority.pubkey());
    let mut ctx = test.start_with_context().await;
    initialize(&mut ctx, &authority, TEST_FLOOR).await;
    ctx
}

/// The program's ProgramData address under the BPF upgradeable
/// loader — where `Initialize` looks for the upgrade authority.
#[allow(deprecated)]
fn programdata_address() -> Pubkey {
    solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id())
}

/// Seed a ProgramData account naming `authority` as the program's
/// upgrade authority (loader bincode layout: variant tag 3, slot,
/// option tag, authority — the layout the program parses). Native
/// program-test has no real loader state for a `processor!`
/// program, so the account is constructed directly.
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

/// Fund the upgrade authority from the payer and run Initialize.
async fn initialize(ctx: &mut ProgramTestContext, authority: &Keypair, floor: u64) {
    let fund = system_instruction::transfer(
        &ctx.payer.pubkey(),
        &authority.pubkey(),
        1_000_000_000,
    );
    send(ctx, &[fund], &[]).await.expect("fund authority");
    send(ctx, &[initialize_ix(&authority.pubkey(), floor)], &[authority])
        .await
        .expect("initialize config");
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
            AccountMeta::new(pda(&[SEED_REPUTATION, creator.as_ref()]), false),
            AccountMeta::new_readonly(pda(&[SEED_CONFIG]), false),
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
            AccountMeta::new(pda(&[SEED_REPUTATION, creator.as_ref()]), false),
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

#[tokio::test]
async fn register_rejected_for_token2022_mint() {
    // A mint account owned by the Token-2022 program, with a byte
    // layout that would parse as a perfectly registrable classic mint
    // (authorities revoked, supply present). LaunchGuard must reject
    // it on ownership alone: Token-2022 extensions are out of scope
    // for v1 (LaunchError::UnsupportedTokenProgram).
    let token2022: Pubkey = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        .parse()
        .unwrap();
    let mint_state = Mint {
        mint_authority: solana_program::program_option::COption::None,
        supply: SUPPLY,
        decimals: 0,
        is_initialized: true,
        freeze_authority: solana_program::program_option::COption::None,
    };
    let mut mint_data = vec![0u8; Mint::LEN];
    mint_state.pack_into_slice(&mut mint_data);
    let fake_mint = Pubkey::new_unique();
    let mut test = ProgramTest::new(
        "launch_guard",
        program_id(),
        processor!(launch_guard::process_instruction),
    );
    test.add_account(
        fake_mint,
        solana_sdk::account::Account {
            lamports: 1_000_000_000,
            data: mint_data,
            owner: token2022,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = test.start_with_context().await;
    let creator = ctx.payer.pubkey();
    let creator_token = Pubkey::new_unique();
    let err = send(
        &mut ctx,
        &[register_ix(&creator, &fake_mint, &creator_token, LOCK, MIN_LOCK_SLOTS)],
        &[],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::UnsupportedTokenProgram as u32);
}

#[tokio::test]
async fn compute_units_register() {
    // Measurement test: prints compute units for RegisterAndLock,
    // for the record in programs/VERIFY.md.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    let ix = register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS);
    let payer = ctx.payer.pubkey();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer),
        &[&ctx.payer, &creator],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    result.result.unwrap();
    let cu = result.metadata.unwrap().compute_units_consumed;
    println!("CU RegisterAndLock: {cu}");
    assert!(cu < 200_000, "RegisterAndLock CU unexpectedly high: {cu}");
}

/// Like make_mint, but revokes ONLY the mint authority — the freeze
/// authority stays with the creator.
async fn make_mint_freeze_kept(
    ctx: &mut ProgramTestContext,
    creator: &Keypair,
) -> (Pubkey, Pubkey) {
    let mint = Keypair::new();
    let token_acct = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let ixs = vec![
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
        set_authority(
            &spl_token::id(),
            &mint.pubkey(),
            None,
            AuthorityType::MintTokens,
            &creator.pubkey(),
            &[],
        )
        .unwrap(),
    ];
    let fund = system_instruction::transfer(&ctx.payer.pubkey(), &creator.pubkey(), 5_000_000_000);
    let mut all = vec![fund];
    all.extend(ixs);
    send(ctx, &all, &[creator, &mint, &token_acct])
        .await
        .expect("mint setup");
    (mint.pubkey(), token_acct.pubkey())
}

#[tokio::test]
async fn register_rejected_if_freeze_authority_kept() {
    // Mint authority revoked, freeze authority still live: the
    // creator could freeze holders' accounts at will — not a
    // LaunchGuard launch.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_freeze_kept(&mut ctx, &creator).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::FreezeAuthorityNotRevoked as u32);
}

#[tokio::test]
async fn register_rejected_at_9_99_percent_lock() {
    // 99,900 of 1,000,000 is 9.99% — one tenth of a percent under
    // the 10% floor is still under it.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 99_900, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);
}

#[tokio::test]
async fn stranger_cannot_extend() {
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
    // Extending is creator-only, like releasing: a longer lock is
    // the creator's commitment to change, not anyone else's.
    let err = send(
        &mut ctx,
        &[extend_ix(&stranger.pubkey(), &mint, 100_000)],
        &[&stranger],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::NotCreator as u32);
}

#[tokio::test]
async fn registered_token_cannot_regain_authorities() {
    // The property LaunchGuard exists for: once a mint's mint and
    // freeze authorities are revoked (the only registrable state),
    // there is no path back — not for the creator, not for anyone.
    // SPL Token itself enforces this (a revoked authority is
    // COption::None and can never sign again); this test pins that
    // the registered state really is that irreversible state.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let attacker = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap();

    for authority_type in [AuthorityType::MintTokens, AuthorityType::FreezeAccount] {
        let err = send(
            &mut ctx,
            &[set_authority(
                &spl_token::id(),
                &mint,
                Some(&attacker.pubkey()),
                authority_type,
                &creator.pubkey(),
                &[],
            )
            .unwrap()],
            &[&creator],
        )
        .await
        .unwrap_err();
        // The exact token-program error is SPL's business; what
        // matters is that the attempt fails.
        let _ = err;
        // Warp a slot so the next attempt is a distinct transaction.
        let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
        ctx.warp_to_slot(clock.slot + 1).unwrap();
    }

    // And the mint is untouched: both authorities still revoked.
    let a = ctx.banks_client.get_account(mint).await.unwrap().unwrap();
    let mint_state = Mint::unpack(&a.data).unwrap();
    assert_eq!(
        mint_state.mint_authority,
        solana_program::program_option::COption::None
    );
    assert_eq!(
        mint_state.freeze_authority,
        solana_program::program_option::COption::None
    );
}

#[tokio::test]
async fn creator_reputation_counts_registrations_and_completions() {
    // The reputation PDA is the creator's public, on-chain track
    // record: registrations increment one counter, releases after
    // the full lock period increment the other. Nothing else moves
    // them.
    let mut ctx = start().await;
    let creator = Keypair::new();

    async fn read_reputation(
        ctx: &mut ProgramTestContext,
        creator: &Pubkey,
    ) -> Option<CreatorReputation> {
        let addr = pda(&[SEED_REPUTATION, creator.as_ref()]);
        let acc = ctx.banks_client.get_account(addr).await.unwrap()?;
        assert_eq!(acc.data.len(), CreatorReputation::LEN);
        Some(CreatorReputation::try_from_slice(&acc.data).unwrap())
    }

    // No launches yet: no reputation account exists for anyone.
    assert!(read_reputation(&mut ctx, &creator.pubkey()).await.is_none());
    assert!(read_reputation(&mut ctx, &Pubkey::new_unique()).await.is_none());

    // First launch creates the PDA with 1 registered, 0 completed.
    let (mint_a, token_a) = make_mint(&mut ctx, &creator, true).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint_a, &token_a, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap();
    let rep = read_reputation(&mut ctx, &creator.pubkey()).await.unwrap();
    assert_eq!(rep.discriminator, DISCRIMINATOR_REPUTATION);
    assert_eq!(rep.creator, creator.pubkey());
    assert_eq!(rep.launches_registered, 1);
    assert_eq!(rep.locks_completed, 0);
    assert!(rep.first_launch_slot > 0);
    assert_eq!(rep.last_completed_slot, 0);

    // A second launch by the same creator loads the same PDA and
    // increments — it does not reset or duplicate the record.
    let (mint_b, token_b) = make_mint(&mut ctx, &creator, true).await;
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint_b, &token_b, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap();
    let rep = read_reputation(&mut ctx, &creator.pubkey()).await.unwrap();
    assert_eq!(rep.launches_registered, 2);
    assert_eq!(rep.locks_completed, 0);

    // Extending a lock changes neither counter.
    send(
        &mut ctx,
        &[extend_ix(&creator.pubkey(), &mint_a, 100_000)],
        &[&creator],
    )
    .await
    .unwrap();
    let rep = read_reputation(&mut ctx, &creator.pubkey()).await.unwrap();
    assert_eq!(rep.launches_registered, 2);
    assert_eq!(rep.locks_completed, 0);

    // Releasing launch A after its full period (including the
    // extension) records exactly one completion.
    let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    ctx.warp_to_slot(clock.slot + MIN_LOCK_SLOTS + 100_000 + 100).unwrap();
    send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint_a, &token_a)],
        &[&creator],
    )
    .await
    .unwrap();
    let rep = read_reputation(&mut ctx, &creator.pubkey()).await.unwrap();
    assert_eq!(rep.launches_registered, 2);
    assert_eq!(rep.locks_completed, 1);
    assert!(rep.last_completed_slot > 0);

    // Launch B is still locked and uncounted as completed; a
    // stranger still has no reputation account at all.
    assert!(read_reputation(&mut ctx, &Pubkey::new_unique()).await.is_none());
}

// ---------------------------------------------------------------------------
// Self-audit regression tests (2026-11-16, REVIEW-1)
//
// F-4: the minimum lock uses ceiling division — a 19-token supply
// may not pass the 10% gate with a 1-token lock.
// F-2: pre-funded PDAs are adopted, not permanent blocks.
// ---------------------------------------------------------------------------

/// Like `make_mint` with `revoke = true`, but with a caller-chosen
/// total supply (the F-4 rounding bug only shows at tiny supplies).
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

#[tokio::test]
async fn register_supply19_lock1_fails_lock2_succeeds() {
    // Self-audit F-4: floor division computed ceil's job wrong —
    // 19 * 1000 / 10_000 = 1 by floor, so a 1-token lock (5.26%)
    // passed the >=10% gate. The ceiling is 2.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint_with_supply(&mut ctx, &creator, 19).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 1, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);

    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, 2, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap();
    let rec = record(&mut ctx, &mint).await;
    assert_eq!(rec.total_supply, 19);
    assert_eq!(rec.locked_amount, 2);
}

#[tokio::test]
async fn register_succeeds_with_prefunded_record_pda() {
    // Self-audit F-2: a dust transfer to the record PDA used to
    // brick this mint's registration forever.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    let record_pda = pda(&[SEED_LAUNCH, mint.as_ref()]);
    let payer = ctx.payer.pubkey();
    send(
        &mut ctx,
        &[system_instruction::transfer(&payer, &record_pda, 1_000_000)],
        &[],
    )
    .await
    .unwrap();
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap();
    let rec = record(&mut ctx, &mint).await;
    assert_eq!(rec.locked_amount, LOCK);
}

#[tokio::test]
async fn register_succeeds_with_prefunded_vault_pda() {
    // Self-audit F-2, vault variant: the vault is a token account
    // created for the authority PDA; pre-funding it used to block
    // registration the same way.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator, true).await;
    let vault_pda = pda(&[SEED_VAULT, mint.as_ref()]);
    let payer = ctx.payer.pubkey();
    send(
        &mut ctx,
        &[system_instruction::transfer(&payer, &vault_pda, 1_000_000)],
        &[],
    )
    .await
    .unwrap();
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap();
    assert_eq!(token_balance(&mut ctx, &vault_pda).await, LOCK);
    assert_eq!(token_balance(&mut ctx, &token).await, SUPPLY - LOCK);
}
