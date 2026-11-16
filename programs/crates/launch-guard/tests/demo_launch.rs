//! LaunchGuard DEMO LAUNCH — the full story, end to end, on localnet.
//!
//! This is the demonstration recorded in programs/DEMO-LAUNCH.md:
//! a fresh demo token is created, its supply minted, BOTH authorities
//! revoked, the launch registered in LaunchGuard with 10% of supply
//! locked for 30 days (6,480,000 slots), and then the rug paths are
//! attempted live — mint more, release early, shorten the lock,
//! release as a stranger — and every one of them must FAIL.
//!
//! The demo token is worthless by construction: fixed demo-only
//! keypairs (below), a local program-test ledger, no real assets.
//! The fixed keypairs make the demo reproducible — every run
//! produces the exact account addresses printed in DEMO-LAUNCH.md.
//! NEVER use these keypairs anywhere else; they guard nothing and
//! must never hold anything.
//!
//! Run with output (the demo report is printed):
//!     cd programs && cargo test --release --test demo_launch -- --nocapture

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
    instruction::{initialize_account3, initialize_mint2, mint_to, set_authority, AuthorityType},
    state::{Account as TokenAccount, Mint},
};

/// Demo supply: 1,000,000,000 demo tokens (0 decimals).
const SUPPLY: u64 = 1_000_000_000;
/// Locked at registration: exactly the 10% LaunchGuard floor.
const LOCK: u64 = 100_000_000;

// Demo-only fixed keypairs (localnet program-test; guard nothing).
const CREATOR_BYTES: [u8; 64] = [
    26, 183, 55, 100, 110, 143, 158, 71, 142, 82, 170, 76, 27, 168, 212, 39, 30, 188, 102, 103,
    154, 23, 237, 149, 68, 16, 63, 164, 21, 98, 145, 87, 111, 50, 133, 193, 29, 31, 142, 144, 17,
    67, 210, 18, 78, 184, 140, 226, 242, 81, 101, 118, 91, 132, 146, 246, 128, 229, 229, 33, 235,
    205, 102, 38,
];
const MINT_BYTES: [u8; 64] = [
    104, 60, 240, 122, 210, 168, 66, 67, 242, 80, 32, 180, 186, 124, 21, 221, 174, 245, 239, 147,
    192, 129, 158, 68, 179, 169, 213, 181, 8, 231, 130, 69, 144, 153, 103, 98, 181, 128, 193, 210,
    192, 159, 202, 53, 190, 28, 58, 18, 73, 191, 9, 242, 190, 125, 114, 186, 82, 39, 157, 51, 230,
    47, 116, 115,
];
const TOKEN_ACCT_BYTES: [u8; 64] = [
    97, 35, 198, 23, 141, 229, 53, 118, 225, 46, 126, 242, 135, 201, 121, 81, 165, 177, 130, 12,
    46, 45, 94, 158, 32, 77, 130, 95, 103, 93, 88, 130, 159, 193, 151, 142, 145, 13, 198, 168, 73,
    83, 12, 41, 145, 79, 129, 159, 199, 39, 65, 144, 26, 142, 139, 24, 160, 234, 159, 57, 152,
    192, 36, 80,
];

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

/// Warp one slot so the next transaction is distinct (program-test
/// caches the blockhash within a slot).
async fn next_slot(ctx: &mut ProgramTestContext) {
    let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    ctx.warp_to_slot(clock.slot + 1).unwrap();
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

async fn mint_state(ctx: &mut ProgramTestContext, mint: &Pubkey) -> Mint {
    let a = ctx.banks_client.get_account(*mint).await.unwrap().unwrap();
    Mint::unpack(&a.data).unwrap()
}

#[tokio::test]
async fn demo_launch_full_story() {
    let mut ctx = start().await;
    let creator = Keypair::from_bytes(&CREATOR_BYTES).unwrap();
    let mint = Keypair::from_bytes(&MINT_BYTES).unwrap();
    let token_acct = Keypair::from_bytes(&TOKEN_ACCT_BYTES).unwrap();
    let mint_pk = mint.pubkey();
    let token_pk = token_acct.pubkey();

    println!("\n================ LAUNCHGUARD DEMO LAUNCH ================");
    println!("Creator:               {}", creator.pubkey());
    println!("Demo token mint:       {mint_pk}");

    // -- Step 1: create the demo token and mint the full supply. --
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let setup = vec![
        system_instruction::transfer(&ctx.payer.pubkey(), &creator.pubkey(), 5_000_000_000),
        system_instruction::create_account(
            &creator.pubkey(),
            &mint_pk,
            rent.minimum_balance(Mint::LEN),
            Mint::LEN as u64,
            &spl_token::id(),
        ),
        initialize_mint2(
            &spl_token::id(),
            &mint_pk,
            &creator.pubkey(),
            Some(&creator.pubkey()),
            0,
        )
        .unwrap(),
        system_instruction::create_account(
            &creator.pubkey(),
            &token_pk,
            rent.minimum_balance(TokenAccount::LEN),
            TokenAccount::LEN as u64,
            &spl_token::id(),
        ),
        initialize_account3(&spl_token::id(), &token_pk, &mint_pk, &creator.pubkey()).unwrap(),
        mint_to(
            &spl_token::id(),
            &mint_pk,
            &token_pk,
            &creator.pubkey(),
            &[],
            SUPPLY,
        )
        .unwrap(),
    ];
    send(&mut ctx, &setup, &[&creator, &mint, &token_acct])
        .await
        .expect("demo token setup");
    println!("Supply minted:         {SUPPLY} (to creator token account {token_pk})");
    assert_eq!(token_balance(&mut ctx, &token_pk).await, SUPPLY);

    // -- Step 2: revoke BOTH authorities. Supply is now fixed forever.
    next_slot(&mut ctx).await;
    let revoke = vec![
        set_authority(
            &spl_token::id(),
            &mint_pk,
            None,
            AuthorityType::MintTokens,
            &creator.pubkey(),
            &[],
        )
        .unwrap(),
        set_authority(
            &spl_token::id(),
            &mint_pk,
            None,
            AuthorityType::FreezeAccount,
            &creator.pubkey(),
            &[],
        )
        .unwrap(),
    ];
    send(&mut ctx, &revoke, &[&creator])
        .await
        .expect("revoke authorities");
    let ms = mint_state(&mut ctx, &mint_pk).await;
    assert_eq!(
        ms.mint_authority,
        solana_program::program_option::COption::None
    );
    assert_eq!(
        ms.freeze_authority,
        solana_program::program_option::COption::None
    );
    println!("Mint authority:        REVOKED");
    println!("Freeze authority:      REVOKED");

    // -- Step 3: register the launch — 10% locked for 30 days. -----
    next_slot(&mut ctx).await;
    send(
        &mut ctx,
        &[register_ix(
            &creator.pubkey(),
            &mint_pk,
            &token_pk,
            LOCK,
            MIN_LOCK_SLOTS,
        )],
        &[&creator],
    )
    .await
    .expect("register demo launch");
    let rec = record(&mut ctx, &mint_pk).await;
    let record_pda = pda(&[SEED_LAUNCH, mint_pk.as_ref()]);
    let vault_pda = pda(&[SEED_VAULT, mint_pk.as_ref()]);
    let reputation_pda = pda(&[SEED_REPUTATION, creator.pubkey().as_ref()]);
    assert_eq!(rec.total_supply, SUPPLY);
    assert_eq!(rec.locked_amount, LOCK);
    assert_eq!(rec.state, STATE_LOCKED);
    assert_eq!(rec.lock_until_slot, rec.registered_slot + MIN_LOCK_SLOTS);
    assert_eq!(token_balance(&mut ctx, &vault_pda).await, LOCK);
    assert_eq!(token_balance(&mut ctx, &token_pk).await, SUPPLY - LOCK);
    let rep_acc = ctx
        .banks_client
        .get_account(reputation_pda)
        .await
        .unwrap()
        .unwrap();
    let rep = CreatorReputation::try_from_slice(&rep_acc.data).unwrap();
    assert_eq!(rep.launches_registered, 1);
    assert_eq!(rep.locks_completed, 0);
    println!("LaunchRecord PDA:      {record_pda}");
    println!("Vault PDA:             {vault_pda}");
    println!("Creator reputation PDA: {reputation_pda}");
    println!("Locked amount:         {} (10% of supply)", rec.locked_amount);
    println!("Registered at slot:    {}", rec.registered_slot);
    println!("Unlock slot:           {} (+{} slots = 30 days)", rec.lock_until_slot, MIN_LOCK_SLOTS);
    println!("Vault balance:         {LOCK}");
    println!("Reputation:            {} launch registered, {} completed", rep.launches_registered, rep.locks_completed);

    // -- Rug attempt (a): mint more tokens after revocation. -------
    next_slot(&mut ctx).await;
    let err = send(
        &mut ctx,
        &[mint_to(
            &spl_token::id(),
            &mint_pk,
            &token_pk,
            &creator.pubkey(),
            &[],
            1_000_000,
        )
        .unwrap()],
        &[&creator],
    )
    .await
    .unwrap_err();
    println!("RUG ATTEMPT (a) mint 1,000,000 more after revocation: FAILED ({err:?})");
    assert_eq!(mint_state(&mut ctx, &mint_pk).await.supply, SUPPLY);

    // -- Rug attempt (b): release the lock early. ------------------
    next_slot(&mut ctx).await;
    let err = send(
        &mut ctx,
        &[release_ix(&creator.pubkey(), &mint_pk, &token_pk)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::StillLocked as u32);
    println!(
        "RUG ATTEMPT (b) release the lock early: FAILED (LaunchGuard StillLocked, error {})",
        LaunchError::StillLocked as u32
    );

    // -- Rug attempt (c): shorten the lock. -------------------------
    // There is no shorten instruction; the only lock-moving
    // instruction is ExtendLock, and a zero-slot "extension" — the
    // closest thing to asking for an earlier end — is rejected.
    // The recorded unlock slot must be byte-for-byte unchanged.
    next_slot(&mut ctx).await;
    let err = send(
        &mut ctx,
        &[extend_ix(&creator.pubkey(), &mint_pk, 0)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::LockPeriodTooShort as u32);
    println!(
        "RUG ATTEMPT (c) shorten the lock (zero-slot extend): FAILED (LaunchGuard LockPeriodTooShort, error {})",
        LaunchError::LockPeriodTooShort as u32
    );

    // -- Rug attempt (d): release by a stranger. --------------------
    next_slot(&mut ctx).await;
    let stranger = Keypair::new();
    let err = send(
        &mut ctx,
        &[release_ix(&stranger.pubkey(), &mint_pk, &token_pk)],
        &[&stranger],
    )
    .await
    .unwrap_err();
    expect_custom(&err, LaunchError::NotCreator as u32);
    println!(
        "RUG ATTEMPT (d) release by a stranger: FAILED (LaunchGuard NotCreator, error {})",
        LaunchError::NotCreator as u32
    );

    // -- Final state: nothing moved. --------------------------------
    let rec = record(&mut ctx, &mint_pk).await;
    assert_eq!(rec.state, STATE_LOCKED);
    assert_eq!(rec.lock_until_slot, rec.registered_slot + MIN_LOCK_SLOTS);
    assert_eq!(token_balance(&mut ctx, &vault_pda).await, LOCK);
    assert_eq!(token_balance(&mut ctx, &token_pk).await, SUPPLY - LOCK);
    assert_eq!(mint_state(&mut ctx, &mint_pk).await.supply, SUPPLY);
    println!("FINAL: vault still holds {LOCK}, lock still ends at slot {}, supply still {SUPPLY}.", rec.lock_until_slot);
    println!("================ DEMO COMPLETE — EVERY RUG ATTEMPT FAILED ================\n");
}
