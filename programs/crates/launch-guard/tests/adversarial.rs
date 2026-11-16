//! LaunchGuard "rug attempt" harness — adversarial tests written from
//! an attacker's checklist. Each test is one way a creator, a stranger,
//! or a compromised account tries to get locked tokens out early, to
//! shorten a lock, to forge a reputation, or to bypass the program
//! entirely. Every attempt must fail (or, where the program permits
//! the action, must only move the lock later — never earlier).
//!
//! Companion to programs/LAUNCH-GUARD.md. Run with:
//!     cd programs && cargo test --release --test adversarial

use borsh::BorshDeserialize;
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
        initialize_account3, initialize_mint2, mint_to, set_authority, transfer as spl_transfer,
        AuthorityType,
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

/// Create a mint + creator token account holding SUPPLY, with both
/// authorities revoked (the only registrable state).
async fn make_mint(
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
    // creator needs lamports for the accounts it creates
    let fund = system_instruction::transfer(&ctx.payer.pubkey(), &creator.pubkey(), 5_000_000_000);
    let mut all = vec![fund];
    all.extend(ixs);
    send(ctx, &all, &[creator, &mint, &token_acct])
        .await
        .expect("mint setup");
    (mint.pubkey(), token_acct.pubkey())
}

/// An empty token account for `owner` on `mint`, funded by the payer.
async fn make_token_account(
    ctx: &mut ProgramTestContext,
    owner: &Pubkey,
    mint: &Pubkey,
) -> Pubkey {
    let acct = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let ixs = vec![
        system_instruction::create_account(
            &ctx.payer.pubkey(),
            &acct.pubkey(),
            rent.minimum_balance(TokenAccount::LEN),
            TokenAccount::LEN as u64,
            &spl_token::id(),
        ),
        initialize_account3(&spl_token::id(), &acct.pubkey(), mint, owner).unwrap(),
    ];
    send(ctx, &ixs, &[&acct]).await.expect("token account");
    acct.pubkey()
}

fn register_ix_rep(
    creator: &Pubkey,
    mint: &Pubkey,
    creator_token: &Pubkey,
    lock_amount: u64,
    lock_slots: u64,
    reputation: &Pubkey,
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
            AccountMeta::new(*reputation, false),
        ],
        data,
    }
}

fn register_ix(
    creator: &Pubkey,
    mint: &Pubkey,
    creator_token: &Pubkey,
    lock_amount: u64,
    lock_slots: u64,
) -> Instruction {
    register_ix_rep(
        creator,
        mint,
        creator_token,
        lock_amount,
        lock_slots,
        &pda(&[SEED_REPUTATION, creator.as_ref()]),
    )
}

fn release_ix_rep(
    creator: &Pubkey,
    mint: &Pubkey,
    creator_token: &Pubkey,
    reputation: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*creator, true),
            AccountMeta::new(pda(&[SEED_LAUNCH, mint.as_ref()]), false),
            AccountMeta::new(pda(&[SEED_VAULT, mint.as_ref()]), false),
            AccountMeta::new(*creator_token, false),
            AccountMeta::new_readonly(pda(&[SEED_AUTHORITY, mint.as_ref()]), false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new(*reputation, false),
        ],
        data: borsh::to_vec(&LaunchInstruction::Release {}).unwrap(),
    }
}

fn release_ix(creator: &Pubkey, mint: &Pubkey, creator_token: &Pubkey) -> Instruction {
    release_ix_rep(
        creator,
        mint,
        creator_token,
        &pda(&[SEED_REPUTATION, creator.as_ref()]),
    )
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

async fn read_reputation(
    ctx: &mut ProgramTestContext,
    creator: &Pubkey,
) -> Option<CreatorReputation> {
    let addr = pda(&[SEED_REPUTATION, creator.as_ref()]);
    let acc = ctx.banks_client.get_account(addr).await.unwrap()?;
    Some(CreatorReputation::try_from_slice(&acc.data).unwrap())
}

async fn clock_slot(ctx: &mut ProgramTestContext) -> u64 {
    let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    clock.slot
}

async fn register(
    ctx: &mut ProgramTestContext,
    creator: &Keypair,
) -> (Pubkey, Pubkey) {
    let (mint, token) = make_mint(ctx, creator).await;
    send(
        ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[creator],
    )
    .await
    .expect("register");
    (mint, token)
}

// ---------------------------------------------------------------------------
// The attacks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attack_release_one_slot_early_fails_at_exact_expiry_succeeds() {
    // The expiry boundary is exact: one slot before lock_until_slot
    // the vault is still sealed; at lock_until_slot itself it opens.
    // A creator timing a release to the last locked slot gets nothing.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = register(&mut ctx, &creator).await;
    let until = record(&mut ctx, &mint).await.lock_until_slot;

    ctx.warp_to_slot(until - 1).unwrap();
    let err = send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint, &token)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::StillLocked as u32);
    assert_eq!(record(&mut ctx, &mint).await.state, STATE_LOCKED);
    assert_eq!(
        token_balance(&mut ctx, &pda(&[SEED_VAULT, mint.as_ref()])).await,
        LOCK
    );

    ctx.warp_to_slot(until).unwrap();
    send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint, &token)],
        &[&creator],
    )
    .await
    .expect("release at exact expiry");
    assert_eq!(token_balance(&mut ctx, &token).await, SUPPLY);
}

#[tokio::test]
async fn attack_double_register_same_mint_fails() {
    // Registering the same mint twice would mean a second record and
    // a second vault over the same locked supply. The launch PDA
    // already exists, so the second registration cannot create it.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = register(&mut ctx, &creator).await;
    // Warp a slot so the second registration is a distinct
    // transaction: an identical retry in the same slot would return
    // the first transaction's cached result instead of executing.
    let clock = clock_slot(&mut ctx).await;
    ctx.warp_to_slot(clock + 1).unwrap();
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap_err();
    let _ = err;
    // Still exactly one launch: the record and vault are untouched.
    let rec = record(&mut ctx, &mint).await;
    assert_eq!(rec.locked_amount, LOCK);
    assert_eq!(rec.state, STATE_LOCKED);
    assert_eq!(
        token_balance(&mut ctx, &pda(&[SEED_VAULT, mint.as_ref()])).await,
        LOCK
    );
    assert_eq!(
        read_reputation(&mut ctx, &creator.pubkey())
            .await
            .unwrap()
            .launches_registered,
        1
    );
}

#[tokio::test]
async fn attack_stranger_drains_vault_directly_fails() {
    // Bypass attempt: skip LaunchGuard entirely and call the SPL
    // token program to move locked tokens out of the vault. The
    // vault is owned by the authority PDA, which only the program
    // can sign for — a stranger's signature is worthless here.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let stranger = Keypair::new();
    let (mint, _token) = register(&mut ctx, &creator).await;
    let stranger_token = make_token_account(&mut ctx, &stranger.pubkey(), &mint).await;
    let vault = pda(&[SEED_VAULT, mint.as_ref()]);
    let err = send(
        &mut ctx,
        &[spl_transfer(
            &spl_token::id(),
            &vault,
            &stranger_token,
            &stranger.pubkey(),
            &[],
            LOCK,
        )
        .unwrap()],
        &[&stranger],
    )
    .await
    .unwrap_err();
    let _ = err;
    assert_eq!(token_balance(&mut ctx, &vault).await, LOCK);
    assert_eq!(token_balance(&mut ctx, &stranger_token).await, 0);
}

#[tokio::test]
async fn attack_release_to_attacker_token_account_fails() {
    // The creator signs the release but names an attacker's token
    // account as the destination. The program returns the funds to
    // the creator's own account only — a foreign destination is a
    // MintMismatch.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let attacker = Keypair::new();
    let (mint, token) = register(&mut ctx, &creator).await;
    let attacker_token = make_token_account(&mut ctx, &attacker.pubkey(), &mint).await;
    let until = record(&mut ctx, &mint).await.lock_until_slot;
    ctx.warp_to_slot(until).unwrap();
    let err = send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint, &attacker_token)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::MintMismatch as u32);
    assert_eq!(
        token_balance(&mut ctx, &pda(&[SEED_VAULT, mint.as_ref()])).await,
        LOCK
    );
    // The honest release still works afterwards.
    send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint, &token)],
        &[&creator],
    )
    .await
    .expect("honest release after failed attempt");
    assert_eq!(token_balance(&mut ctx, &token).await, SUPPLY);
}

#[tokio::test]
async fn attack_release_without_reputation_account_fails() {
    // Release requires the creator's reputation PDA to exist and to
    // be the derived address. A nonexistent (or foreign) account
    // fails the release instead of silently skipping the completion
    // count — and because the transaction rolls back, no funds move.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = register(&mut ctx, &creator).await;
    let until = record(&mut ctx, &mint).await.lock_until_slot;
    ctx.warp_to_slot(until).unwrap();
    let missing = Pubkey::new_unique();
    let err = send(
        &mut ctx,
        &[release_ix_rep(&creator.pubkey(), &mint, &token, &missing)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::InvalidPda as u32);
    assert_eq!(record(&mut ctx, &mint).await.state, STATE_LOCKED);
    assert_eq!(
        token_balance(&mut ctx, &pda(&[SEED_VAULT, mint.as_ref()])).await,
        LOCK
    );
    assert_eq!(token_balance(&mut ctx, &token).await, SUPPLY - LOCK);
}

#[tokio::test]
async fn attack_register_with_forged_reputation_fails() {
    // An attacker registers a launch but passes a *victim's*
    // reputation PDA, hoping the registration lands on someone
    // else's track record (or inflates a reputation they control
    // elsewhere). The program derives the PDA from the signer, so a
    // foreign reputation account is rejected and nothing is created.
    let mut ctx = start().await;
    let victim = Keypair::new();
    let attacker = Keypair::new();
    let (_victim_mint, _victim_token) = register(&mut ctx, &victim).await;
    let victim_rep = pda(&[SEED_REPUTATION, victim.pubkey().as_ref()]);

    let (mint, token) = make_mint(&mut ctx, &attacker).await;
    let err = send(
        &mut ctx,
        &[register_ix_rep(
            &attacker.pubkey(),
            &mint,
            &token,
            LOCK,
            MIN_LOCK_SLOTS,
            &victim_rep,
        )],
        &[&attacker],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::InvalidPda as u32);
    // The failed registration created nothing for the attacker, and
    // the victim's record is untouched.
    assert!(ctx
        .banks_client
        .get_account(pda(&[SEED_LAUNCH, mint.as_ref()]))
        .await
        .unwrap()
        .is_none());
    assert!(read_reputation(&mut ctx, &attacker.pubkey()).await.is_none());
    let victim_rep_state = read_reputation(&mut ctx, &victim.pubkey()).await.unwrap();
    assert_eq!(victim_rep_state.launches_registered, 1);
    assert_eq!(victim_rep_state.locks_completed, 0);
}

#[tokio::test]
async fn attack_lock_can_only_move_later() {
    // There is no instruction that shortens a lock. The three ways
    // to try: extend by zero, extend after release, and extend after
    // expiry — the first two fail outright; the third is permitted
    // because it only re-locks the creator's own funds for longer,
    // and the old expiry stays sealed until the new date.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = register(&mut ctx, &creator).await;
    let until = record(&mut ctx, &mint).await.lock_until_slot;

    let err = send(
        &mut ctx,
        &[extend_ix(&creator.pubkey(), &mint, 0)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockPeriodTooShort as u32);

    // Past expiry but not yet released: extending still only pushes
    // the date later.
    ctx.warp_to_slot(until + 100).unwrap();
    send(
        &mut ctx,
        &[extend_ix(&creator.pubkey(), &mint, 100_000)],
        &[&creator],
    )
    .await
    .expect("extend after expiry");
    let new_until = record(&mut ctx, &mint).await.lock_until_slot;
    assert_eq!(new_until, until + 100_000);
    let err = send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint, &token)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::StillLocked as u32);

    // Release at the new date, then extending is impossible: the
    // launch is over.
    ctx.warp_to_slot(new_until).unwrap();
    send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint, &token)],
        &[&creator],
    )
    .await
    .expect("release at extended expiry");
    let clock = clock_slot(&mut ctx).await;
    ctx.warp_to_slot(clock + 1).unwrap();
    let err = send(
        &mut ctx,
        &[extend_ix(&creator.pubkey(), &mint, 100_000)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::AlreadyReleased as u32);
}

#[tokio::test]
async fn attack_two_creators_interleaved_reputations_stay_separate() {
    // Two creators launch interleaved mints. Each creator's
    // reputation counts only their own launches and completions,
    // and neither can release the other's launch or borrow the
    // other's reputation account to do it.
    let mut ctx = start().await;
    let alice = Keypair::new();
    let bob = Keypair::new();
    let (mint_a1, token_a1) = register(&mut ctx, &alice).await;
    let (mint_b1, _token_b1) = register(&mut ctx, &bob).await;
    let (mint_a2, token_a2) = register(&mut ctx, &alice).await;

    let rep_a = read_reputation(&mut ctx, &alice.pubkey()).await.unwrap();
    let rep_b = read_reputation(&mut ctx, &bob.pubkey()).await.unwrap();
    assert_eq!((rep_a.launches_registered, rep_a.locks_completed), (2, 0));
    assert_eq!((rep_b.launches_registered, rep_b.locks_completed), (1, 0));

    // Bob cannot release Alice's launch, even after its expiry.
    let until_a1 = record(&mut ctx, &mint_a1).await.lock_until_slot;
    ctx.warp_to_slot(until_a1).unwrap();
    let bob_token_a1 = make_token_account(&mut ctx, &bob.pubkey(), &mint_a1).await;
    let err = send(
        &mut ctx,
        &[release_ix(&bob.pubkey(), &mint_a1, &bob_token_a1)],
        &[&bob],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::NotCreator as u32);

    // Alice releases her first launch; Bob's counters do not move.
    send(
        &mut ctx,
        &[release_ix(&alice.pubkey(), &mint_a1, &token_a1)],
        &[&alice],
    )
    .await
    .expect("alice releases first launch");
    let rep_a = read_reputation(&mut ctx, &alice.pubkey()).await.unwrap();
    let rep_b = read_reputation(&mut ctx, &bob.pubkey()).await.unwrap();
    assert_eq!((rep_a.launches_registered, rep_a.locks_completed), (2, 1));
    assert_eq!((rep_b.launches_registered, rep_b.locks_completed), (1, 0));

    // Alice cannot release her second launch with Bob's reputation
    // account: the program derives the PDA from the signer.
    let until_a2 = record(&mut ctx, &mint_a2).await.lock_until_slot;
    // Both launches were registered in the same opening slot, so
    // their expiries coincide — warp forward, never back.
    let target = until_a2.max(clock_slot(&mut ctx).await + 1);
    ctx.warp_to_slot(target).unwrap();
    let bob_rep = pda(&[SEED_REPUTATION, bob.pubkey().as_ref()]);
    let err = send(
        &mut ctx,
        &[release_ix_rep(&alice.pubkey(), &mint_a2, &token_a2, &bob_rep)],
        &[&alice],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::InvalidPda as u32);
    assert_eq!(record(&mut ctx, &mint_a2).await.state, STATE_LOCKED);

    // The honest release still completes her record.
    send(
        &mut ctx,
        &[release_ix(&alice.pubkey(), &mint_a2, &token_a2)],
        &[&alice],
    )
    .await
    .expect("alice releases second launch");
    let rep_a = read_reputation(&mut ctx, &alice.pubkey()).await.unwrap();
    assert_eq!((rep_a.launches_registered, rep_a.locks_completed), (2, 2));
}

#[tokio::test]
async fn attack_failed_registration_does_not_create_reputation() {
    // A registration that fails validation (here: a lock under the
    // 10% floor) must not leave a reputation account behind or
    // inflate any counter — a creator cannot farm registrations
    // from rejected launches.
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token) = make_mint(&mut ctx, &creator).await;
    let err = send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK / 2, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockTooSmall as u32);
    assert!(read_reputation(&mut ctx, &creator.pubkey()).await.is_none());
    assert!(ctx
        .banks_client
        .get_account(pda(&[SEED_LAUNCH, mint.as_ref()]))
        .await
        .unwrap()
        .is_none());

    // The later successful registration counts exactly once.
    send(
        &mut ctx,
        &[register_ix(&creator.pubkey(), &mint, &token, LOCK, MIN_LOCK_SLOTS)],
        &[&creator],
    )
    .await
    .expect("register after failed attempt");
    assert_eq!(
        read_reputation(&mut ctx, &creator.pubkey())
            .await
            .unwrap()
            .launches_registered,
        1
    );
}
