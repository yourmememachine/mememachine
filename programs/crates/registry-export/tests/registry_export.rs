//! Registry export against the REAL program: register launches in
//! program-test, read the actual `LaunchRecord` / `CreatorReputation`
//! account bytes the program wrote, and assert the registry built
//! from them is correct. This is the same account data the binary's
//! RPC path would receive — only the transport differs
//! (`BanksClient::get_account` here, `getProgramAccounts` live).
//!
//! Run: `cd programs && cargo test --release -p registry-export --test registry_export`

use borsh::BorshDeserialize;
use launch_guard::state::*;
use registry_export::{build_registry, verify_registry};
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
    transaction::Transaction,
};
use spl_token::{
    instruction::{initialize_account3, initialize_mint2, mint_to, set_authority, AuthorityType},
    state::{Account as TokenAccount, Mint},
};
use std::collections::HashMap;

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

/// Dust floor `start()` initializes the config with — far below
/// the locks these tests register, so the export behaviour under
/// test is unchanged.
const TEST_FLOOR: u64 = 1_000;

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
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer), &all, ctx.last_blockhash);
    ctx.banks_client.process_transaction(tx).await
}

async fn next_slot(ctx: &mut ProgramTestContext) {
    let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    ctx.warp_to_slot(clock.slot + 1).unwrap();
}

async fn get_account(ctx: &mut ProgramTestContext, pubkey: &Pubkey) -> Account {
    ctx.banks_client
        .get_account(*pubkey)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("account {pubkey} missing"))
}

/// Create a mint, mint `supply` to a fresh token account, revoke both
/// authorities, and register the launch. Returns (mint, token account).
async fn register_launch(
    ctx: &mut ProgramTestContext,
    creator: &Keypair,
    supply: u64,
    lock_amount: u64,
    lock_slots: u64,
) -> (Pubkey, Pubkey) {
    let mint = Keypair::new();
    let token_acct = Keypair::new();
    let mint_pk = mint.pubkey();
    let token_pk = token_acct.pubkey();
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
            supply,
        )
        .unwrap(),
    ];
    send(ctx, &setup, &[creator, &mint, &token_acct])
        .await
        .expect("token setup");
    next_slot(ctx).await;
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
    send(ctx, &revoke, &[creator]).await.expect("revoke");
    next_slot(ctx).await;
    let data = borsh::to_vec(&LaunchInstruction::RegisterAndLock {
        lock_amount,
        lock_slots,
    })
    .unwrap();
    let register = Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(creator.pubkey(), true),
            AccountMeta::new_readonly(mint_pk, false),
            AccountMeta::new(token_pk, false),
            AccountMeta::new(pda(&[SEED_LAUNCH, mint_pk.as_ref()]), false),
            AccountMeta::new(pda(&[SEED_VAULT, mint_pk.as_ref()]), false),
            AccountMeta::new_readonly(pda(&[SEED_AUTHORITY, mint_pk.as_ref()]), false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_program::system_program::id(), false),
            AccountMeta::new(pda(&[SEED_REPUTATION, creator.pubkey().as_ref()]), false),
            AccountMeta::new_readonly(pda(&[SEED_CONFIG]), false),
        ],
        data,
    };
    send(ctx, &[register], &[creator])
        .await
        .expect("register launch");
    (mint_pk, token_pk)
}

async fn release(
    ctx: &mut ProgramTestContext,
    creator: &Keypair,
    mint: &Pubkey,
    token_acct: &Pubkey,
) {
    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(creator.pubkey(), true),
            AccountMeta::new(pda(&[SEED_LAUNCH, mint.as_ref()]), false),
            AccountMeta::new(pda(&[SEED_VAULT, mint.as_ref()]), false),
            AccountMeta::new(*token_acct, false),
            AccountMeta::new_readonly(pda(&[SEED_AUTHORITY, mint.as_ref()]), false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new(pda(&[SEED_REPUTATION, creator.pubkey().as_ref()]), false),
        ],
        data: borsh::to_vec(&LaunchInstruction::Release {}).unwrap(),
    };
    send(ctx, &[ix], &[creator]).await.expect("release");
}

#[tokio::test]
async fn registry_from_real_launches() {
    let mut ctx = start().await;
    let creator_a = Keypair::new();
    let creator_b = Keypair::new();
    // A: 10% for 30 days (the floor). B: 25% for 60 days.
    let (mint_a, _) = register_launch(&mut ctx, &creator_a, 1_000_000_000, 100_000_000, 6_480_000)
        .await;
    let (mint_b, _) =
        register_launch(&mut ctx, &creator_b, 500_000_000, 125_000_000, 12_960_000).await;

    // What getProgramAccounts would return: every account the program
    // owns — the two records plus the two reputations — plus a
    // foreign account (a mint) that the parser must ignore.
    let mut accounts: Vec<(Pubkey, Account)> = Vec::new();
    for (mint, creator) in [(mint_a, &creator_a), (mint_b, &creator_b)] {
        let record_pda = pda(&[SEED_LAUNCH, mint.as_ref()]);
        accounts.push((record_pda, get_account(&mut ctx, &record_pda).await));
        let rep_pda = pda(&[SEED_REPUTATION, creator.pubkey().as_ref()]);
        accounts.push((rep_pda, get_account(&mut ctx, &rep_pda).await));
    }
    accounts.push((mint_a, get_account(&mut ctx, &mint_a).await));

    let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    let registry = build_registry(&accounts, &program_id(), clock.slot);
    assert_eq!(registry.total_launches, 2);
    assert_eq!(registry.total_locked, 225_000_000);
    assert_eq!(registry.generated_at_slot, clock.slot);

    let entry_a = registry
        .launches
        .iter()
        .find(|e| e.mint == mint_a.to_string())
        .expect("launch A in registry");
    assert_eq!(entry_a.creator, creator_a.pubkey().to_string());
    assert_eq!(entry_a.total_supply, 1_000_000_000);
    assert_eq!(entry_a.locked_amount, 100_000_000);
    assert_eq!(entry_a.locked_pct_bps, 1_000);
    assert_eq!(entry_a.status, "locked");
    let rep_a = entry_a.reputation.as_ref().expect("reputation A joined");
    assert_eq!(rep_a.launches_registered, 1);
    assert_eq!(rep_a.locks_completed, 0);
    // The record bytes the program wrote decode identically through
    // the exporter's parser — the registry is not re-deriving them.
    let record = LaunchRecord::try_from_slice(
        &get_account(&mut ctx, &pda(&[SEED_LAUNCH, mint_a.as_ref()]))
            .await
            .data,
    )
    .unwrap();
    assert_eq!(entry_a.lock_until_slot, record.lock_until_slot);
    assert_eq!(entry_a.registered_slot, record.registered_slot);

    let entry_b = registry
        .launches
        .iter()
        .find(|e| e.mint == mint_b.to_string())
        .expect("launch B in registry");
    assert_eq!(entry_b.locked_amount, 125_000_000);
    assert_eq!(entry_b.locked_pct_bps, 2_500);
    assert_eq!(entry_b.lock_until_slot, entry_b.registered_slot + 12_960_000);

    // The registry is JSON a checker can consume directly.
    let json = serde_json::to_string_pretty(&registry).unwrap();
    assert!(json.contains(&mint_a.to_string()));
    assert!(json.contains(&mint_b.to_string()));
    let back: registry_export::Registry = serde_json::from_str(&json).unwrap();
    assert_eq!(back, registry);
}

#[tokio::test]
async fn registry_verify_and_release_against_real_accounts() {
    let mut ctx = start().await;
    let creator = Keypair::new();
    let (mint, token_acct) =
        register_launch(&mut ctx, &creator, 1_000_000_000, 100_000_000, 6_480_000).await;
    let record_pda = pda(&[SEED_LAUNCH, mint.as_ref()]);
    let vault_pda = pda(&[SEED_VAULT, mint.as_ref()]);

    // -- Verify the live lock: vault covers it, authorities revoked.
    let mut accounts = vec![
        (record_pda, get_account(&mut ctx, &record_pda).await),
        (
            pda(&[SEED_REPUTATION, creator.pubkey().as_ref()]),
            get_account(&mut ctx, &pda(&[SEED_REPUTATION, creator.pubkey().as_ref()])).await,
        ),
    ];
    let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    let mut registry = build_registry(&accounts, &program_id(), clock.slot);
    assert_eq!(registry.launches[0].status, "locked");
    let mut vaults = HashMap::new();
    vaults.insert(vault_pda, get_account(&mut ctx, &vault_pda).await);
    let mut mints = HashMap::new();
    mints.insert(mint, get_account(&mut ctx, &mint).await);
    verify_registry(&mut registry, &vaults, &mints);
    let entry = &registry.launches[0];
    assert_eq!(entry.vault_balance, Some(100_000_000));
    assert_eq!(entry.vault_verified, Some(true));
    assert_eq!(entry.authorities_revoked, Some(true));

    // -- Warp past expiry, release, re-export: released, vault empty.
    let record = LaunchRecord::try_from_slice(
        &get_account(&mut ctx, &record_pda).await.data,
    )
    .unwrap();
    ctx.warp_to_slot(record.lock_until_slot).unwrap();
    next_slot(&mut ctx).await;
    release(&mut ctx, &creator, &mint, &token_acct).await;
    accounts = vec![(record_pda, get_account(&mut ctx, &record_pda).await)];
    let clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    let mut registry = build_registry(&accounts, &program_id(), clock.slot);
    assert_eq!(registry.launches[0].status, "released");
    assert_eq!(registry.total_locked, 0);
    assert_eq!(
        LaunchRecord::try_from_slice(&get_account(&mut ctx, &record_pda).await.data)
            .unwrap()
            .state,
        STATE_RELEASED
    );
    let mut vaults = HashMap::new();
    vaults.insert(vault_pda, get_account(&mut ctx, &vault_pda).await);
    verify_registry(&mut registry, &vaults, &HashMap::new());
    let entry = &registry.launches[0];
    assert_eq!(entry.vault_balance, Some(0));
    assert_eq!(entry.vault_verified, Some(true));
    let rep = CreatorReputation::try_from_slice(
        &get_account(&mut ctx, &pda(&[SEED_REPUTATION, creator.pubkey().as_ref()]))
            .await
            .data,
    )
    .unwrap();
    assert_eq!(rep.locks_completed, 1);
}
