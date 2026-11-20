//! RugCall integration tests: InitializeConfig, CreateMarket and
//! BuyPosition, driven through the real processor in
//! solana-program-test against the real SPL token program.
//! (ResolveMarket, Claim and CancelMarket are still stubs; one
//! test pins that they keep returning NotImplemented.)
//!
//! Time is controlled ONLY through the Clock sysvar (`set_clock`
//! below): program-test's `warp_to_slot` does not reliably
//! advance `unix_timestamp`, and no test in this file warps.
//! Every time-sensitive transaction is preceded immediately by a
//! `set_clock` call, and `create_market_happy_path` doubles as
//! the program-level clock probe — its `created_at` assertion
//! fails if the sysvar never took effect.
//!
//! Pool tests use asymmetric amounts throughout: symmetric pools
//! would let a Flagged/Survives side-mapping bug pass silently.
//!
//! Run with:
//!     cd programs && cargo test --release --test rug_call

use borsh::BorshDeserialize;
use rug_call::state::*;
use solana_program::{
    clock::Clock,
    instruction::{AccountMeta, Instruction, InstructionError},
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
    instruction::{initialize_account3, initialize_mint2, mint_to},
    state::Mint,
};

/// Fixed wall-clock the suite creates markets at. Expiry for a
/// market created at T0 is T0 + 86_400 (Config duration).
const T0: i64 = 1_800_000_000;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([82; 32])
}

fn pda(seeds: &[&[u8]]) -> (Pubkey, u8) {
    Pubkey::find_program_address(seeds, &program_id())
}

fn config_pda() -> Pubkey {
    pda(&[SEED_CONFIG]).0
}

fn market_pda(subject_mint: &Pubkey, created_at: i64) -> Pubkey {
    pda(&[SEED_MARKET, subject_mint.as_ref(), &created_at.to_le_bytes()]).0
}

fn live_pda(subject_mint: &Pubkey) -> Pubkey {
    pda(&[SEED_LIVE_MARKET, subject_mint.as_ref()]).0
}

fn vault_pda(market: &Pubkey) -> Pubkey {
    pda(&[SEED_VAULT, market.as_ref()]).0
}

fn position_pda(market: &Pubkey, user: &Pubkey) -> Pubkey {
    pda(&[SEED_POSITION, market.as_ref(), user.as_ref()]).0
}

fn subject_a() -> Pubkey {
    Pubkey::new_from_array([42; 32])
}

fn subject_b() -> Pubkey {
    Pubkey::new_from_array([43; 32])
}

/// The program's ProgramData address under the BPF upgradeable
/// loader — where `InitializeConfig` looks for the upgrade
/// authority.
#[allow(deprecated)]
fn programdata_address() -> Pubkey {
    solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id())
}

/// Seed a ProgramData account naming `authority` as the program's
/// upgrade authority (loader layout: variant tag 3, slot, option
/// tag, authority — the layout the program parses).
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

// ---------------------------------------------------------------------------
// Clock control. The ONLY way time moves in this suite.
// ---------------------------------------------------------------------------

/// Set the wall clock to `ts` (unix seconds) and prove it landed
/// by reading the sysvar back. If this assert ever fires,
/// `set_sysvar` is not taking effect in this program-test
/// version — stop and escalate; do not fall back to warp_to_slot
/// silently.
async fn set_clock(ctx: &mut ProgramTestContext, ts: i64) {
    let mut clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    clock.unix_timestamp = ts;
    ctx.set_sysvar(&clock);
    let readback: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    assert_eq!(
        readback.unix_timestamp, ts,
        "harness clock probe: set_sysvar did not take effect"
    );
}

// ---------------------------------------------------------------------------
// Instruction builders. Account order is the processor's order:
// the signer first, Config second in every handler.
// ---------------------------------------------------------------------------

fn initialize_config_ix(
    authority: &Pubkey,
    resolver: &Pubkey,
    collateral_mint: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new(config_pda(), false),
            AccountMeta::new_readonly(*collateral_mint, false),
            AccountMeta::new_readonly(programdata_address(), false),
            AccountMeta::new_readonly(solana_program::system_program::id(), false),
        ],
        data: borsh::to_vec(&RugCallInstruction::InitializeConfig {
            resolver: *resolver,
            collateral_mint: *collateral_mint,
        })
        .unwrap(),
    }
}

/// `created_at` must equal the timestamp most recently passed to
/// `set_clock`: the market PDA seed IS the creation second, so
/// the builder and the harness clock cannot disagree silently —
/// a mismatch derives the wrong PDA and the create fails.
fn create_market_ix(
    creator: &Pubkey,
    subject_mint: &Pubkey,
    created_at: i64,
    collateral_mint: &Pubkey,
) -> Instruction {
    let market = market_pda(subject_mint, created_at);
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*creator, true),
            AccountMeta::new_readonly(config_pda(), false),
            AccountMeta::new(market, false),
            AccountMeta::new(live_pda(subject_mint), false),
            AccountMeta::new(vault_pda(&market), false),
            AccountMeta::new_readonly(*collateral_mint, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_program::system_program::id(), false),
        ],
        data: borsh::to_vec(&RugCallInstruction::CreateMarket {
            subject_mint: *subject_mint,
        })
        .unwrap(),
    }
}

fn buy_ix(
    buyer: &Pubkey,
    buyer_token: &Pubkey,
    market: &Pubkey,
    side: u8,
    amount: u64,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*buyer, true),
            AccountMeta::new_readonly(config_pda(), false),
            AccountMeta::new(*market, false),
            AccountMeta::new(vault_pda(market), false),
            AccountMeta::new(*buyer_token, false),
            AccountMeta::new(position_pda(market, buyer), false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_program::system_program::id(), false),
        ],
        data: borsh::to_vec(&RugCallInstruction::BuyPosition { side, amount }).unwrap(),
    }
}

// ---------------------------------------------------------------------------
// Start helpers and setup.
// ---------------------------------------------------------------------------

async fn start_uninitialized() -> (ProgramTestContext, Keypair) {
    let authority = Keypair::new();
    let mut test = ProgramTest::new(
        "rug_call",
        program_id(),
        processor!(rug_call::process_instruction),
    );
    seed_programdata(&mut test, &authority.pubkey());
    let ctx = test.start_with_context().await;
    (ctx, authority)
}

/// Uninitialized harness + collateral mint + InitializeConfig.
/// Returns (ctx, authority, resolver, collateral_mint).
async fn start_initialized() -> (ProgramTestContext, Keypair, Keypair, Pubkey) {
    let (mut ctx, authority) = start_uninitialized().await;
    let mint = make_collateral_mint(&mut ctx).await;
    let resolver = Keypair::new();
    initialize_config(&mut ctx, &authority, &resolver.pubkey(), &mint)
        .await
        .expect("initialize config");
    (ctx, authority, resolver, mint)
}

/// Start with a Config account written directly into the harness
/// (bypassing InitializeConfig) carrying caller-chosen `min_stake`
/// and `default_duration_secs`. This is the only honest way to
/// prove the handlers read those values from Config rather than
/// from the constants that normally seed it: InitializeConfig
/// always installs the constants, so the two sources are
/// indistinguishable through the front door.
async fn start_with_fixture_config(
    min_stake: u64,
    default_duration_secs: i64,
) -> (ProgramTestContext, Keypair, Pubkey) {
    let authority = Keypair::new();
    let mint_keypair = Keypair::new();
    let fixture = RugCallConfig {
        discriminator: DISCRIMINATOR_CONFIG,
        admin: authority.pubkey(),
        resolver: Keypair::new().pubkey(),
        collateral_mint: mint_keypair.pubkey(),
        fee_bps: FEE_BPS,
        default_duration_secs,
        min_stake,
        bump: pda(&[SEED_CONFIG]).1,
    };
    let mut test = ProgramTest::new(
        "rug_call",
        program_id(),
        processor!(rug_call::process_instruction),
    );
    seed_programdata(&mut test, &authority.pubkey());
    test.add_account(
        config_pda(),
        Account {
            lamports: 10_000_000,
            data: borsh::to_vec(&fixture).unwrap(),
            owner: program_id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = test.start_with_context().await;
    make_mint(&mut ctx, &mint_keypair, 6).await;
    (ctx, authority, mint_keypair.pubkey())
}

async fn initialize_config(
    ctx: &mut ProgramTestContext,
    authority: &Keypair,
    resolver: &Pubkey,
    mint: &Pubkey,
) -> Result<(), BanksClientError> {
    fund(ctx, &authority.pubkey(), 1_000_000_000).await?;
    send(
        ctx,
        &[initialize_config_ix(&authority.pubkey(), resolver, mint)],
        &[authority],
    )
    .await
}

async fn fund(
    ctx: &mut ProgramTestContext,
    to: &Pubkey,
    lamports: u64,
) -> Result<(), BanksClientError> {
    let ix = system_instruction::transfer(&ctx.payer.pubkey(), to, lamports);
    send(ctx, &[ix], &[]).await
}

/// Create a real SPL mint with the given keypair and decimals.
/// Mint authority is the harness payer; NO freeze authority (the
/// dUSDC policy: the mint authority stays alive for top-ups, and
/// this program never inspects mint authority at all).
async fn make_mint(ctx: &mut ProgramTestContext, mint: &Keypair, decimals: u8) {
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let payer = ctx.payer.pubkey();
    let ixs = vec![
        system_instruction::create_account(
            &payer,
            &mint.pubkey(),
            rent.minimum_balance(Mint::LEN),
            Mint::LEN as u64,
            &spl_token::id(),
        ),
        initialize_mint2(&spl_token::id(), &mint.pubkey(), &payer, None, decimals).unwrap(),
    ];
    send(ctx, &ixs, &[mint]).await.expect("mint setup");
}

/// The test collateral mint: 6 decimals, like dUSDC.
async fn make_collateral_mint(ctx: &mut ProgramTestContext) -> Pubkey {
    let mint = Keypair::new();
    make_mint(ctx, &mint, 6).await;
    mint.pubkey()
}

/// A funded user keypair plus its collateral token account
/// holding exactly `amount` raw units.
async fn make_user(ctx: &mut ProgramTestContext, mint: &Pubkey, amount: u64) -> (Keypair, Pubkey) {
    let user = Keypair::new();
    let token_acct = Keypair::new();
    fund(ctx, &user.pubkey(), 2_000_000_000)
        .await
        .expect("fund user");
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let payer = ctx.payer.pubkey();
    let mut ixs = vec![
        system_instruction::create_account(
            &user.pubkey(),
            &token_acct.pubkey(),
            rent.minimum_balance(spl_token::state::Account::LEN),
            spl_token::state::Account::LEN as u64,
            &spl_token::id(),
        ),
        initialize_account3(&spl_token::id(), &token_acct.pubkey(), mint, &user.pubkey()).unwrap(),
    ];
    if amount > 0 {
        ixs.push(
            mint_to(&spl_token::id(), mint, &token_acct.pubkey(), &payer, &[], amount).unwrap(),
        );
    }
    send(ctx, &ixs, &[&user, &token_acct])
        .await
        .expect("user token setup");
    (user, token_acct.pubkey())
}

/// A funded creator keypair (pays market/vault rent).
async fn make_creator(ctx: &mut ProgramTestContext) -> Keypair {
    let creator = Keypair::new();
    fund(ctx, &creator.pubkey(), 2_000_000_000)
        .await
        .expect("fund creator");
    creator
}

/// Create a market on `subject` at the harness clock's current
/// setting; the caller must `set_clock` first.
async fn create_market(
    ctx: &mut ProgramTestContext,
    creator: &Keypair,
    subject: &Pubkey,
    created_at: i64,
    mint: &Pubkey,
) -> Result<Pubkey, BanksClientError> {
    let ix = create_market_ix(&creator.pubkey(), subject, created_at, mint);
    send(ctx, &[ix], &[creator]).await?;
    Ok(market_pda(subject, created_at))
}

async fn buy(
    ctx: &mut ProgramTestContext,
    user: &Keypair,
    user_token: &Pubkey,
    market: &Pubkey,
    side: u8,
    amount: u64,
) -> Result<(), BanksClientError> {
    send(
        ctx,
        &[buy_ix(&user.pubkey(), user_token, market, side, amount)],
        &[user],
    )
    .await
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
        InstructionError::Custom(c),
    )) = err
    else {
        panic!("expected Custom({code}), got {err:?}");
    };
    assert_eq!(*c, code);
}

fn expect_instruction_error(err: &BanksClientError, expected: InstructionError) {
    let BanksClientError::TransactionError(TransactionError::InstructionError(_, kind)) = err
    else {
        panic!("expected instruction error {expected:?}, got {err:?}");
    };
    assert_eq!(*kind, expected);
}

// ---------------------------------------------------------------------------
// Read helpers.
// ---------------------------------------------------------------------------

async fn read_config(ctx: &mut ProgramTestContext) -> Option<RugCallConfig> {
    let acc = ctx.banks_client.get_account(config_pda()).await.unwrap()?;
    RugCallConfig::try_from_slice(&acc.data).ok()
}

async fn read_market(ctx: &mut ProgramTestContext, market: &Pubkey) -> Option<Market> {
    let acc = ctx.banks_client.get_account(*market).await.unwrap()?;
    Market::try_from_slice(&acc.data).ok()
}

async fn read_position(
    ctx: &mut ProgramTestContext,
    market: &Pubkey,
    user: &Pubkey,
) -> Option<Position> {
    let acc = ctx
        .banks_client
        .get_account(position_pda(market, user))
        .await
        .unwrap()?;
    Position::try_from_slice(&acc.data).ok()
}

async fn read_live_market(ctx: &mut ProgramTestContext, subject: &Pubkey) -> Option<LiveMarket> {
    let acc = ctx.banks_client.get_account(live_pda(subject)).await.unwrap()?;
    LiveMarket::try_from_slice(&acc.data).ok()
}

async fn read_token_account(ctx: &mut ProgramTestContext, token: &Pubkey) -> spl_token::state::Account {
    let acc = ctx
        .banks_client
        .get_account(*token)
        .await
        .unwrap()
        .expect("token account exists");
    spl_token::state::Account::unpack(&acc.data).unwrap()
}

async fn token_balance(ctx: &mut ProgramTestContext, token: &Pubkey) -> u64 {
    read_token_account(ctx, token).await.amount
}

/// The standing accounting invariant, asserted after buys: the
/// pools sum to the vault balance, and the vault holds exactly
/// what the positions claim (no fee, no skim, no rounding).
async fn assert_accounting(
    ctx: &mut ProgramTestContext,
    market_addr: &Pubkey,
    flagged: u64,
    survives: u64,
) {
    let m = read_market(ctx, market_addr).await.expect("market exists");
    assert_eq!(m.flagged_pool, flagged, "flagged pool");
    assert_eq!(m.survives_pool, survives, "survives pool");
    let vault = token_balance(ctx, &vault_pda(market_addr)).await;
    assert_eq!(vault, flagged + survives, "vault balance == pool total");
}

// ---------------------------------------------------------------------------
// InitializeConfig.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_happy_path() {
    let (mut ctx, authority) = start_uninitialized().await;
    let mint = make_collateral_mint(&mut ctx).await;
    let resolver = Keypair::new();
    initialize_config(&mut ctx, &authority, &resolver.pubkey(), &mint)
        .await
        .expect("initialize");
    let cfg = read_config(&mut ctx).await.expect("config exists");
    assert_eq!(cfg.discriminator, DISCRIMINATOR_CONFIG);
    assert_eq!(cfg.admin, authority.pubkey());
    assert_eq!(cfg.resolver, resolver.pubkey());
    assert_eq!(cfg.collateral_mint, mint);
    // The payload names only resolver and mint; everything else
    // is the crate's constants, installed verbatim.
    assert_eq!(cfg.fee_bps, FEE_BPS);
    assert_eq!(cfg.fee_bps, 0);
    assert_eq!(cfg.default_duration_secs, DEFAULT_MARKET_DURATION_SECS);
    assert_eq!(cfg.default_duration_secs, 86_400);
    assert_eq!(cfg.min_stake, MIN_STAKE_RAW);
    assert_eq!(cfg.min_stake, 1_000_000);
    assert_eq!(cfg.bump, pda(&[SEED_CONFIG]).1);
    let acc = ctx
        .banks_client
        .get_account(config_pda())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acc.owner, program_id());
    assert_eq!(acc.data.len(), RugCallConfig::LEN);
}

#[tokio::test]
async fn initialize_twice_rejected() {
    let (mut ctx, authority, resolver, mint) = start_initialized().await;
    // Same authority, different resolver payload: exactly-once is
    // an anti-rotation invariant — the second call must not be
    // able to swap the resolver Config already names.
    let err = initialize_config(&mut ctx, &authority, &Keypair::new().pubkey(), &mint)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::ConfigAlreadyInitialized as u32); // 1
    let cfg = read_config(&mut ctx).await.unwrap();
    assert_eq!(cfg.resolver, resolver.pubkey());
    assert_eq!(cfg.collateral_mint, mint);
}

#[tokio::test]
async fn initialize_by_non_upgrade_authority_rejected() {
    let (mut ctx, authority) = start_uninitialized().await;
    let mint = make_collateral_mint(&mut ctx).await;
    let stranger = Keypair::new();
    fund(&mut ctx, &stranger.pubkey(), 1_000_000_000)
        .await
        .expect("fund stranger");
    let err = send(
        &mut ctx,
        &[initialize_config_ix(
            &stranger.pubkey(),
            &Keypair::new().pubkey(),
            &mint,
        )],
        &[&stranger],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::NotUpgradeAuthority as u32); // 3
    assert!(read_config(&mut ctx).await.is_none());
    // The failed attempt set nothing: the authority still can.
    initialize_config(&mut ctx, &authority, &Keypair::new().pubkey(), &mint)
        .await
        .expect("authority initializes after stranger's failure");
}

#[tokio::test]
async fn initialize_with_wrong_decimals_mint_rejected() {
    // MIN_STAKE_RAW is denominated at 6 decimals; a mint with any
    // other decimals would silently rescale every stake in the
    // program, and Config is immutable — the mistake must be
    // caught here, at the one call that can still refuse it.
    let (mut ctx, authority) = start_uninitialized().await;
    let mint = Keypair::new();
    make_mint(&mut ctx, &mint, 9).await;
    fund(&mut ctx, &authority.pubkey(), 1_000_000_000)
        .await
        .expect("fund authority");
    let err = send(
        &mut ctx,
        &[initialize_config_ix(
            &authority.pubkey(),
            &Keypair::new().pubkey(),
            &mint.pubkey(),
        )],
        &[&authority],
    )
    .await
    .unwrap_err();
    expect_instruction_error(&err, InstructionError::InvalidAccountData);
    assert!(read_config(&mut ctx).await.is_none());
}

#[tokio::test]
async fn create_before_initialize_fails_closed() {
    let (mut ctx, _authority) = start_uninitialized().await;
    let creator = make_creator(&mut ctx).await;
    // A collateral mint exists; what is missing is Config, and
    // without it there is no mint to check the create against.
    let mint = make_collateral_mint(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    let err = send(
        &mut ctx,
        &[create_market_ix(&creator.pubkey(), &subject_a(), T0, &mint)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::ConfigNotInitialized as u32); // 2
    assert!(read_market(&mut ctx, &market_pda(&subject_a(), T0))
        .await
        .is_none());
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_none());
}

// ---------------------------------------------------------------------------
// CreateMarket.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_market_happy_path() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    let market_addr = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let m = read_market(&mut ctx, &market_addr).await.expect("market exists");
    assert_eq!(m.discriminator, DISCRIMINATOR_MARKET);
    assert_eq!(m.subject_mint, subject_a());
    assert_eq!(m.creator, creator.pubkey());
    // CLOCK PROBE: created_at must equal the set timestamp. If
    // this fails while set_clock's read-back passed, the program
    // is not reading the Clock sysvar.
    assert_eq!(m.created_at, T0);
    assert_eq!(m.expiry, T0 + 86_400);
    assert_eq!(m.flagged_pool, 0);
    assert_eq!(m.survives_pool, 0);
    assert_eq!(m.state, STATE_OPEN);
    assert_eq!(m.outcome, OUTCOME_NONE);
    assert_eq!(m.evidence_hash, [0u8; 32]);
    assert_eq!(m.hard_trigger, HARD_TRIGGER_NONE);
    assert_eq!(m.resolved_at, 0);
    assert_eq!(
        m.bump,
        pda(&[SEED_MARKET, subject_a().as_ref(), &T0.to_le_bytes()]).1
    );
    assert_eq!(m.vault_bump, pda(&[SEED_VAULT, market_addr.as_ref()]).1);
    let live = read_live_market(&mut ctx, &subject_a())
        .await
        .expect("live lock exists");
    assert_eq!(live.discriminator, DISCRIMINATOR_LIVE_MARKET);
    assert_eq!(live.market, market_addr);
    assert_eq!(live.bump, pda(&[SEED_LIVE_MARKET, subject_a().as_ref()]).1);
    let vault = read_token_account(&mut ctx, &vault_pda(&market_addr)).await;
    assert_eq!(vault.mint, mint);
    assert_eq!(vault.owner, market_addr);
    assert_eq!(vault.amount, 0);
}

#[tokio::test]
async fn create_duplicate_live_market_rejected() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("first market");
    // A different second: the second market PDA differs, so a
    // rejection here can only be the LiveMarket lock speaking,
    // not a PDA collision.
    set_clock(&mut ctx, T0 + 10).await;
    let err = send(
        &mut ctx,
        &[create_market_ix(&creator.pubkey(), &subject_a(), T0 + 10, &mint)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::DuplicateLiveMarket as u32); // 7
    assert!(read_market(&mut ctx, &market_pda(&subject_a(), T0 + 10))
        .await
        .is_none());
    assert_eq!(
        read_live_market(&mut ctx, &subject_a()).await.unwrap().market,
        market_pda(&subject_a(), T0)
    );
}

#[tokio::test]
async fn create_duplicate_same_second_rejected() {
    // Same second: the market PDA would collide too, but the
    // lock check runs first, so the error is the honest one. The
    // second attempt comes from a different creator — a resend
    // from the same creator would be a byte-identical transaction
    // that the harness reports as already-processed without
    // re-executing, which would test nothing.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let other_creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("first market");
    let err = send(
        &mut ctx,
        &[create_market_ix(&other_creator.pubkey(), &subject_a(), T0, &mint)],
        &[&other_creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::DuplicateLiveMarket as u32); // 7
}

#[tokio::test]
async fn create_second_market_on_different_mint_succeeds() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    let a = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("market A");
    let b = create_market(&mut ctx, &creator, &subject_b(), T0, &mint)
        .await
        .expect("market B on a different subject mint");
    assert_ne!(a, b);
    assert_eq!(
        read_live_market(&mut ctx, &subject_b()).await.unwrap().market,
        b
    );
}

#[tokio::test]
async fn create_expiry_at_i64_boundary_succeeds() {
    // created_at + duration landing exactly on i64::MAX is a
    // valid market. The Clock sysvar is harness-settable, so this
    // precondition is honestly constructible here even though no
    // real cluster's clock will reach it for ~292 billion years.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let edge = i64::MAX - 86_400;
    set_clock(&mut ctx, edge).await;
    let market_addr = create_market(&mut ctx, &creator, &subject_a(), edge, &mint)
        .await
        .expect("create at the i64 boundary");
    let m = read_market(&mut ctx, &market_addr).await.unwrap();
    assert_eq!(m.created_at, edge);
    assert_eq!(m.expiry, i64::MAX);
}

#[tokio::test]
async fn create_expiry_overflow_rejected() {
    // One second later and the checked add has no answer: the
    // create is refused with Overflow, and nothing is created —
    // a wrapping add would have minted a market born expired.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let edge = i64::MAX - 86_399;
    set_clock(&mut ctx, edge).await;
    let err = send(
        &mut ctx,
        &[create_market_ix(&creator.pubkey(), &subject_a(), edge, &mint)],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::Overflow as u32); // 21
    assert!(read_market(&mut ctx, &market_pda(&subject_a(), edge))
        .await
        .is_none());
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_none());
}

// ---------------------------------------------------------------------------
// BuyPosition.
// ---------------------------------------------------------------------------

/// The standing buy sequence: two wallets, opposite sides, a
/// repeat buy that must accumulate into the same position, and a
/// hedge at exactly the minimum stake. Every step asserts the
/// full accounting state, so a side swap anywhere shows up in the
/// wrong pool immediately and never recovers.
#[tokio::test]
async fn buy_sequence_asymmetric_pools() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");

    // Step 1: Alice stakes 3 dUSDC on Flagged.
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 3_000_000)
        .await
        .expect("alice buys flagged");
    assert_accounting(&mut ctx, &market, 3_000_000, 0).await;
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 17_000_000);
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert_eq!((pos.flagged_amount, pos.survives_amount), (3_000_000, 0));
    assert!(!pos.claimed);

    // Step 2: Bob stakes 7 dUSDC on Survives.
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("bob buys survives");
    assert_accounting(&mut ctx, &market, 3_000_000, 7_000_000).await;
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 13_000_000);
    let pos = read_position(&mut ctx, &market, &bob.pubkey()).await.unwrap();
    assert_eq!((pos.flagged_amount, pos.survives_amount), (0, 7_000_000));

    // Step 3: Alice adds 2 more on Flagged — same position,
    // accumulated, never reset.
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 2_000_000)
        .await
        .expect("alice adds to flagged");
    assert_accounting(&mut ctx, &market, 5_000_000, 7_000_000).await;
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 15_000_000);
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert_eq!((pos.flagged_amount, pos.survives_amount), (5_000_000, 0));
    assert!(!pos.claimed);

    // Step 4: Bob hedges with exactly the minimum stake on
    // Flagged — the second side of an existing position.
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("bob hedges at min stake");
    assert_accounting(&mut ctx, &market, 6_000_000, 7_000_000).await;
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 12_000_000);
    let pos = read_position(&mut ctx, &market, &bob.pubkey()).await.unwrap();
    assert_eq!(
        (pos.flagged_amount, pos.survives_amount),
        (1_000_000, 7_000_000)
    );
}

#[tokio::test]
async fn buy_exactly_min_stake_succeeds() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("buy at exactly the minimum");
    assert_accounting(&mut ctx, &market, 1_000_000, 0).await;
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert_eq!(pos.flagged_amount, 1_000_000);
}

#[tokio::test]
async fn buy_one_below_min_stake_rejected() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let err = buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 999_999)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::BelowMinStake as u32); // 9
    assert!(read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .is_none());
    assert_accounting(&mut ctx, &market, 0, 0).await;
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 5_000_000);
}

#[tokio::test]
async fn buy_zero_amount_rejected() {
    // Zero rides the min-stake check — there is no separate
    // zero-amount error, and there must be no zero-stake position.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let err = buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 0)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::BelowMinStake as u32); // 9
    assert!(read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .is_none());
    assert_accounting(&mut ctx, &market, 0, 0).await;
}

#[tokio::test]
async fn buy_at_expiry_minus_one_succeeds() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    set_clock(&mut ctx, T0 + 86_399).await;
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("buy in the last open second");
    assert_accounting(&mut ctx, &market, 1_000_000, 0).await;
}

#[tokio::test]
async fn buy_at_expiry_rejected() {
    // At now == expiry the resolution path wins the tie.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::BuyAfterExpiry as u32); // 11
    assert!(read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .is_none());
    assert_accounting(&mut ctx, &market, 0, 0).await;
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 5_000_000);
}

#[tokio::test]
async fn buy_with_wrong_mint_token_account_rejected() {
    // A second mint, also 6 decimals — mint identity is the
    // check, not decimals.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let other_mint = make_collateral_mint(&mut ctx).await;
    let (alice, alice_wrong_token) = make_user(&mut ctx, &other_mint, 5_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let err = buy(
        &mut ctx,
        &alice,
        &alice_wrong_token,
        &market,
        SIDE_FLAGGED,
        1_000_000,
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::MintMismatch as u32); // 5
    assert!(read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .is_none());
    assert_accounting(&mut ctx, &market, 0, 0).await;
    assert_eq!(token_balance(&mut ctx, &alice_wrong_token).await, 5_000_000);
}

#[tokio::test]
async fn buy_invalid_side_zero_rejected() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let err = buy(&mut ctx, &alice, &alice_token, &market, 0, 1_000_000)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::InvalidSide as u32); // 8
    assert!(read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .is_none());
    assert_accounting(&mut ctx, &market, 0, 0).await;
}

#[tokio::test]
async fn buy_invalid_side_three_rejected() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let err = buy(&mut ctx, &alice, &alice_token, &market, 3, 1_000_000)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::InvalidSide as u32); // 8
    assert!(read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .is_none());
    assert_accounting(&mut ctx, &market, 0, 0).await;
}

// ---------------------------------------------------------------------------
// BuyPosition — the overflow boundary, honestly constructed.
//
// A successful buy can never overflow a pool through legitimate
// token flows: pools are bounded by the vault balance, which is
// bounded by mint supply (u64). The checked adds are
// defense-in-depth. But the Overflow ERROR is reachable — because
// the math runs before the transfer — and these tests pin both
// the boundary and the ordering decision. They mint real supply
// in the harness; token amounts are not lamports, so u64-scale
// fixtures cost nothing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn buy_overflow_at_u64_max_boundary() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    // Alice holds the entire possible supply.
    let (alice, alice_token) = make_user(&mut ctx, &mint, u64::MAX).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 0).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");

    // Buy to one min-stake below the ceiling: succeeds.
    buy(
        &mut ctx,
        &alice,
        &alice_token,
        &market,
        SIDE_FLAGGED,
        u64::MAX - 1_000_000,
    )
    .await
    .expect("buy to MAX - min");
    assert_accounting(&mut ctx, &market, u64::MAX - 1_000_000, 0).await;
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 1_000_000);

    // Buy the last min-stake: the checked add lands exactly on
    // u64::MAX and passes — the boundary is inclusive.
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("buy to exactly MAX");
    assert_accounting(&mut ctx, &market, u64::MAX, 0).await;
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 0);
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert_eq!(pos.flagged_amount, u64::MAX);

    // One more min-stake, from a wallet that does not hold it:
    // the refusal must be this program's Overflow — the math runs
    // before the transfer, so the token program's insufficient-
    // funds error never gets a say. And a failed buy leaves no
    // position account behind.
    let err = buy(&mut ctx, &bob, &bob_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::Overflow as u32); // 21
    assert!(read_position(&mut ctx, &market, &bob.pubkey())
        .await
        .is_none());
    assert_accounting(&mut ctx, &market, u64::MAX, 0).await;
}

#[tokio::test]
async fn buy_total_only_overflow_rejected() {
    // Isolates the prospective-total check: Carol's side pool and
    // her position both fit comfortably — only the total crosses
    // u64::MAX. An implementation that checked side and position
    // but not the total would pass its math and die in the
    // transfer with a different error; this test catches that.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, u64::MAX - 1_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 1_000_000).await;
    let (carol, carol_token) = make_user(&mut ctx, &mint, 0).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(
        &mut ctx,
        &alice,
        &alice_token,
        &market,
        SIDE_FLAGGED,
        u64::MAX - 1_000_000,
    )
    .await
    .expect("alice fills flagged");
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_SURVIVES, 1_000_000)
        .await
        .expect("bob fills survives to a total of exactly MAX");
    assert_accounting(&mut ctx, &market, u64::MAX - 1_000_000, 1_000_000).await;

    let err = buy(&mut ctx, &carol, &carol_token, &market, SIDE_SURVIVES, 1_000_000)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::Overflow as u32); // 21
    assert!(read_position(&mut ctx, &market, &carol.pubkey())
        .await
        .is_none());
    assert_accounting(&mut ctx, &market, u64::MAX - 1_000_000, 1_000_000).await;
}

#[tokio::test]
async fn buy_huge_unbacked_amount_fails_on_funds_not_overflow() {
    // The complement of the overflow tests: on an empty market a
    // u64::MAX buy PASSES the checked math (0 + MAX fits) and must
    // fail as what it is — an unbacked amount — never as
    // Overflow. The suite must not grow a superstition that every
    // huge amount is error 21.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (dave, dave_token) = make_user(&mut ctx, &mint, 0).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let err = buy(&mut ctx, &dave, &dave_token, &market, SIDE_FLAGGED, u64::MAX)
        .await
        .unwrap_err();
    expect_instruction_error(&err, InstructionError::InsufficientFunds);
    assert!(read_position(&mut ctx, &market, &dave.pubkey())
        .await
        .is_none());
    assert_accounting(&mut ctx, &market, 0, 0).await;
}

// ---------------------------------------------------------------------------
// Config is the source — not the constants.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_min_stake_comes_from_config_account() {
    // Fixture Config carries min_stake = 2 dUSDC. A 1.5 dUSDC buy
    // — above the constant the front door would have installed —
    // must be rejected, and a 2 dUSDC buy must succeed.
    let (mut ctx, _authority, mint) = start_with_fixture_config(2_000_000, 86_400).await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let err = buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 1_500_000)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::BelowMinStake as u32); // 9
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 2_000_000)
        .await
        .expect("buy at the fixture minimum");
    assert_accounting(&mut ctx, &market, 2_000_000, 0).await;
}

#[tokio::test]
async fn config_duration_comes_from_config_account() {
    // Fixture Config carries a one-hour duration; the market's
    // expiry must be created_at + 3_600, not + 86_400.
    let (mut ctx, _authority, mint) = start_with_fixture_config(1_000_000, 3_600).await;
    let creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.created_at, T0);
    assert_eq!(m.expiry, T0 + 3_600);
}

// ---------------------------------------------------------------------------
// Day 3's handlers are not half-landed.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unimplemented_handlers_still_return_not_implemented() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![],
        data: borsh::to_vec(&RugCallInstruction::ResolveMarket {
            outcome: OUTCOME_FLAGGED,
            hard_trigger: 3,
            evidence_hash: [9u8; 32],
        })
        .unwrap(),
    };
    let err = send(&mut ctx, &[ix], &[]).await.unwrap_err();
    expect_custom(&err, RugCallError::NotImplemented as u32); // 22
}
