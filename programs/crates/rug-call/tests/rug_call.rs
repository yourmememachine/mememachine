//! RugCall integration tests: all six instructions, driven
//! through the real processor in solana-program-test against
//! the real SPL token program. Day 2 covered InitializeConfig,
//! CreateMarket and BuyPosition; Day 3 adds ResolveMarket,
//! Claim and CancelMarket with the full adversarial suite
//! (invariants M-2..M-7 from the design record).
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

/// ResolveMarket accounts: [resolver, config, market,
/// live_market, creator]. The creator entry receives the closed
/// lock's rent, so it is writable; it never signs.
fn resolve_ix(
    resolver: &Pubkey,
    market: &Pubkey,
    subject_mint: &Pubkey,
    creator: &Pubkey,
    outcome: u8,
    hard_trigger: u8,
    evidence_hash: [u8; 32],
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*resolver, true),
            AccountMeta::new_readonly(config_pda(), false),
            AccountMeta::new(*market, false),
            AccountMeta::new(live_pda(subject_mint), false),
            AccountMeta::new(*creator, false),
        ],
        data: borsh::to_vec(&RugCallInstruction::ResolveMarket {
            outcome,
            hard_trigger,
            evidence_hash,
        })
        .unwrap(),
    }
}

/// Claim accounts: [user, config, market, vault, user_token,
/// position, token_program]. The market is readonly — Claim
/// never writes it (invariant M-1); it appears in the token CPI
/// as the vault's authority. Claim creates and closes nothing,
/// so there is no system-program entry.
fn claim_ix(user: &Pubkey, market: &Pubkey, user_token: &Pubkey) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*user, true),
            AccountMeta::new_readonly(config_pda(), false),
            AccountMeta::new_readonly(*market, false),
            AccountMeta::new(vault_pda(market), false),
            AccountMeta::new(*user_token, false),
            AccountMeta::new(position_pda(market, user), false),
            AccountMeta::new_readonly(spl_token::id(), false),
        ],
        data: borsh::to_vec(&RugCallInstruction::Claim {}).unwrap(),
    }
}

/// CancelMarket accounts: [caller, config, market, live_market,
/// creator] — the same shape as ResolveMarket.
fn cancel_ix(
    caller: &Pubkey,
    market: &Pubkey,
    subject_mint: &Pubkey,
    creator: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*caller, true),
            AccountMeta::new_readonly(config_pda(), false),
            AccountMeta::new(*market, false),
            AccountMeta::new(live_pda(subject_mint), false),
            AccountMeta::new(*creator, false),
        ],
        data: borsh::to_vec(&RugCallInstruction::CancelMarket {}).unwrap(),
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
    let token = make_user_with(ctx, &user, mint, amount).await;
    (user, token)
}

/// `make_user` with the keypair supplied by the caller — the
/// fixture tests seed a Position for a known user before the
/// harness starts, so the user cannot be invented afterwards.
async fn make_user_with(
    ctx: &mut ProgramTestContext,
    user: &Keypair,
    mint: &Pubkey,
    amount: u64,
) -> Pubkey {
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
    send(ctx, &ixs, &[user, &token_acct])
        .await
        .expect("user token setup");
    token_acct.pubkey()
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

#[allow(clippy::too_many_arguments)]
async fn resolve(
    ctx: &mut ProgramTestContext,
    resolver: &Keypair,
    market: &Pubkey,
    subject_mint: &Pubkey,
    creator: &Pubkey,
    outcome: u8,
    hard_trigger: u8,
    evidence_hash: [u8; 32],
) -> Result<(), BanksClientError> {
    send(
        ctx,
        &[resolve_ix(
            &resolver.pubkey(),
            market,
            subject_mint,
            creator,
            outcome,
            hard_trigger,
            evidence_hash,
        )],
        &[resolver],
    )
    .await
}

async fn claim(
    ctx: &mut ProgramTestContext,
    user: &Keypair,
    market: &Pubkey,
    user_token: &Pubkey,
) -> Result<(), BanksClientError> {
    send(
        ctx,
        &[claim_ix(&user.pubkey(), market, user_token)],
        &[user],
    )
    .await
}

async fn cancel(
    ctx: &mut ProgramTestContext,
    caller: &Keypair,
    market: &Pubkey,
    subject_mint: &Pubkey,
    creator: &Pubkey,
) -> Result<(), BanksClientError> {
    send(
        ctx,
        &[cancel_ix(&caller.pubkey(), market, subject_mint, creator)],
        &[caller],
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

/// Send with an explicit fee payer. A test that must submit a
/// transaction otherwise byte-identical to one already
/// processed (a repeat claim, a repeat cancel, a boundary
/// retry) needs this: the bank returns the cached result for
/// a duplicate signature WITHOUT re-executing the program, so
/// the repeat attempt would test the cache, not the code.
/// Changing the payer changes the signature and forces real
/// execution.
async fn send_with_payer(
    ctx: &mut ProgramTestContext,
    ixs: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<(), BanksClientError> {
    let mut all: Vec<&Keypair> = vec![payer];
    for s in signers {
        if s.pubkey() != payer.pubkey() {
            all.push(s);
        }
    }
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&payer.pubkey()),
        &all,
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

/// A Claim resubmitted by its own user as fee payer — see
/// `send_with_payer` for why the repeat needs a new signature.
async fn claim_again(
    ctx: &mut ProgramTestContext,
    user: &Keypair,
    market: &Pubkey,
    user_token: &Pubkey,
) -> Result<(), BanksClientError> {
    send_with_payer(
        ctx,
        &[claim_ix(&user.pubkey(), market, user_token)],
        user,
        &[user],
    )
    .await
}

/// A CancelMarket resubmitted by its caller as fee payer.
async fn cancel_again(
    ctx: &mut ProgramTestContext,
    caller: &Keypair,
    market: &Pubkey,
    subject_mint: &Pubkey,
    creator: &Pubkey,
) -> Result<(), BanksClientError> {
    send_with_payer(
        ctx,
        &[cancel_ix(&caller.pubkey(), market, subject_mint, creator)],
        caller,
        &[caller],
    )
    .await
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

fn expect_invalid_account_data(err: &BanksClientError) {
    expect_instruction_error(err, InstructionError::InvalidAccountData);
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
// Day-3 helpers: terminal-state assertions and shared setups.
// ---------------------------------------------------------------------------

async fn account_lamports(ctx: &mut ProgramTestContext, addr: &Pubkey) -> u64 {
    ctx.banks_client
        .get_account(*addr)
        .await
        .unwrap()
        .map(|a| a.lamports)
        .unwrap_or(0)
}

/// The market account must be byte-identical to the snapshot:
/// rejections mutate nothing, and Claim never writes the market
/// at all (invariant M-1).
async fn assert_market_eq(ctx: &mut ProgramTestContext, market: &Pubkey, snapshot: &Market) {
    let now = read_market(ctx, market).await.expect("market exists");
    assert_eq!(&now, snapshot, "market account mutated unexpectedly");
}

/// The LiveMarket lock must be gone after a terminal
/// transition: closed means zero lamports, system owner, zero
/// data — the runtime reclaims it, and the read helper maps
/// both "no account" and "undecodable account" to None.
async fn assert_live_closed(ctx: &mut ProgramTestContext, subject: &Pubkey) {
    assert!(
        read_live_market(ctx, subject).await.is_none(),
        "live lock should be closed"
    );
}

/// The standing assertions for every successful Resolve/Cancel:
/// the lock is closed and its rent landed on the market
/// creator, exactly. Callers capture `live_lamports_before`
/// and `creator_lamports_before` immediately before the
/// terminal transaction.
async fn assert_lock_closed_with_rent(
    ctx: &mut ProgramTestContext,
    subject: &Pubkey,
    creator: &Pubkey,
    live_lamports_before: u64,
    creator_lamports_before: u64,
) {
    assert_live_closed(ctx, subject).await;
    assert_eq!(
        account_lamports(ctx, creator).await,
        creator_lamports_before + live_lamports_before,
        "lock rent must land on the market creator, exactly"
    );
}

/// Conservation and solvency after claims (invariants M-1/M-2).
/// `positions` lists every position ever created on the market
/// as (flagged, survives, claimed). Asserts: the stored pools
/// still equal the per-side sums of the positions; everything
/// paid out plus what the vault still holds equals the vault's
/// initial balance (these tests make no stray transfers; the
/// one deliberate exception passes its post-donation balance
/// as `initial_vault`); and the vault covers the obligations
/// of every unclaimed position, computed from the stored
/// pools and the terminal state alone — the same numbers the
/// program itself will use, so the invariant is claim-order
/// independent.
async fn assert_conservation(
    ctx: &mut ProgramTestContext,
    market_addr: &Pubkey,
    positions: &[(u64, u64, bool)],
    initial_vault: u64,
    total_paid: u64,
) {
    let m = read_market(ctx, market_addr).await.expect("market exists");
    let sum_flagged: u64 = positions.iter().map(|p| p.0).sum();
    let sum_survives: u64 = positions.iter().map(|p| p.1).sum();
    assert_eq!(m.flagged_pool, sum_flagged, "stored flagged pool drifted");
    assert_eq!(m.survives_pool, sum_survives, "stored survives pool drifted");
    let vault = token_balance(ctx, &vault_pda(market_addr)).await;
    assert_eq!(
        total_paid + vault,
        initial_vault,
        "paid out + vault must equal the initial vault"
    );
    let total_pool = m.flagged_pool + m.survives_pool;
    let mut obligations: u64 = 0;
    for &(flagged, survives, claimed) in positions {
        if claimed {
            continue;
        }
        let eventual = if m.state == STATE_CANCELLED {
            flagged + survives
        } else {
            // Resolved. A zero winning pool refunds in full;
            // otherwise a position's obligation is its pro-rata
            // payout, or zero if it holds no winning stake.
            let (winning_pool, winning_stake) = if m.outcome == OUTCOME_FLAGGED {
                (m.flagged_pool, flagged)
            } else {
                (m.survives_pool, survives)
            };
            if winning_pool == 0 {
                flagged + survives
            } else if winning_stake == 0 {
                0
            } else {
                parimutuel_payout(winning_stake, total_pool, winning_pool)
                    .expect("payout exists on program-written state")
            }
        };
        obligations += eventual;
    }
    assert!(
        vault >= obligations,
        "vault {vault} must cover unclaimed obligations {obligations}"
    );
}

/// The standard Day-3 market: created on `subject` at T0 by a
/// fresh creator, with the asymmetric STD3 stakes — Alice 1
/// dUSDC Flagged, Bob 2 dUSDC Flagged, Carol 7 dUSDC Survives
/// (pools 3M / 7M, vault 10M). Returns the creator, the market
/// address, and the three (user, token) pairs.
async fn setup_std3(
    ctx: &mut ProgramTestContext,
    mint: &Pubkey,
    subject: &Pubkey,
) -> (
    Keypair,
    Pubkey,
    (Keypair, Pubkey),
    (Keypair, Pubkey),
    (Keypair, Pubkey),
) {
    let creator = make_creator(ctx).await;
    let (alice, alice_token) = make_user(ctx, mint, 20_000_000).await;
    let (bob, bob_token) = make_user(ctx, mint, 20_000_000).await;
    let (carol, carol_token) = make_user(ctx, mint, 20_000_000).await;
    set_clock(ctx, T0).await;
    let market = create_market(ctx, &creator, subject, T0, mint)
        .await
        .expect("create market");
    buy(ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("alice buys flagged");
    buy(ctx, &bob, &bob_token, &market, SIDE_FLAGGED, 2_000_000)
        .await
        .expect("bob buys flagged");
    buy(ctx, &carol, &carol_token, &market, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("carol buys survives");
    assert_accounting(ctx, &market, 3_000_000, 7_000_000).await;
    (creator, market, (alice, alice_token), (bob, bob_token), (carol, carol_token))
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
// Full lifecycle — all six instructions in one deployment.
// (This replaces the Day-2 pin that the three terminal
// handlers still returned NotImplemented: the premise is gone,
// and the lifecycle below is the smoke-level proof that no
// handler still does, and that the two terminal paths compose.)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_lifecycle_all_six_instructions() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (carol, carol_token) = make_user(&mut ctx, &mint, 20_000_000).await;

    // Market A: create, STD3 buys, resolve Flagged, claims.
    set_clock(&mut ctx, T0).await;
    let market_a = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market A");
    buy(&mut ctx, &alice, &alice_token, &market_a, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("alice buys A flagged");
    buy(&mut ctx, &bob, &bob_token, &market_a, SIDE_FLAGGED, 2_000_000)
        .await
        .expect("bob buys A flagged");
    buy(&mut ctx, &carol, &carol_token, &market_a, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("carol buys A survives");
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market_a,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve A flagged");
    claim(&mut ctx, &alice, &market_a, &alice_token)
        .await
        .expect("alice claims A");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 22_333_333);
    let err = claim(&mut ctx, &carol, &market_a, &carol_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::NoClaimableStake as u32); // 19

    // Market B, same clock: create, two buys, resolver cancels,
    // exact refunds, and the shared claimed flag refuses a
    // second helping on the refund path too.
    let market_b = create_market(&mut ctx, &creator, &subject_b(), T0 + 86_400, &mint)
        .await
        .expect("create market B");
    buy(&mut ctx, &bob, &bob_token, &market_b, SIDE_SURVIVES, 2_000_000)
        .await
        .expect("bob buys B survives");
    buy(&mut ctx, &alice, &alice_token, &market_b, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("alice buys B flagged");
    cancel(
        &mut ctx,
        &resolver,
        &market_b,
        &subject_b(),
        &creator.pubkey(),
    )
    .await
    .expect("resolver cancels B");
    claim(&mut ctx, &bob, &market_b, &bob_token)
        .await
        .expect("bob refunds B");
    claim(&mut ctx, &alice, &market_b, &alice_token)
        .await
        .expect("alice refunds B");
    // Bob: 20M - 2M (A flagged) - 2M (B survives) + 2M refund.
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 18_000_000);
    // Alice: 22_333_333 - 1M (B flagged) + 1M refund.
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 22_333_333);
    let err = claim_again(&mut ctx, &bob, &market_b, &bob_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::AlreadyClaimed as u32); // 18
}

// ---------------------------------------------------------------------------
// ResolveMarket — happy paths.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolve_flagged_early_at_creation_second() {
    // Early resolution is the design's live path: a hard
    // trigger fires mid-window and the resolver posts at once —
    // here, in the very second the market was created.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let live_before = account_lamports(&mut ctx, &live_pda(&subject_a())).await;
    let creator_before = account_lamports(&mut ctx, &creator.pubkey()).await;
    // Clock is still T0 from setup.
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("early flagged resolve");
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_RESOLVED);
    assert_eq!(m.outcome, OUTCOME_FLAGGED);
    assert_eq!(m.hard_trigger, 3);
    assert_eq!(m.evidence_hash, [9u8; 32]);
    assert_eq!(m.resolved_at, T0);
    assert_eq!((m.flagged_pool, m.survives_pool), (3_000_000, 7_000_000));
    assert_lock_closed_with_rent(
        &mut ctx,
        &subject_a(),
        &creator.pubkey(),
        live_before,
        creator_before,
    )
    .await;
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

#[tokio::test]
async fn resolve_flagged_at_expiry_minus_one_trigger_min() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_399).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        1,
        [9u8; 32],
    )
    .await
    .expect("flagged resolve one second before expiry");
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_RESOLVED);
    assert_eq!(m.hard_trigger, 1);
    assert_eq!(m.resolved_at, T0 + 86_399);
    assert_live_closed(&mut ctx, &subject_a()).await;
}

#[tokio::test]
async fn resolve_flagged_at_expiry_trigger_max() {
    // At now == expiry the resolution path wins the tie against
    // buys — for Resolve itself both outcomes are legal here.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        5,
        [9u8; 32],
    )
    .await
    .expect("flagged resolve at expiry");
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_RESOLVED);
    assert_eq!(m.hard_trigger, 5);
    assert_eq!(m.resolved_at, T0 + 86_400);
}

#[tokio::test]
async fn resolve_flagged_after_cancel_grace_still_succeeds() {
    // The anyone-cancel path is an option, not a resolver
    // deadline: if nobody cancelled, the resolver may still
    // resolve after the grace window has fully run.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 172_801).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("flagged resolve after the grace window");
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_RESOLVED);
    assert_eq!(m.resolved_at, T0 + 172_801);
}

#[tokio::test]
async fn resolve_survives_at_expiry_exact() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let live_before = account_lamports(&mut ctx, &live_pda(&subject_a())).await;
    let creator_before = account_lamports(&mut ctx, &creator.pubkey()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [7u8; 32],
    )
    .await
    .expect("survives resolve at expiry");
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_RESOLVED);
    assert_eq!(m.outcome, OUTCOME_SURVIVES);
    assert_eq!(m.hard_trigger, 0);
    assert_eq!(m.evidence_hash, [7u8; 32]);
    assert_eq!(m.resolved_at, T0 + 86_400);
    assert_eq!((m.flagged_pool, m.survives_pool), (3_000_000, 7_000_000));
    assert_lock_closed_with_rent(
        &mut ctx,
        &subject_a(),
        &creator.pubkey(),
        live_before,
        creator_before,
    )
    .await;
}

#[tokio::test]
async fn resolve_survives_after_expiry() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 90_000).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [9u8; 32],
    )
    .await
    .expect("survives resolve after expiry");
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_RESOLVED);
    assert_eq!(m.outcome, OUTCOME_SURVIVES);
    assert_eq!(m.resolved_at, T0 + 90_000);
}

#[tokio::test]
async fn resolve_flagged_empty_market() {
    // A market nobody bet on still resolves: pools 0/0, the
    // lock closes, and the vault stays empty.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        1,
        [9u8; 32],
    )
    .await
    .expect("resolve empty market");
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_RESOLVED);
    assert_eq!((m.flagged_pool, m.survives_pool), (0, 0));
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 0);
    assert_live_closed(&mut ctx, &subject_a()).await;
}

// ---------------------------------------------------------------------------
// ResolveMarket — rejections. Each is single-fault: everything
// else about the call is valid, so the named error is the only
// possible answer. Every rejection asserts the market is
// byte-identical afterwards and the lock is still live.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolve_by_non_resolver_rejected() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    // The creator signs instead of the resolver.
    let err = send(
        &mut ctx,
        &[resolve_ix(
            &creator.pubkey(),
            &market,
            &subject_a(),
            &creator.pubkey(),
            OUTCOME_FLAGGED,
            3,
            [9u8; 32],
        )],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::NotResolver as u32); // 12
    assert_market_eq(&mut ctx, &market, &before).await;
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_some());
}

#[tokio::test]
async fn resolve_on_resolved_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("first resolve");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [7u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::AlreadyResolved as u32); // 13
    assert_market_eq(&mut ctx, &market, &snapshot).await;
}

#[tokio::test]
async fn resolve_on_cancelled_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("resolver cancels");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::AlreadyCancelled as u32); // 14
    assert_market_eq(&mut ctx, &market, &snapshot).await;
}

#[tokio::test]
async fn resolve_outcome_zero_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        0,
        0,
        [9u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::InvalidSide as u32); // 8
    assert_market_eq(&mut ctx, &market, &before).await;
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_some());
}

#[tokio::test]
async fn resolve_outcome_three_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        3,
        3,
        [9u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::InvalidSide as u32); // 8
    assert_market_eq(&mut ctx, &market, &before).await;
}

#[tokio::test]
async fn resolve_flagged_trigger_zero_rejected() {
    // A Flagged resolution must name its trigger: "flagged, no
    // reason" is not an acceptable resolution.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        0,
        [9u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::InvalidHardTrigger as u32); // 15
    assert_market_eq(&mut ctx, &market, &before).await;
}

#[tokio::test]
async fn resolve_flagged_trigger_six_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        6,
        [9u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::InvalidHardTrigger as u32); // 15
    assert_market_eq(&mut ctx, &market, &before).await;
}

#[tokio::test]
async fn resolve_survives_with_trigger_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        3,
        [9u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::InvalidHardTrigger as u32); // 15
    assert_market_eq(&mut ctx, &market, &before).await;
}

#[tokio::test]
async fn resolve_flagged_zero_evidence_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [0u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::InvalidEvidenceHash as u32); // 23
    assert_market_eq(&mut ctx, &market, &before).await;
}

#[tokio::test]
async fn resolve_survives_zero_evidence_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [0u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::InvalidEvidenceHash as u32); // 23
    assert_market_eq(&mut ctx, &market, &before).await;
}

#[tokio::test]
async fn resolve_survives_one_second_early_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_399).await;
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [9u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::EarlySurvivesResolution as u32); // 16
    assert_market_eq(&mut ctx, &market, &before).await;
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_some());
    // The rejection was the clock/outcome pair, not a wedged
    // market: a Flagged resolve at the same second succeeds.
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        2,
        [9u8; 32],
    )
    .await
    .expect("flagged resolve at the same second");
}

#[tokio::test]
async fn resolve_by_non_resolver_on_resolved_market_names_authority() {
    // Precedence pin: the resolver check runs before the market
    // is even loaded, so a stranger resolving a Resolved market
    // hears NotResolver, not AlreadyResolved — a non-resolver
    // cannot probe market state through this instruction.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("first resolve");
    let err = send(
        &mut ctx,
        &[resolve_ix(
            &creator.pubkey(),
            &market,
            &subject_a(),
            &creator.pubkey(),
            OUTCOME_FLAGGED,
            3,
            [9u8; 32],
        )],
        &[&creator],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::NotResolver as u32); // 12
}

#[tokio::test]
async fn resolve_bad_outcome_on_resolved_market_names_payload() {
    // Precedence pin: payload validation precedes the market
    // load and the state check (the BuyPosition precedent), so
    // a malformed resolution on a Resolved market is refused as
    // malformed — InvalidSide, not AlreadyResolved.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("first resolve");
    let err = resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        0,
        0,
        [9u8; 32],
    )
    .await
    .unwrap_err();
    expect_custom(&err, RugCallError::InvalidSide as u32); // 8
}

// ---------------------------------------------------------------------------
// Claim — payouts. The conservation invariant is asserted
// after every claim: paid + vault == initial vault, and the
// vault always covers the unclaimed obligations computed from
// the stored pools (invariants M-1/M-2).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_conservation_sequence_with_dust() {
    // The flagship sequence, with exact starting balances.
    // Pools F=3M (Alice 1M, Bob 2M) / S=7M (Carol); resolve
    // Flagged at expiry. Alice's payout is
    // floor(1M x 10M / 3M) = 3_333_333, Bob's is 6_666_666,
    // and exactly 1 raw unit of floor dust stays in the vault.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 10_000_000).await;
    let (carol, carol_token) = make_user(&mut ctx, &mint, 10_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("alice buys");
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_FLAGGED, 2_000_000)
        .await
        .expect("bob buys");
    buy(&mut ctx, &carol, &carol_token, &market, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("carol buys");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 4_000_000);
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 8_000_000);
    assert_eq!(token_balance(&mut ctx, &carol_token).await, 3_000_000);

    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve flagged");
    let positions = [
        (1_000_000, 0, false),
        (2_000_000, 0, false),
        (0, 7_000_000, false),
    ];
    assert_conservation(&mut ctx, &market, &positions, 10_000_000, 0).await;
    let snapshot = read_market(&mut ctx, &market).await.unwrap();

    // Alice claims first.
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("alice claims");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 7_333_333);
    let positions = [
        (1_000_000, 0, true),
        (2_000_000, 0, false),
        (0, 7_000_000, false),
    ];
    assert_conservation(&mut ctx, &market, &positions, 10_000_000, 3_333_333).await;
    assert_market_eq(&mut ctx, &market, &snapshot).await;
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert!(pos.claimed);
    assert_eq!((pos.flagged_amount, pos.survives_amount), (1_000_000, 0));

    // Bob claims second; the payout is identical to what he
    // would have received first (M-1: claim order cannot
    // matter, because the pools never move).
    claim(&mut ctx, &bob, &market, &bob_token)
        .await
        .expect("bob claims");
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 14_666_666);
    let positions = [
        (1_000_000, 0, true),
        (2_000_000, 0, true),
        (0, 7_000_000, false),
    ];
    assert_conservation(&mut ctx, &market, &positions, 10_000_000, 9_999_999).await;
    assert_market_eq(&mut ctx, &market, &snapshot).await;

    // Carol held only the losing side: no claim, no flag flip,
    // no movement — and the final vault is exactly the dust.
    let err = claim(&mut ctx, &carol, &market, &carol_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::NoClaimableStake as u32); // 19
    assert_eq!(token_balance(&mut ctx, &carol_token).await, 3_000_000);
    let pos = read_position(&mut ctx, &market, &carol.pubkey())
        .await
        .unwrap();
    assert!(!pos.claimed);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 1);
}

#[tokio::test]
async fn claim_survives_split_two_winners() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (carol, carol_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (dave, dave_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("alice buys");
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_FLAGGED, 2_000_000)
        .await
        .expect("bob buys");
    buy(&mut ctx, &carol, &carol_token, &market, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("carol buys");
    buy(&mut ctx, &dave, &dave_token, &market, SIDE_SURVIVES, 1_000_000)
        .await
        .expect("dave buys");
    // Pools F=3M / S=8M, total 11M. Resolve Survives.
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [9u8; 32],
    )
    .await
    .expect("resolve survives");
    // Carol: floor(7M x 11M / 8M) = 9_625_000.
    claim(&mut ctx, &carol, &market, &carol_token)
        .await
        .expect("carol claims");
    assert_eq!(token_balance(&mut ctx, &carol_token).await, 22_625_000);
    let positions = [
        (1_000_000, 0, false),
        (2_000_000, 0, false),
        (0, 7_000_000, true),
        (0, 1_000_000, false),
    ];
    assert_conservation(&mut ctx, &market, &positions, 11_000_000, 9_625_000).await;
    // Dave: floor(1M x 11M / 8M) = 1_375_000 — an exact split,
    // no dust this time.
    claim(&mut ctx, &dave, &market, &dave_token)
        .await
        .expect("dave claims");
    assert_eq!(token_balance(&mut ctx, &dave_token).await, 20_375_000);
    let positions = [
        (1_000_000, 0, false),
        (2_000_000, 0, false),
        (0, 7_000_000, true),
        (0, 1_000_000, true),
    ];
    assert_conservation(&mut ctx, &market, &positions, 11_000_000, 11_000_000).await;
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 0);
}

#[tokio::test]
async fn claim_hedged_winner_takes_whole_pool() {
    // Bob holds both sides. His winning Survives stake buys a
    // share of a pool his own losing Flagged stake enlarged:
    // floor(7M x 13M / 7M) = the whole 13M pool, and the losing
    // 1M is NOT refunded on top — one claim settles the whole
    // position.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 5_000_000)
        .await
        .expect("alice buys");
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("bob buys flagged");
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("bob buys survives");
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [9u8; 32],
    )
    .await
    .expect("resolve survives");
    let bob_before = token_balance(&mut ctx, &bob_token).await;
    claim(&mut ctx, &bob, &market, &bob_token)
        .await
        .expect("bob claims");
    assert_eq!(
        token_balance(&mut ctx, &bob_token).await - bob_before,
        13_000_000
    );
    let pos = read_position(&mut ctx, &market, &bob.pubkey()).await.unwrap();
    assert!(pos.claimed);
    assert_eq!((pos.flagged_amount, pos.survives_amount), (1_000_000, 7_000_000));
    let err = claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::NoClaimableStake as u32); // 19
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 0);
}

#[tokio::test]
async fn claim_lone_winner_takes_whole_pool() {
    // One Flagged holder against two Survives holders; an
    // early Flagged resolution (trigger H4) makes the lone
    // winner the whole pool's only claimant.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (eve, eve_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (frank, frank_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (grace, grace_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(&mut ctx, &eve, &eve_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("eve buys flagged");
    buy(&mut ctx, &frank, &frank_token, &market, SIDE_SURVIVES, 2_000_000)
        .await
        .expect("frank buys survives");
    buy(&mut ctx, &grace, &grace_token, &market, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("grace buys survives");
    set_clock(&mut ctx, T0 + 5).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        4,
        [9u8; 32],
    )
    .await
    .expect("early flagged resolve");
    let eve_before = token_balance(&mut ctx, &eve_token).await;
    claim(&mut ctx, &eve, &market, &eve_token)
        .await
        .expect("eve claims");
    assert_eq!(
        token_balance(&mut ctx, &eve_token).await - eve_before,
        10_000_000
    );
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 0);
}

#[tokio::test]
async fn claim_reverse_order_same_payouts() {
    // P1's order reversed: Bob (the larger winner) claims
    // first. M-1 says the amounts cannot depend on the order.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, (alice, alice_token), (bob, bob_token), _c) =
        setup_std3(&mut ctx, &mint, &subject_b()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_b(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve flagged");
    claim(&mut ctx, &bob, &market, &bob_token)
        .await
        .expect("bob claims first");
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 24_666_666);
    let positions = [
        (1_000_000, 0, false),
        (2_000_000, 0, true),
        (0, 7_000_000, false),
    ];
    assert_conservation(&mut ctx, &market, &positions, 10_000_000, 6_666_666).await;
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("alice claims second");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 22_333_333);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 1);
}

#[tokio::test]
async fn claim_same_second_as_resolve() {
    // No claim timelock exists: a position resolved early can
    // be claimed in the same wall-clock second.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, (alice, alice_token), _b, _c) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    // Clock still T0: early Flagged resolve, then claim.
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve at T0");
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("claim at T0");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 22_333_333);
}

#[tokio::test]
async fn claim_long_after_resolution() {
    // No claim deadline exists either: funds are never trapped
    // by the passage of time after resolution.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, (carol, carol_token)) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [9u8; 32],
    )
    .await
    .expect("resolve survives");
    set_clock(&mut ctx, T0 + 86_400 + 31_536_000).await;
    claim(&mut ctx, &carol, &market, &carol_token)
        .await
        .expect("claim a year later");
    // Carol is the lone winning side: floor(7M x 10M / 7M).
    assert_eq!(token_balance(&mut ctx, &carol_token).await, 23_000_000);
}

#[tokio::test]
async fn claim_ignores_stray_vault_donation() {
    // Anyone can transfer tokens into a vault; the accounting
    // must not notice. Payouts come from the stored pools
    // only, and the donation strands in the vault next to the
    // floor dust (Day-1 invariant 4).
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, (alice, alice_token), (bob, bob_token), _c) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    let (donor, donor_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    let donate = spl_token::instruction::transfer(
        &spl_token::id(),
        &donor_token,
        &vault_pda(&market),
        &donor.pubkey(),
        &[],
        500_000,
    )
    .unwrap();
    send(&mut ctx, &[donate], &[&donor]).await.expect("donate");
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_500_000);
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!((m.flagged_pool, m.survives_pool), (3_000_000, 7_000_000));
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve flagged");
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("alice claims");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 22_333_333);
    let positions = [
        (1_000_000, 0, true),
        (2_000_000, 0, false),
        (0, 7_000_000, false),
    ];
    assert_conservation(&mut ctx, &market, &positions, 10_500_000, 3_333_333).await;
    claim(&mut ctx, &bob, &market, &bob_token)
        .await
        .expect("bob claims");
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 24_666_666);
    // Donation (500_000) + dust (1), both stranded by design.
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 500_001);
}

// ---------------------------------------------------------------------------
// Claim — refunds: cancelled markets, the zero-winning-pool
// branch, and one-sided markets. Refunds return exact stakes,
// summed across both sides of a hedged position.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_cancelled_refunds_both_sides_hedged() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (carol, carol_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 3_000_000)
        .await
        .expect("alice buys flagged");
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("bob buys survives");
    buy(&mut ctx, &carol, &carol_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("carol buys flagged");
    buy(&mut ctx, &carol, &carol_token, &market, SIDE_SURVIVES, 2_000_000)
        .await
        .expect("carol buys survives");
    // Pools F=4M / S=9M, total 13M. Resolver cancels mid-window.
    set_clock(&mut ctx, T0 + 100).await;
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("resolver cancels");
    // Carol first: her refund is both sides summed (1M + 2M).
    claim(&mut ctx, &carol, &market, &carol_token)
        .await
        .expect("carol refunds");
    assert_eq!(token_balance(&mut ctx, &carol_token).await, 20_000_000);
    let positions = [
        (3_000_000, 0, false),
        (0, 7_000_000, false),
        (1_000_000, 2_000_000, true),
    ];
    assert_conservation(&mut ctx, &market, &positions, 13_000_000, 3_000_000).await;
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("alice refunds");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 20_000_000);
    let positions = [
        (3_000_000, 0, true),
        (0, 7_000_000, false),
        (1_000_000, 2_000_000, true),
    ];
    assert_conservation(&mut ctx, &market, &positions, 13_000_000, 6_000_000).await;
    claim(&mut ctx, &bob, &market, &bob_token)
        .await
        .expect("bob refunds");
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 20_000_000);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 0);
}

#[tokio::test]
async fn claim_zero_winning_pool_refunds_not_no_claimable() {
    // Everyone backed Survives; the resolver posts Flagged.
    // The winning pool is zero, so the frozen precedence
    // refunds in full — the NoClaimableStake branch must NOT
    // fire, or the entire pool would strand behind claims
    // nobody can ever make.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_SURVIVES, 3_000_000)
        .await
        .expect("alice buys survives");
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("bob buys survives");
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        2,
        [9u8; 32],
    )
    .await
    .expect("resolve flagged against an empty flagged pool");
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("alice refunds in full");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 20_000_000);
    let positions = [(0, 3_000_000, true), (0, 7_000_000, false)];
    assert_conservation(&mut ctx, &market, &positions, 10_000_000, 3_000_000).await;
    claim(&mut ctx, &bob, &market, &bob_token)
        .await
        .expect("bob refunds in full");
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 20_000_000);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 0);
}

#[tokio::test]
async fn claim_one_sided_all_on_winning_flagged() {
    // Winning pool == total pool: the pro-rata formula
    // degenerates to the exact stake. No windfall is invented
    // and none is withheld.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_FLAGGED, 3_000_000)
        .await
        .expect("alice buys");
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_FLAGGED, 7_000_000)
        .await
        .expect("bob buys");
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        1,
        [9u8; 32],
    )
    .await
    .expect("resolve flagged");
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("alice claims");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 20_000_000);
    claim(&mut ctx, &bob, &market, &bob_token)
        .await
        .expect("bob claims");
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 20_000_000);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 0);
}

#[tokio::test]
async fn claim_one_sided_all_on_winning_survives() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let (alice, alice_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    let (bob, bob_token) = make_user(&mut ctx, &mint, 20_000_000).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    buy(&mut ctx, &alice, &alice_token, &market, SIDE_SURVIVES, 3_000_000)
        .await
        .expect("alice buys");
    buy(&mut ctx, &bob, &bob_token, &market, SIDE_SURVIVES, 7_000_000)
        .await
        .expect("bob buys");
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [9u8; 32],
    )
    .await
    .expect("resolve survives");
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("alice claims");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 20_000_000);
    claim(&mut ctx, &bob, &market, &bob_token)
        .await
        .expect("bob claims");
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 20_000_000);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 0);
}

#[tokio::test]
async fn claim_empty_resolved_market_fails_cleanly() {
    // Invariant M-6: a claim against a market with no
    // positions fails cleanly — the missing Position account
    // is invalid account data (the load convention), NOT
    // NoClaimableStake, which is reserved for a position that
    // exists but has nothing to collect.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        1,
        [9u8; 32],
    )
    .await
    .expect("resolve empty market");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    let (dave, dave_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    let err = claim(&mut ctx, &dave, &market, &dave_token)
        .await
        .unwrap_err();
    expect_invalid_account_data(&err);
    assert!(read_position(&mut ctx, &market, &dave.pubkey())
        .await
        .is_none());
    assert_market_eq(&mut ctx, &market, &snapshot).await;
    assert_eq!(token_balance(&mut ctx, &dave_token).await, 5_000_000);
}

#[tokio::test]
async fn claim_empty_cancelled_market_fails_cleanly() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("resolver cancels");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    let (dave, dave_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    let err = claim(&mut ctx, &dave, &market, &dave_token)
        .await
        .unwrap_err();
    expect_invalid_account_data(&err);
    assert!(read_position(&mut ctx, &market, &dave.pubkey())
        .await
        .is_none());
    assert_market_eq(&mut ctx, &market, &snapshot).await;
}

/// Start a harness whose Config, Market (already Cancelled)
/// and Position (zero stake) accounts are seeded directly.
/// A zero-stake position can never be produced by BuyPosition
/// (the min-stake check forbids it), so this state is only
/// reachable as a fixture — but the refund branches' zero
/// check must still be pinned, because the error enum's own
/// doc promises NoClaimableStake for it.
async fn start_with_cancelled_market_zero_position(
) -> (ProgramTestContext, Keypair, Pubkey, Pubkey) {
    let authority = Keypair::new();
    let mint_keypair = Keypair::new();
    let user = Keypair::new();
    let creator = Keypair::new();
    let market_addr = market_pda(&subject_a(), T0);
    let mut test = ProgramTest::new(
        "rug_call",
        program_id(),
        processor!(rug_call::process_instruction),
    );
    seed_programdata(&mut test, &authority.pubkey());
    let fixture_config = RugCallConfig {
        discriminator: DISCRIMINATOR_CONFIG,
        admin: authority.pubkey(),
        resolver: Keypair::new().pubkey(),
        collateral_mint: mint_keypair.pubkey(),
        fee_bps: FEE_BPS,
        default_duration_secs: DEFAULT_MARKET_DURATION_SECS,
        min_stake: MIN_STAKE_RAW,
        bump: pda(&[SEED_CONFIG]).1,
    };
    test.add_account(
        config_pda(),
        Account {
            lamports: 10_000_000,
            data: borsh::to_vec(&fixture_config).unwrap(),
            owner: program_id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let fixture_market = Market {
        discriminator: DISCRIMINATOR_MARKET,
        subject_mint: subject_a(),
        creator: creator.pubkey(),
        created_at: T0,
        expiry: T0 + DEFAULT_MARKET_DURATION_SECS,
        flagged_pool: 0,
        survives_pool: 0,
        state: STATE_CANCELLED,
        outcome: OUTCOME_NONE,
        evidence_hash: [0u8; 32],
        hard_trigger: HARD_TRIGGER_NONE,
        resolved_at: 0,
        bump: pda(&[SEED_MARKET, subject_a().as_ref(), &T0.to_le_bytes()]).1,
        vault_bump: pda(&[SEED_VAULT, market_addr.as_ref()]).1,
    };
    test.add_account(
        market_addr,
        Account {
            lamports: 10_000_000,
            data: borsh::to_vec(&fixture_market).unwrap(),
            owner: program_id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let fixture_position = Position {
        discriminator: DISCRIMINATOR_POSITION,
        market: market_addr,
        user: user.pubkey(),
        flagged_amount: 0,
        survives_amount: 0,
        claimed: false,
        bump: pda(&[SEED_POSITION, market_addr.as_ref(), user.pubkey().as_ref()]).1,
    };
    test.add_account(
        position_pda(&market_addr, &user.pubkey()),
        Account {
            lamports: 10_000_000,
            data: borsh::to_vec(&fixture_position).unwrap(),
            owner: program_id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    // The vault as a raw SPL token account: mint = the fixture
    // collateral mint, owner = the market PDA, amount 0.
    let mut vault_state = spl_token::state::Account::default();
    vault_state.mint = mint_keypair.pubkey();
    vault_state.owner = market_addr;
    vault_state.amount = 0;
    vault_state.state = spl_token::state::AccountState::Initialized;
    let mut vault_data = vec![0u8; spl_token::state::Account::LEN];
    solana_program::program_pack::Pack::pack_into_slice(&vault_state, &mut vault_data);
    test.add_account(
        vault_pda(&market_addr),
        Account {
            lamports: 10_000_000,
            data: vault_data,
            owner: spl_token::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = test.start_with_context().await;
    make_mint(&mut ctx, &mint_keypair, 6).await;
    let user_token = make_user_with(&mut ctx, &user, &mint_keypair.pubkey(), 5_000_000).await;
    (ctx, user, market_addr, user_token)
}

#[tokio::test]
async fn claim_zero_stake_position_on_cancelled_market_rejected() {
    // The other half of the missing-vs-empty distinction: the
    // position EXISTS but holds nothing, so the refund branch
    // refuses with NoClaimableStake — and does not set the
    // claimed flag on the way out.
    let (mut ctx, user, market, user_token) = start_with_cancelled_market_zero_position().await;
    let err = claim(&mut ctx, &user, &market, &user_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::NoClaimableStake as u32); // 19
    let pos = read_position(&mut ctx, &market, &user.pubkey())
        .await
        .unwrap();
    assert!(!pos.claimed);
    assert_eq!(token_balance(&mut ctx, &user_token).await, 5_000_000);
}

// ---------------------------------------------------------------------------
// Claim — rejections.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_on_open_market_rejected() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let (_creator, market, (alice, alice_token), _b, _c) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    let err = claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::ClaimBeforeResolve as u32); // 17
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert!(!pos.claimed);
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 19_000_000);
}

#[tokio::test]
async fn claim_on_open_market_after_expiry_rejected() {
    // Expiry alone does not open Claim: only a terminal state
    // does. An expired-but-unresolved market is in its grace
    // window, not in payout.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let (_creator, market, (alice, alice_token), _b, _c) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_500).await;
    let err = claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::ClaimBeforeResolve as u32); // 17
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

#[tokio::test]
async fn claim_twice_winner_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, (alice, alice_token), _b, _c) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve flagged");
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("first claim");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 22_333_333);
    // The repeat goes through claim_again (user as payer): a
    // byte-identical resubmission would return the first
    // transaction's cached success without re-executing.
    let err = claim_again(&mut ctx, &alice, &market, &alice_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::AlreadyClaimed as u32); // 18
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 22_333_333);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 6_666_667);
}

#[tokio::test]
async fn claim_twice_refund_rejected() {
    // The payout and refund paths share the one claimed flag:
    // a refunded position cannot refund again.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, (bob, bob_token), _c) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("resolver cancels");
    claim(&mut ctx, &bob, &market, &bob_token)
        .await
        .expect("first refund");
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 20_000_000);
    let err = claim_again(&mut ctx, &bob, &market, &bob_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::AlreadyClaimed as u32); // 18
    assert_eq!(token_balance(&mut ctx, &bob_token).await, 20_000_000);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 8_000_000);
}

#[tokio::test]
async fn claim_losing_only_flagged_resolution_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, (carol, carol_token)) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve flagged");
    let err = claim(&mut ctx, &carol, &market, &carol_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::NoClaimableStake as u32); // 19
    // A failed claim sets nothing: her flag stays false, her
    // balance and the vault are untouched.
    let pos = read_position(&mut ctx, &market, &carol.pubkey())
        .await
        .unwrap();
    assert!(!pos.claimed);
    assert_eq!(token_balance(&mut ctx, &carol_token).await, 13_000_000);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

#[tokio::test]
async fn claim_losing_only_survives_resolution_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, (alice, alice_token), _b, _c) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_SURVIVES,
        0,
        [9u8; 32],
    )
    .await
    .expect("resolve survives");
    let err = claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::NoClaimableStake as u32); // 19
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert!(!pos.claimed);
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 19_000_000);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

#[tokio::test]
async fn claim_without_position_on_resolved_market_rejected() {
    // "No position exists" (invalid account data) is a
    // different answer from "position exists but lost"
    // (NoClaimableStake) — the two must not blur.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve flagged");
    let (dave, dave_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    let err = claim(&mut ctx, &dave, &market, &dave_token)
        .await
        .unwrap_err();
    expect_invalid_account_data(&err);
    assert!(read_position(&mut ctx, &market, &dave.pubkey())
        .await
        .is_none());
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

// ---------------------------------------------------------------------------
// CancelMarket — paths and boundaries. Every success asserts
// the full cancelled shape: state Cancelled, outcome NONE,
// hash zero, trigger 0, resolved_at 0, pools frozen — plus
// the standing lock/rent assertions.
// ---------------------------------------------------------------------------

async fn assert_cancelled_shape(
    ctx: &mut ProgramTestContext,
    market: &Pubkey,
    flagged: u64,
    survives: u64,
) {
    let m = read_market(ctx, market).await.unwrap();
    assert_eq!(m.state, STATE_CANCELLED);
    assert_eq!(m.outcome, OUTCOME_NONE);
    assert_eq!(m.evidence_hash, [0u8; 32]);
    assert_eq!(m.hard_trigger, HARD_TRIGGER_NONE);
    assert_eq!(m.resolved_at, 0);
    assert_eq!((m.flagged_pool, m.survives_pool), (flagged, survives));
}

#[tokio::test]
async fn cancel_by_resolver_at_creation_second() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let live_before = account_lamports(&mut ctx, &live_pda(&subject_a())).await;
    let creator_before = account_lamports(&mut ctx, &creator.pubkey()).await;
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("resolver cancels at T0");
    assert_cancelled_shape(&mut ctx, &market, 3_000_000, 7_000_000).await;
    assert_lock_closed_with_rent(
        &mut ctx,
        &subject_a(),
        &creator.pubkey(),
        live_before,
        creator_before,
    )
    .await;
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

#[tokio::test]
async fn cancel_by_resolver_before_expiry() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_399).await;
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("resolver cancels before expiry");
    assert_cancelled_shape(&mut ctx, &market, 3_000_000, 7_000_000).await;
    assert_live_closed(&mut ctx, &subject_a()).await;
}

#[tokio::test]
async fn cancel_by_resolver_at_grace_boundary() {
    // The grace boundary binds strangers, not the resolver.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_400 + 86_400).await;
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("resolver cancels at the grace boundary");
    assert_cancelled_shape(&mut ctx, &market, 3_000_000, 7_000_000).await;
}

#[tokio::test]
async fn cancel_by_anyone_first_second_after_grace() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let stranger = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0 + 86_400 + 86_401).await;
    cancel(&mut ctx, &stranger, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("stranger cancels one second after grace");
    assert_cancelled_shape(&mut ctx, &market, 3_000_000, 7_000_000).await;
    assert_live_closed(&mut ctx, &subject_a()).await;
}

#[tokio::test]
async fn cancel_by_anyone_long_after_grace() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let stranger = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0 + 86_400 + 7 * 86_400).await;
    cancel(&mut ctx, &stranger, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("stranger cancels a week after grace");
    assert_cancelled_shape(&mut ctx, &market, 3_000_000, 7_000_000).await;
}

#[tokio::test]
async fn cancel_by_anyone_at_grace_boundary_rejected() {
    // The comparison is strict: at now == expiry + grace the
    // gate is still shut, and one second later the same
    // stranger succeeds — the boundary is the only fault.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let stranger = make_creator(&mut ctx).await;
    let before = read_market(&mut ctx, &market).await.unwrap();
    set_clock(&mut ctx, T0 + 86_400 + 86_400).await;
    let err = cancel(&mut ctx, &stranger, &market, &subject_a(), &creator.pubkey())
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::CancelTooEarly as u32); // 20
    assert_market_eq(&mut ctx, &market, &before).await;
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_some());
    set_clock(&mut ctx, T0 + 86_400 + 86_401).await;
    // cancel_again (caller as payer): the retry must not be a
    // byte-identical resubmission of the failed attempt, or
    // the bank would replay its cached error.
    cancel_again(&mut ctx, &stranger, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("same stranger, one second later");
    assert_cancelled_shape(&mut ctx, &market, 3_000_000, 7_000_000).await;
}

#[tokio::test]
async fn cancel_by_nonresolvers_at_expiry_rejected() {
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let stranger = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    let err = cancel(&mut ctx, &stranger, &market, &subject_a(), &creator.pubkey())
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::CancelTooEarly as u32); // 20
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_OPEN);
}

#[tokio::test]
async fn cancel_by_nonresolver_early_rejected() {
    // Neither a stranger nor the market's own creator holds
    // any cancel privilege before the grace window ends.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let stranger = make_creator(&mut ctx).await;
    let err = cancel(&mut ctx, &stranger, &market, &subject_a(), &creator.pubkey())
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::CancelTooEarly as u32); // 20
    let err = cancel(&mut ctx, &creator, &market, &subject_a(), &creator.pubkey())
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::CancelTooEarly as u32); // 20
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_OPEN);
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_some());
}

#[tokio::test]
async fn cancel_on_resolved_by_resolver_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    let err = cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::AlreadyResolved as u32); // 13
    assert_market_eq(&mut ctx, &market, &snapshot).await;
}

#[tokio::test]
async fn cancel_on_resolved_by_anyone_after_grace_rejected() {
    // Timing would pass; state is the sole fault. Terminal
    // states are final for every caller at every time.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    let stranger = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0 + 86_400 + 86_401).await;
    let err = cancel(&mut ctx, &stranger, &market, &subject_a(), &creator.pubkey())
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::AlreadyResolved as u32); // 13
    assert_market_eq(&mut ctx, &market, &snapshot).await;
}

#[tokio::test]
async fn cancel_on_cancelled_by_resolver_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("first cancel");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    // The repeat pays its own fee (see cancel_again), so the
    // resolver needs lamports — it normally never holds any.
    fund(&mut ctx, &resolver.pubkey(), 1_000_000_000)
        .await
        .expect("fund resolver");
    let err = cancel_again(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::AlreadyCancelled as u32); // 14
    assert_market_eq(&mut ctx, &market, &snapshot).await;
}

#[tokio::test]
async fn cancel_on_cancelled_by_anyone_after_grace_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("first cancel");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    let stranger = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0 + 86_400 + 86_401).await;
    let err = cancel(&mut ctx, &stranger, &market, &subject_a(), &creator.pubkey())
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::AlreadyCancelled as u32); // 14
    assert_market_eq(&mut ctx, &market, &snapshot).await;
}

// ---------------------------------------------------------------------------
// Lifecycle pins: claims stay bound to their own market after
// a successor opens, and terminal markets refuse buys.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_on_old_market_after_successor_created() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, (alice, alice_token), _b, _c) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 10).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve old market");
    // The successor opens on the same mint one second later.
    set_clock(&mut ctx, T0 + 11).await;
    let successor = create_market(&mut ctx, &creator, &subject_a(), T0 + 11, &mint)
        .await
        .expect("create successor");
    assert_ne!(successor, market);
    // Alice's claim still pays from the OLD market's vault,
    // computed from the OLD market's pools.
    claim(&mut ctx, &alice, &market, &alice_token)
        .await
        .expect("claim on old market");
    assert_eq!(token_balance(&mut ctx, &alice_token).await, 22_333_333);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 6_666_667);
    // The successor is completely untouched.
    let s = read_market(&mut ctx, &successor).await.unwrap();
    assert_eq!(s.state, STATE_OPEN);
    assert_eq!((s.flagged_pool, s.survives_pool), (0, 0));
    assert_eq!(token_balance(&mut ctx, &vault_pda(&successor)).await, 0);
    assert_eq!(
        read_live_market(&mut ctx, &subject_a()).await.unwrap().market,
        successor
    );
}

#[tokio::test]
async fn buy_on_resolved_market_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve at T0");
    // Clock is still T0, so expiry cannot be the fault: the
    // state check is the only thing standing in the way. The
    // buyer is a fresh user — a repeat of a setup buy would be
    // a byte-identical transaction and would return its cached
    // success instead of executing.
    let (dave, dave_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    let err = buy(&mut ctx, &dave, &dave_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::MarketNotOpen as u32); // 10
    assert_accounting(&mut ctx, &market, 3_000_000, 7_000_000).await;
}

#[tokio::test]
async fn buy_on_cancelled_market_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("cancel at T0");
    let (dave, dave_token) = make_user(&mut ctx, &mint, 5_000_000).await;
    let err = buy(&mut ctx, &dave, &dave_token, &market, SIDE_FLAGGED, 1_000_000)
        .await
        .unwrap_err();
    expect_custom(&err, RugCallError::MarketNotOpen as u32); // 10
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

// ---------------------------------------------------------------------------
// The R-8 pins: a market that terminates inside its creation
// second still occupies its timestamp-seeded PDA, so a
// same-second re-create on the same mint fails closed with
// InvalidAccountData (the occupied-PDA check — NOT
// DuplicateLiveMarket: the lock is already gone). One second
// later the successor derives a fresh PDA and succeeds.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recreate_same_second_after_resolve_fails_then_succeeds() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let other_creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    resolve(
        &mut ctx,
        &resolver,
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        1,
        [9u8; 32],
    )
    .await
    .expect("early resolve at T0");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    // Same second, different creator (a same-creator resend
    // would be a byte-identical transaction the harness
    // reports as already-processed without re-executing).
    let err = send(
        &mut ctx,
        &[create_market_ix(&other_creator.pubkey(), &subject_a(), T0, &mint)],
        &[&other_creator],
    )
    .await
    .unwrap_err();
    expect_invalid_account_data(&err);
    // The failed create resurrected nothing and overwrote
    // nothing: the terminal market is intact, the lock stays
    // closed.
    assert_market_eq(&mut ctx, &market, &snapshot).await;
    assert_live_closed(&mut ctx, &subject_a()).await;
    // One second later: a fresh PDA, a fresh market.
    set_clock(&mut ctx, T0 + 1).await;
    let successor = create_market(&mut ctx, &other_creator, &subject_a(), T0 + 1, &mint)
        .await
        .expect("re-create one second later");
    assert_ne!(successor, market);
    let s = read_market(&mut ctx, &successor).await.unwrap();
    assert_eq!(s.created_at, T0 + 1);
    assert_eq!(s.expiry, T0 + 1 + 86_400);
    assert_eq!(
        read_live_market(&mut ctx, &subject_a()).await.unwrap().market,
        successor
    );
    assert_market_eq(&mut ctx, &market, &snapshot).await;
}

#[tokio::test]
async fn recreate_same_second_after_cancel_fails_then_succeeds() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let creator = make_creator(&mut ctx).await;
    let other_creator = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0).await;
    let market = create_market(&mut ctx, &creator, &subject_a(), T0, &mint)
        .await
        .expect("create market");
    cancel(&mut ctx, &resolver, &market, &subject_a(), &creator.pubkey())
        .await
        .expect("cancel at T0");
    let snapshot = read_market(&mut ctx, &market).await.unwrap();
    let err = send(
        &mut ctx,
        &[create_market_ix(&other_creator.pubkey(), &subject_a(), T0, &mint)],
        &[&other_creator],
    )
    .await
    .unwrap_err();
    expect_invalid_account_data(&err);
    assert_market_eq(&mut ctx, &market, &snapshot).await;
    assert_live_closed(&mut ctx, &subject_a()).await;
    set_clock(&mut ctx, T0 + 1).await;
    let successor = create_market(&mut ctx, &other_creator, &subject_a(), T0 + 1, &mint)
        .await
        .expect("re-create one second later");
    assert_ne!(successor, market);
    let s = read_market(&mut ctx, &successor).await.unwrap();
    assert_eq!(s.created_at, T0 + 1);
    assert_eq!(
        read_live_market(&mut ctx, &subject_a()).await.unwrap().market,
        successor
    );
    assert_market_eq(&mut ctx, &market, &snapshot).await;
}

// ---------------------------------------------------------------------------
// Binding attacks (invariants M-3, M-7): each test corrupts
// exactly one account in an otherwise valid, would-succeed
// instruction. PDA key mismatches are InvalidPda (4), the
// token-program identity is UnsupportedTokenProgram (6),
// token and stored-field binding faults are MintMismatch (5),
// and a missing signer is the program's own
// MissingRequiredSignature.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn binding_resolve_wrong_config_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    let mut ix = resolve_ix(
        &resolver.pubkey(),
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    );
    ix.accounts[1] = AccountMeta::new_readonly(Pubkey::new_unique(), false);
    let err = send(&mut ctx, &[ix], &[&resolver]).await.unwrap_err();
    expect_custom(&err, RugCallError::InvalidPda as u32); // 4
    let m = read_market(&mut ctx, &market).await.unwrap();
    assert_eq!(m.state, STATE_OPEN);
}

#[tokio::test]
async fn binding_resolve_wrong_live_market_rejected() {
    // The supplied lock is a REAL live lock — of another
    // market. A valid account at the wrong address is still
    // the wrong address.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    create_market(&mut ctx, &creator, &subject_b(), T0, &mint)
        .await
        .expect("create market B");
    set_clock(&mut ctx, T0 + 86_400).await;
    let mut ix = resolve_ix(
        &resolver.pubkey(),
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    );
    ix.accounts[3] = AccountMeta::new(live_pda(&subject_b()), false);
    let err = send(&mut ctx, &[ix], &[&resolver]).await.unwrap_err();
    expect_custom(&err, RugCallError::InvalidPda as u32); // 4
    assert_eq!(
        read_market(&mut ctx, &market).await.unwrap().state,
        STATE_OPEN
    );
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_some());
    assert!(read_live_market(&mut ctx, &subject_b()).await.is_some());
}

#[tokio::test]
async fn binding_resolve_wrong_creator_rejected() {
    // The rent destination must be the market's stored
    // creator: a stranger in that slot is a stored-binding
    // fault (MintMismatch, the Day-2 precedent for stored
    // fields), and receives nothing.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let stranger = make_creator(&mut ctx).await;
    let stranger_before = account_lamports(&mut ctx, &stranger.pubkey()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    let mut ix = resolve_ix(
        &resolver.pubkey(),
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    );
    ix.accounts[4] = AccountMeta::new(stranger.pubkey(), false);
    let err = send(&mut ctx, &[ix], &[&resolver]).await.unwrap_err();
    expect_custom(&err, RugCallError::MintMismatch as u32); // 5
    assert_eq!(
        read_market(&mut ctx, &market).await.unwrap().state,
        STATE_OPEN
    );
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_some());
    assert_eq!(
        account_lamports(&mut ctx, &stranger.pubkey()).await,
        stranger_before
    );
}

#[tokio::test]
async fn binding_resolve_unsigned_resolver_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    set_clock(&mut ctx, T0 + 86_400).await;
    let mut ix = resolve_ix(
        &resolver.pubkey(),
        &market,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    );
    ix.accounts[0].is_signer = false;
    let err = send(&mut ctx, &[ix], &[]).await.unwrap_err();
    expect_instruction_error(&err, InstructionError::MissingRequiredSignature);
    assert_eq!(
        read_market(&mut ctx, &market).await.unwrap().state,
        STATE_OPEN
    );
}

/// Shared base for the Claim binding attacks: STD3 resolved
/// Flagged at expiry, Alice about to claim 3_333_333.
async fn setup_resolved_std3(
    ctx: &mut ProgramTestContext,
    resolver: &Keypair,
    mint: &Pubkey,
) -> (Keypair, Pubkey, (Keypair, Pubkey), (Keypair, Pubkey), (Keypair, Pubkey)) {
    let out = setup_std3(ctx, mint, &subject_a()).await;
    set_clock(ctx, T0 + 86_400).await;
    resolve(
        ctx,
        resolver,
        &out.1,
        &subject_a(),
        &out.0.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve flagged");
    out
}

#[tokio::test]
async fn binding_claim_wrong_config_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (_creator, market, (alice, alice_token), _b, _c) =
        setup_resolved_std3(&mut ctx, &resolver, &mint).await;
    let mut ix = claim_ix(&alice.pubkey(), &market, &alice_token);
    ix.accounts[1] = AccountMeta::new_readonly(Pubkey::new_unique(), false);
    let err = send(&mut ctx, &[ix], &[&alice]).await.unwrap_err();
    expect_custom(&err, RugCallError::InvalidPda as u32); // 4
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

#[tokio::test]
async fn binding_claim_wrong_vault_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, (alice, alice_token), _b, _c) =
        setup_resolved_std3(&mut ctx, &resolver, &mint).await;
    let market_b = create_market(&mut ctx, &creator, &subject_b(), T0 + 86_400, &mint)
        .await
        .expect("create market B");
    let mut ix = claim_ix(&alice.pubkey(), &market, &alice_token);
    ix.accounts[3] = AccountMeta::new(vault_pda(&market_b), false);
    let err = send(&mut ctx, &[ix], &[&alice]).await.unwrap_err();
    expect_custom(&err, RugCallError::InvalidPda as u32); // 4
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert!(!pos.claimed);
}

#[tokio::test]
async fn binding_claim_wrong_token_program_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (_creator, market, (alice, alice_token), _b, _c) =
        setup_resolved_std3(&mut ctx, &resolver, &mint).await;
    let mut ix = claim_ix(&alice.pubkey(), &market, &alice_token);
    ix.accounts[6] = AccountMeta::new_readonly(solana_program::system_program::id(), false);
    let err = send(&mut ctx, &[ix], &[&alice]).await.unwrap_err();
    expect_custom(&err, RugCallError::UnsupportedTokenProgram as u32); // 6
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

#[tokio::test]
async fn binding_claim_wrong_mint_token_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (_creator, market, (alice, _at), _b, _c) =
        setup_resolved_std3(&mut ctx, &resolver, &mint).await;
    // Alice's own token account — on a different 6-decimal
    // mint. Owner matches, mint does not.
    let other_mint = make_collateral_mint(&mut ctx).await;
    let alice_wrong_token = make_user_with(&mut ctx, &alice, &other_mint, 5_000_000).await;
    let ix = claim_ix(&alice.pubkey(), &market, &alice_wrong_token);
    let err = send(&mut ctx, &[ix], &[&alice]).await.unwrap_err();
    expect_custom(&err, RugCallError::MintMismatch as u32); // 5
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

#[tokio::test]
async fn binding_claim_pays_only_signers_own_token() {
    // Invariant M-7: a claim can only ever pay the signer's
    // own token account. Alice claiming into Bob's account is
    // a binding fault, and nobody's balance moves.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (_creator, market, (alice, _at), (_bob, bob_token), _c) =
        setup_resolved_std3(&mut ctx, &resolver, &mint).await;
    let bob_before = token_balance(&mut ctx, &bob_token).await;
    let ix = claim_ix(&alice.pubkey(), &market, &bob_token);
    let err = send(&mut ctx, &[ix], &[&alice]).await.unwrap_err();
    expect_custom(&err, RugCallError::MintMismatch as u32); // 5
    assert_eq!(token_balance(&mut ctx, &bob_token).await, bob_before);
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert!(!pos.claimed);
}

#[tokio::test]
async fn binding_claim_position_of_another_user_rejected() {
    // Bob signs and pays himself, but supplies Alice's
    // position: the position PDA is re-derived from the
    // signer, so the supplied account is simply not his.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (_creator, market, (alice, _at), (bob, bob_token), _c) =
        setup_resolved_std3(&mut ctx, &resolver, &mint).await;
    let mut ix = claim_ix(&bob.pubkey(), &market, &bob_token);
    ix.accounts[5] = AccountMeta::new(position_pda(&market, &alice.pubkey()), false);
    let err = send(&mut ctx, &[ix], &[&bob]).await.unwrap_err();
    expect_custom(&err, RugCallError::InvalidPda as u32); // 4
    assert_eq!(token_balance(&mut ctx, &vault_pda(&market)).await, 10_000_000);
}

#[tokio::test]
async fn binding_claim_position_from_another_market_rejected() {
    // Alice genuinely holds a position in market B — and it
    // still buys her nothing on market A.
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market_a, (alice, alice_token), _b, _c) =
        setup_std3(&mut ctx, &mint, &subject_a()).await;
    let market_b = create_market(&mut ctx, &creator, &subject_b(), T0, &mint)
        .await
        .expect("create market B");
    buy(&mut ctx, &alice, &alice_token, &market_b, SIDE_FLAGGED, 1_000_000)
        .await
        .expect("alice buys B flagged");
    set_clock(&mut ctx, T0 + 86_400).await;
    resolve(
        &mut ctx,
        &resolver,
        &market_a,
        &subject_a(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve A");
    resolve(
        &mut ctx,
        &resolver,
        &market_b,
        &subject_b(),
        &creator.pubkey(),
        OUTCOME_FLAGGED,
        3,
        [9u8; 32],
    )
    .await
    .expect("resolve B");
    let mut ix = claim_ix(&alice.pubkey(), &market_a, &alice_token);
    ix.accounts[5] = AccountMeta::new(position_pda(&market_b, &alice.pubkey()), false);
    let err = send(&mut ctx, &[ix], &[&alice]).await.unwrap_err();
    expect_custom(&err, RugCallError::InvalidPda as u32); // 4
    assert_eq!(
        token_balance(&mut ctx, &vault_pda(&market_a)).await,
        10_000_000
    );
}

#[tokio::test]
async fn binding_claim_unsigned_user_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (_creator, market, (alice, alice_token), _b, _c) =
        setup_resolved_std3(&mut ctx, &resolver, &mint).await;
    let alice_before = token_balance(&mut ctx, &alice_token).await;
    let mut ix = claim_ix(&alice.pubkey(), &market, &alice_token);
    ix.accounts[0].is_signer = false;
    let err = send(&mut ctx, &[ix], &[]).await.unwrap_err();
    expect_instruction_error(&err, InstructionError::MissingRequiredSignature);
    assert_eq!(token_balance(&mut ctx, &alice_token).await, alice_before);
    let pos = read_position(&mut ctx, &market, &alice.pubkey())
        .await
        .unwrap();
    assert!(!pos.claimed);
}

#[tokio::test]
async fn binding_cancel_wrong_config_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let mut ix = cancel_ix(
        &resolver.pubkey(),
        &market,
        &subject_a(),
        &creator.pubkey(),
    );
    ix.accounts[1] = AccountMeta::new_readonly(Pubkey::new_unique(), false);
    let err = send(&mut ctx, &[ix], &[&resolver]).await.unwrap_err();
    expect_custom(&err, RugCallError::InvalidPda as u32); // 4
    assert_eq!(
        read_market(&mut ctx, &market).await.unwrap().state,
        STATE_OPEN
    );
}

#[tokio::test]
async fn binding_cancel_wrong_live_market_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    create_market(&mut ctx, &creator, &subject_b(), T0, &mint)
        .await
        .expect("create market B");
    let mut ix = cancel_ix(
        &resolver.pubkey(),
        &market,
        &subject_a(),
        &creator.pubkey(),
    );
    ix.accounts[3] = AccountMeta::new(live_pda(&subject_b()), false);
    let err = send(&mut ctx, &[ix], &[&resolver]).await.unwrap_err();
    expect_custom(&err, RugCallError::InvalidPda as u32); // 4
    assert_eq!(
        read_market(&mut ctx, &market).await.unwrap().state,
        STATE_OPEN
    );
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_some());
    assert!(read_live_market(&mut ctx, &subject_b()).await.is_some());
}

#[tokio::test]
async fn binding_cancel_wrong_creator_rejected() {
    let (mut ctx, _authority, resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let stranger = make_creator(&mut ctx).await;
    let stranger_before = account_lamports(&mut ctx, &stranger.pubkey()).await;
    let mut ix = cancel_ix(
        &resolver.pubkey(),
        &market,
        &subject_a(),
        &creator.pubkey(),
    );
    ix.accounts[4] = AccountMeta::new(stranger.pubkey(), false);
    let err = send(&mut ctx, &[ix], &[&resolver]).await.unwrap_err();
    expect_custom(&err, RugCallError::MintMismatch as u32); // 5
    assert_eq!(
        read_market(&mut ctx, &market).await.unwrap().state,
        STATE_OPEN
    );
    assert!(read_live_market(&mut ctx, &subject_a()).await.is_some());
    assert_eq!(
        account_lamports(&mut ctx, &stranger.pubkey()).await,
        stranger_before
    );
}

#[tokio::test]
async fn binding_cancel_unsigned_caller_rejected() {
    // Anyone may cancel after grace — but "anyone" still has
    // to sign. An unsigned caller slot is refused even when
    // the timing would pass.
    let (mut ctx, _authority, _resolver, mint) = start_initialized().await;
    let (creator, market, _a, _b, _c) = setup_std3(&mut ctx, &mint, &subject_a()).await;
    let stranger = make_creator(&mut ctx).await;
    set_clock(&mut ctx, T0 + 86_400 + 86_401).await;
    let mut ix = cancel_ix(
        &stranger.pubkey(),
        &market,
        &subject_a(),
        &creator.pubkey(),
    );
    ix.accounts[0].is_signer = false;
    let err = send(&mut ctx, &[ix], &[]).await.unwrap_err();
    expect_instruction_error(&err, InstructionError::MissingRequiredSignature);
    assert_eq!(
        read_market(&mut ctx, &market).await.unwrap().state,
        STATE_OPEN
    );
}
