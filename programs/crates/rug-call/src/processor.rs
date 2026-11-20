//! RugCall instruction handlers.
//!
//! The dispatch and the instruction set are final, and all six
//! handlers are implemented: InitializeConfig, CreateMarket and
//! BuyPosition (Day 2) plus ResolveMarket, Claim and CancelMarket
//! (Day 3). The state layout they operate on is in the state
//! module and does not change.

use borsh::BorshDeserialize;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    msg,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};
use spl_token::{
    instruction::{initialize_account3, transfer},
    state::{Account as TokenAccount, Mint},
};

use crate::state::*;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let instruction = RugCallInstruction::try_from_slice(data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    match instruction {
        RugCallInstruction::InitializeConfig {
            resolver,
            collateral_mint,
        } => process_initialize_config(program_id, accounts, resolver, collateral_mint),
        RugCallInstruction::CreateMarket { subject_mint } => {
            process_create_market(program_id, accounts, subject_mint)
        }
        RugCallInstruction::BuyPosition { side, amount } => {
            process_buy_position(program_id, accounts, side, amount)
        }
        RugCallInstruction::ResolveMarket {
            outcome,
            hard_trigger,
            evidence_hash,
        } => process_resolve_market(program_id, accounts, outcome, hard_trigger, evidence_hash),
        RugCallInstruction::Claim {} => process_claim(program_id, accounts),
        RugCallInstruction::CancelMarket {} => process_cancel_market(program_id, accounts),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn config_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_CONFIG], program_id)
}

fn market_pda(program_id: &Pubkey, subject_mint: &Pubkey, created_at: i64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_MARKET, subject_mint.as_ref(), &created_at.to_le_bytes()],
        program_id,
    )
}

fn live_market_pda(program_id: &Pubkey, subject_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_LIVE_MARKET, subject_mint.as_ref()], program_id)
}

fn vault_pda(program_id: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_VAULT, market.as_ref()], program_id)
}

fn position_pda(program_id: &Pubkey, market: &Pubkey, user: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_POSITION, market.as_ref(), user.as_ref()],
        program_id,
    )
}

/// Prove `payer` is the program's upgrade authority, from the
/// ProgramData account — the same check the bridge programs run
/// in their Initialize (bridge-common's `verify_upgrade_authority`,
/// self-audit F-1), mirrored here because this crate is a separate
/// workspace. Structural problems with the account are
/// InvalidAccountData; a well-formed account naming a different
/// authority is NotUpgradeAuthority. A program with no upgrade
/// authority (immutable) has nobody who can initialize it — the
/// authority option tag is 0 and the check fails closed.
#[allow(deprecated)]
fn verify_upgrade_authority(
    program_id: &Pubkey,
    programdata_account: &AccountInfo,
    payer: &Pubkey,
) -> ProgramResult {
    use solana_program::bpf_loader_upgradeable;
    let expected = bpf_loader_upgradeable::get_program_data_address(program_id);
    if programdata_account.key != &expected
        || programdata_account.owner != &bpf_loader_upgradeable::id()
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = programdata_account.try_borrow_data()?;
    // Variant tags follow the enum's declaration order in
    // solana-loader-v3-interface: Uninitialized 0, Buffer 1,
    // Program 2, ProgramData 3.
    const PROGRAMDATA_METADATA_LEN: usize = 45;
    const VARIANT_PROGRAM_DATA: u32 = 3;
    if data.len() < PROGRAMDATA_METADATA_LEN
        || u32::from_le_bytes(data[0..4].try_into().unwrap()) != VARIANT_PROGRAM_DATA
    {
        return Err(ProgramError::InvalidAccountData);
    }
    // Authority option tag at byte 12 (after tag + slot). Anything
    // other than 1 (Some) — including 0 (None, an immutable
    // program) — means there is no upgrade authority to bind to.
    if data[12] != 1 {
        return Err(ProgramError::InvalidAccountData);
    }
    let authority = Pubkey::new_from_array(data[13..45].try_into().unwrap());
    if authority != *payer {
        return Err(RugCallError::NotUpgradeAuthority.into());
    }
    Ok(())
}

/// Create a PDA-owned account, adopting a pre-funded shell if one
/// exists (the bridge/LaunchGuard create-or-adopt pattern,
/// self-audit F-2): the PDAs here are deterministic, so anyone
/// could send lamports to one before its instruction runs and
/// permanently block it — plain `create_account` refuses an
/// account that already holds lamports, and there would be no
/// recovery path. A pre-existing account is adopted only if it is
/// a pristine shell (lamports > 0, owned by the system program,
/// no data) — the only state a bare transfer can produce. It is
/// topped up to rent-exempt if needed, then allocated and
/// assigned with the PDA's own signature, exactly as if created
/// fresh. Any other pre-existing state still errors.
#[allow(deprecated)]
fn create_or_adopt_account<'a>(
    payer: &AccountInfo<'a>,
    new_account: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
    owner: &Pubkey,
    space: usize,
    seeds: &[&[u8]],
) -> ProgramResult {
    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(space);
    if new_account.lamports() > 0 {
        if *new_account.owner != solana_program::system_program::ID
            || !new_account.data_is_empty()
        {
            return Err(ProgramError::InvalidAccountData);
        }
        if new_account.lamports() < lamports {
            invoke(
                &system_instruction::transfer(
                    payer.key,
                    new_account.key,
                    lamports - new_account.lamports(),
                ),
                &[payer.clone(), new_account.clone(), system_program.clone()],
            )?;
        }
        invoke_signed(
            &system_instruction::allocate(new_account.key, space as u64),
            &[new_account.clone(), system_program.clone()],
            &[seeds],
        )?;
        invoke_signed(
            &system_instruction::assign(new_account.key, owner),
            &[new_account.clone(), system_program.clone()],
            &[seeds],
        )?;
        return Ok(());
    }
    let ix = system_instruction::create_account(
        payer.key,
        new_account.key,
        lamports,
        space as u64,
        owner,
    );
    invoke_signed(
        &ix,
        &[payer.clone(), new_account.clone(), system_program.clone()],
        &[seeds],
    )
}

fn load_config(account: &AccountInfo, program_id: &Pubkey) -> Result<RugCallConfig, ProgramError> {
    // An account the program does not own at the config PDA is a
    // not-yet-created config (system-owned shell or nothing at
    // all): InitializeConfig has not run, and every other handler
    // fails closed rather than inventing a resolver or mint.
    if account.owner != program_id {
        return Err(RugCallError::ConfigNotInitialized.into());
    }
    let data = account.try_borrow_data()?;
    if data.is_empty() || data[0] != DISCRIMINATOR_CONFIG {
        return Err(ProgramError::InvalidAccountData);
    }
    RugCallConfig::try_from_slice(&data).map_err(|_| ProgramError::InvalidAccountData)
}

fn save_config(account: &AccountInfo, config: &RugCallConfig) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(config).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

fn load_market(account: &AccountInfo, program_id: &Pubkey) -> Result<Market, ProgramError> {
    // The market must be one of ours. An account owned by another
    // program can carry attacker-chosen bytes, and there is no
    // MarketNotInitialized error — a foreign or missing market is
    // invalid account data, full stop.
    if account.owner != program_id {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = account.try_borrow_data()?;
    if data.is_empty() || data[0] != DISCRIMINATOR_MARKET {
        return Err(ProgramError::InvalidAccountData);
    }
    let market = Market::try_from_slice(&data).map_err(|_| ProgramError::InvalidAccountData)?;
    if !matches!(
        market.state,
        STATE_OPEN | STATE_RESOLVED | STATE_CANCELLED
    ) {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(market)
}

fn save_market(account: &AccountInfo, market: &Market) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(market).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

fn save_live_market(account: &AccountInfo, live: &LiveMarket) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(live).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

fn load_live_market(account: &AccountInfo, program_id: &Pubkey) -> Result<LiveMarket, ProgramError> {
    // Same rule as the market loader: an account the program
    // does not own carries attacker-chosen bytes, and the frozen
    // error enum has no LiveMarketNotInitialized variant.
    if account.owner != program_id {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = account.try_borrow_data()?;
    if data.is_empty() || data[0] != DISCRIMINATOR_LIVE_MARKET {
        return Err(ProgramError::InvalidAccountData);
    }
    LiveMarket::try_from_slice(&data).map_err(|_| ProgramError::InvalidAccountData)
}

/// Close the LiveMarket lock, returning its rent to the market
/// creator (the Day-1 record: the lock's lifetime is the
/// market's Open lifetime, and its rent follows the market
/// home). Native close, no CPI: credit the creator, drain the
/// lock, hand it back to the system program, truncate it to
/// zero length — the runtime reclaims a zero-lamport,
/// system-owned, zero-length account at transaction end,
/// freeing the address for the next market on the same mint.
/// No system-program account is needed because nothing here
/// invokes it.
///
/// Borrow discipline: the credit runs in its own scope and
/// drops before the lock is touched, and only the creator
/// ENTRY is ever borrowed — when the caller or resolver is
/// also the creator, the account list holds the same key
/// twice (signer entry + creator entry) over one underlying
/// account; borrowing both entries at once would trip the
/// borrow check, and crediting through both would pay the
/// rent twice.
fn close_live_market(live_account: &AccountInfo, creator_account: &AccountInfo) -> ProgramResult {
    let closing_lamports = live_account.lamports();
    {
        let mut dest = creator_account.try_borrow_mut_lamports()?;
        **dest = (**dest)
            .checked_add(closing_lamports)
            .ok_or(RugCallError::Overflow)?;
    }
    **live_account.try_borrow_mut_lamports()? = 0;
    live_account.assign(&solana_program::system_program::ID);
    live_account.resize(0)?;
    Ok(())
}

fn load_position(account: &AccountInfo, program_id: &Pubkey) -> Result<Position, ProgramError> {
    if account.owner != program_id {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = account.try_borrow_data()?;
    if data.is_empty() || data[0] != DISCRIMINATOR_POSITION {
        return Err(ProgramError::InvalidAccountData);
    }
    Position::try_from_slice(&data).map_err(|_| ProgramError::InvalidAccountData)
}

fn save_position(account: &AccountInfo, position: &Position) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(position).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

// ---------------------------------------------------------------------------
// InitializeConfig — one-time setup. Creates the config account
// carrying the resolver, the collateral mint, and the program's
// constants. Bound to the program's upgrade authority (proven
// against ProgramData, the bridge F-1 pattern): the resolver and
// mint named here are fixed for the life of the deployment — no
// instruction edits Config afterwards — so the one call that sets
// them belongs to the key that already controls the program
// binary at the loader level. The collateral mint is verified,
// not just recorded: it must be a real, initialized classic SPL
// mint with 6 decimals (the decimals MIN_STAKE_RAW is denominated
// in). A blind payload key — a typo, a Token-2022 mint, a key
// that is not a mint at all — would otherwise be a permanent
// brick that only surfaces at the first CreateMarket.
// ---------------------------------------------------------------------------

fn process_initialize_config(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    resolver: Pubkey,
    collateral_mint: Pubkey,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let payer = next_account_info(iter)?;
    let config_account = next_account_info(iter)?;
    let mint_account = next_account_info(iter)?;
    let programdata_account = next_account_info(iter)?;
    let system_program = next_account_info(iter)?;

    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (config_pda_key, config_bump) = config_pda(program_id);
    if config_account.key != &config_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    // Exactly once. A program-owned config account means setup
    // already ran — refuse, whatever bytes it carries, rather
    // than letting a second call reinterpret them. Checked before
    // the mint and authority proofs so a repeat call names the
    // real fault even from a stranger.
    if config_account.owner == program_id {
        return Err(RugCallError::ConfigAlreadyInitialized.into());
    }
    if mint_account.key != &collateral_mint {
        return Err(RugCallError::MintMismatch.into());
    }
    if mint_account.owner != &spl_token::id() {
        return Err(RugCallError::UnsupportedTokenProgram.into());
    }
    let mint = Mint::unpack(&mint_account.try_borrow_data()?)?;
    if !mint.is_initialized || mint.decimals != 6 {
        return Err(ProgramError::InvalidAccountData);
    }
    verify_upgrade_authority(program_id, programdata_account, payer.key)?;
    create_or_adopt_account(
        payer,
        config_account,
        system_program,
        program_id,
        RugCallConfig::LEN,
        &[SEED_CONFIG, &[config_bump]],
    )?;
    save_config(
        config_account,
        &RugCallConfig {
            discriminator: DISCRIMINATOR_CONFIG,
            admin: *payer.key,
            resolver,
            collateral_mint,
            fee_bps: FEE_BPS,
            default_duration_secs: DEFAULT_MARKET_DURATION_SECS,
            min_stake: MIN_STAKE_RAW,
            bump: config_bump,
        },
    )?;
    msg!(
        "RugCall: initialized — resolver {}, collateral mint {}",
        resolver,
        collateral_mint
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// CreateMarket — permissionless. Opens a market on a subject mint
// (mainnet data, carried as 32 bytes; no account exists for it
// here). The creation second comes from the Clock sysvar, never
// from the caller: a supplied bucket could be ground to dodge the
// duplicate check or squat future addresses. "One live market per
// mint" is enforced by the LiveMarket lock account — created
// here, closed by ResolveMarket/CancelMarket — because the
// timestamp-seeded market PDA alone can never collide and a
// program cannot scan for sibling accounts.
// ---------------------------------------------------------------------------

fn process_create_market(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    subject_mint: Pubkey,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let creator = next_account_info(iter)?;
    let config_account = next_account_info(iter)?;
    let market_account = next_account_info(iter)?;
    let live_account = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let mint_account = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;
    let system_program = next_account_info(iter)?;

    if !creator.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if token_program.key != &spl_token::id() {
        return Err(RugCallError::UnsupportedTokenProgram.into());
    }
    let (config_pda_key, _) = config_pda(program_id);
    if config_account.key != &config_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    let config = load_config(config_account, program_id)?;
    if mint_account.key != &config.collateral_mint {
        return Err(RugCallError::MintMismatch.into());
    }
    if mint_account.owner != &spl_token::id() {
        return Err(RugCallError::UnsupportedTokenProgram.into());
    }

    let created_at = Clock::get()?.unix_timestamp;
    let created_bytes = created_at.to_le_bytes();
    let (market_pda_key, market_bump) = market_pda(program_id, &subject_mint, created_at);
    let (live_pda_key, live_bump) = live_market_pda(program_id, &subject_mint);
    let (vault_pda_key, vault_bump) = vault_pda(program_id, &market_pda_key);
    if market_account.key != &market_pda_key
        || live_account.key != &live_pda_key
        || vault_account.key != &vault_pda_key
    {
        return Err(RugCallError::InvalidPda.into());
    }
    // The duplicate check, before any creation: a program-owned
    // lock account is a live market by construction, whatever
    // bytes it carries.
    if live_account.owner == program_id {
        return Err(RugCallError::DuplicateLiveMarket.into());
    }
    if market_account.owner == program_id {
        // Same-second re-create: the market PDA is second-bucketed
        // and market accounts are never closed, so a market that
        // was created and resolved inside one second still sits at
        // this address. Overwriting a terminal market would break
        // the post-resolution immutability invariant — fail
        // closed; a retry one second later derives a fresh PDA.
        return Err(ProgramError::InvalidAccountData);
    }
    // Wall-clock by design, and checked anyway: a wrapping add
    // would mint a market born already expired.
    let expiry = created_at
        .checked_add(config.default_duration_secs)
        .ok_or(RugCallError::Overflow)?;

    create_or_adopt_account(
        creator,
        live_account,
        system_program,
        program_id,
        LiveMarket::LEN,
        &[SEED_LIVE_MARKET, subject_mint.as_ref(), &[live_bump]],
    )?;
    create_or_adopt_account(
        creator,
        market_account,
        system_program,
        program_id,
        Market::LEN,
        &[
            SEED_MARKET,
            subject_mint.as_ref(),
            &created_bytes,
            &[market_bump],
        ],
    )?;
    // The vault: an SPL token account at its own PDA, owned (as a
    // token account) by the market PDA, so only this program —
    // signing as the market — can ever move the pool.
    create_or_adopt_account(
        creator,
        vault_account,
        system_program,
        &spl_token::id(),
        TokenAccount::LEN,
        &[SEED_VAULT, market_pda_key.as_ref(), &[vault_bump]],
    )?;
    invoke(
        &initialize_account3(
            token_program.key,
            vault_account.key,
            &config.collateral_mint,
            &market_pda_key,
        )?,
        &[
            vault_account.clone(),
            mint_account.clone(),
            market_account.clone(),
            token_program.clone(),
        ],
    )?;

    save_market(
        market_account,
        &Market {
            discriminator: DISCRIMINATOR_MARKET,
            subject_mint,
            creator: *creator.key,
            created_at,
            expiry,
            flagged_pool: 0,
            survives_pool: 0,
            state: STATE_OPEN,
            outcome: OUTCOME_NONE,
            evidence_hash: [0u8; 32],
            hard_trigger: HARD_TRIGGER_NONE,
            resolved_at: 0,
            bump: market_bump,
            vault_bump,
        },
    )?;
    save_live_market(
        live_account,
        &LiveMarket {
            discriminator: DISCRIMINATOR_LIVE_MARKET,
            market: market_pda_key,
            bump: live_bump,
        },
    )?;
    msg!(
        "RugCall: market {} on subject {} expires {}",
        market_pda_key,
        subject_mint,
        expiry
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// BuyPosition — stake collateral on one side of an open market.
// Every check that can fail runs before any account is created or
// any token moves, and all of the arithmetic is checked before
// the transfer CPI: the refusal of an overflowing buy must be
// this program's own error, not whatever the token program
// happens to return for an unbacked amount. Pools and positions
// are credited by exactly the transferred amount — no fee, no
// skim, no rounding exists in a buy — so the sum of position
// amounts per side always equals that side's pool.
// ---------------------------------------------------------------------------

fn process_buy_position(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    side: u8,
    amount: u64,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let user = next_account_info(iter)?;
    let config_account = next_account_info(iter)?;
    let market_account = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let user_token = next_account_info(iter)?;
    let position_account = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;
    let system_program = next_account_info(iter)?;

    if !user.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if token_program.key != &spl_token::id() {
        return Err(RugCallError::UnsupportedTokenProgram.into());
    }
    let (config_pda_key, _) = config_pda(program_id);
    if config_account.key != &config_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    let config = load_config(config_account, program_id)?;
    if side != SIDE_FLAGGED && side != SIDE_SURVIVES {
        return Err(RugCallError::InvalidSide.into());
    }
    // The minimum is Config's, not the constant's: the constant
    // only seeds Config at InitializeConfig. A zero amount is
    // rejected by this same check.
    if amount < config.min_stake {
        return Err(RugCallError::BelowMinStake.into());
    }

    let mut market = load_market(market_account, program_id)?;
    // Binding: the supplied account must be the PDA its own
    // stored fields derive. Creation checked the address against
    // a Clock-derived timestamp; re-deriving on every use binds
    // the stored subject and creation second to the address, so a
    // program-owned market can never be substituted cross-mint.
    let (market_pda_key, _) = market_pda(program_id, &market.subject_mint, market.created_at);
    if market_account.key != &market_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    if market.state != STATE_OPEN {
        return Err(RugCallError::MarketNotOpen.into());
    }
    // Buys require now < expiry; at now == expiry the resolution
    // path wins the tie — the fail-closed direction.
    let now = Clock::get()?.unix_timestamp;
    if now >= market.expiry {
        return Err(RugCallError::BuyAfterExpiry.into());
    }

    let (vault_pda_key, _) = vault_pda(program_id, market_account.key);
    if vault_account.key != &vault_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    let vault_data = TokenAccount::unpack(&vault_account.try_borrow_data()?)?;
    if vault_data.mint != config.collateral_mint {
        return Err(RugCallError::MintMismatch.into());
    }
    if vault_data.owner != *market_account.key {
        return Err(ProgramError::InvalidAccountData);
    }
    let user_data = TokenAccount::unpack(&user_token.try_borrow_data()?)?;
    if user_data.owner != *user.key || user_data.mint != config.collateral_mint {
        return Err(RugCallError::MintMismatch.into());
    }

    let (position_pda_key, position_bump) = position_pda(program_id, market_account.key, user.key);
    if position_account.key != &position_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    let position_exists = position_account.owner == program_id;
    let mut position = if position_exists {
        let loaded = load_position(position_account, program_id)?;
        if loaded.market != *market_account.key || loaded.user != *user.key {
            return Err(RugCallError::MintMismatch.into());
        }
        loaded
    } else {
        Position {
            discriminator: DISCRIMINATOR_POSITION,
            market: *market_account.key,
            user: *user.key,
            flagged_amount: 0,
            survives_amount: 0,
            claimed: false,
            bump: position_bump,
        }
    };

    // All of the checked math, before the balance check, any
    // creation, or the transfer: the side pool, the position's
    // side, and the prospective total built from the NEW pools
    // (the same construction Claim will use on stored pools). A
    // buy that would overflow any of them is refused here with
    // this program's own error — even when the buyer could not
    // have backed the amount, because the refusal must name the
    // real fault (an overflowing pool), not the incidental one
    // (an empty wallet).
    let (new_flagged, new_survives, new_position_flagged, new_position_survives) =
        if side == SIDE_FLAGGED {
            let pool = market
                .flagged_pool
                .checked_add(amount)
                .ok_or(RugCallError::Overflow)?;
            let _new_total = pool
                .checked_add(market.survives_pool)
                .ok_or(RugCallError::Overflow)?;
            let pos = position
                .flagged_amount
                .checked_add(amount)
                .ok_or(RugCallError::Overflow)?;
            (pool, market.survives_pool, pos, position.survives_amount)
        } else {
            let pool = market
                .survives_pool
                .checked_add(amount)
                .ok_or(RugCallError::Overflow)?;
            let _new_total = market
                .flagged_pool
                .checked_add(pool)
                .ok_or(RugCallError::Overflow)?;
            let pos = position
                .survives_amount
                .checked_add(amount)
                .ok_or(RugCallError::Overflow)?;
            (market.flagged_pool, pool, position.flagged_amount, pos)
        };

    // The balance check sits after the math, deliberately: an
    // overflowing buy is refused as Overflow above regardless of
    // the buyer's balance, and a merely unbacked buy is refused
    // here, before anything is created or moved.
    if user_data.amount < amount {
        return Err(ProgramError::InsufficientFunds);
    }

    if !position_exists {
        create_or_adopt_account(
            user,
            position_account,
            system_program,
            program_id,
            Position::LEN,
            &[
                SEED_POSITION,
                market_account.key.as_ref(),
                user.key.as_ref(),
                &[position_bump],
            ],
        )?;
    }
    invoke(
        &transfer(
            token_program.key,
            user_token.key,
            vault_account.key,
            user.key,
            &[],
            amount,
        )?,
        &[
            user_token.clone(),
            vault_account.clone(),
            user.clone(),
            token_program.clone(),
        ],
    )?;
    market.flagged_pool = new_flagged;
    market.survives_pool = new_survives;
    position.flagged_amount = new_position_flagged;
    position.survives_amount = new_position_survives;
    save_market(market_account, &market)?;
    save_position(position_account, &position)?;
    msg!(
        "RugCall: {} staked {} on side {} in market {}",
        user.key,
        amount,
        side,
        market_account.key
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// ResolveMarket — resolver only. Posts the outcome bound to the
// sha256 of the published evidence bundle. Irreversible: the
// market leaves Open forever, the pools freeze as they stand
// (Claim computes from them and never writes the market), and
// the LiveMarket lock closes in this same instruction, freeing
// the subject mint for its next market. Before expiry only a
// Flagged outcome is accepted — "no trigger yet" is not
// evidence of survival until the window closes — and a Flagged
// outcome must always name the trigger that fired.
// ---------------------------------------------------------------------------

fn process_resolve_market(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    outcome: u8,
    hard_trigger: u8,
    evidence_hash: [u8; 32],
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let resolver = next_account_info(iter)?;
    let config_account = next_account_info(iter)?;
    let market_account = next_account_info(iter)?;
    let live_account = next_account_info(iter)?;
    let creator_account = next_account_info(iter)?;

    if !resolver.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (config_pda_key, _) = config_pda(program_id);
    if config_account.key != &config_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    let config = load_config(config_account, program_id)?;
    // Authority first, before the market is even loaded: this
    // instruction takes orders from exactly one key, and a
    // stranger's call has no other fault worth naming. It also
    // keeps a non-resolver from probing market state (13 vs 14
    // vs 16) through this instruction's error codes.
    if resolver.key != &config.resolver {
        return Err(RugCallError::NotResolver.into());
    }
    // Payload, in field order, before any market state is read —
    // the BuyPosition precedent (side and min-stake precede the
    // market load and MarketNotOpen). The outcome byte comes
    // first because the trigger rule is defined per outcome:
    // there is no trigger validity to check until the outcome
    // itself is known to be one of the two real values.
    if outcome != OUTCOME_FLAGGED && outcome != OUTCOME_SURVIVES {
        return Err(RugCallError::InvalidSide.into());
    }
    let trigger_ok = if outcome == OUTCOME_FLAGGED {
        hard_trigger != HARD_TRIGGER_NONE && hard_trigger <= HARD_TRIGGER_MAX
    } else {
        hard_trigger == HARD_TRIGGER_NONE
    };
    if !trigger_ok {
        return Err(RugCallError::InvalidHardTrigger.into());
    }
    // The all-zero hash is the pre-resolution sentinel:
    // accepting it would post a resolution bound to no evidence
    // bundle at all.
    if evidence_hash == [0u8; 32] {
        return Err(RugCallError::InvalidEvidenceHash.into());
    }

    let mut market = load_market(market_account, program_id)?;
    // Binding, as in BuyPosition: re-derive the PDA from the
    // market's own stored fields and compare.
    let (market_pda_key, _) = market_pda(program_id, &market.subject_mint, market.created_at);
    if market_account.key != &market_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    if market.state == STATE_RESOLVED {
        return Err(RugCallError::AlreadyResolved.into());
    }
    if market.state == STATE_CANCELLED {
        return Err(RugCallError::AlreadyCancelled.into());
    }
    // One clock read: the value the timing check tests is the
    // value recorded as resolved_at. At now >= expiry both
    // outcomes are allowed; before expiry, Survives is refused —
    // the window has not closed, so survival is not yet a fact.
    let now = Clock::get()?.unix_timestamp;
    if outcome == OUTCOME_SURVIVES && now < market.expiry {
        return Err(RugCallError::EarlySurvivesResolution.into());
    }

    let (live_pda_key, _) = live_market_pda(program_id, &market.subject_mint);
    if live_account.key != &live_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    let live = load_live_market(live_account, program_id)?;
    if live.market != *market_account.key {
        return Err(RugCallError::MintMismatch.into());
    }
    // The lock's rent goes home to the market's creator, so the
    // supplied destination must be the stored creator — the
    // same stored-binding rule (and error) as a position's
    // stored market/user in BuyPosition. The creator does not
    // sign: receiving lamports needs no signature, so rent
    // recovery never depends on the creator being online.
    if creator_account.key != &market.creator {
        return Err(RugCallError::MintMismatch.into());
    }

    market.state = STATE_RESOLVED;
    market.outcome = outcome;
    market.evidence_hash = evidence_hash;
    market.hard_trigger = hard_trigger;
    market.resolved_at = now;
    save_market(market_account, &market)?;
    close_live_market(live_account, creator_account)?;
    msg!(
        "RugCall: market {} resolved outcome {} trigger {}",
        market_account.key,
        outcome,
        hard_trigger
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Claim — collect from a terminal market. Branch precedence is
// the Day-1 freeze, in this exact order: a Cancelled market
// refunds the caller's total stake; on a Resolved market a
// zero winning pool also refunds the total stake (nobody
// backed the winning side, so there is no winner to pay and
// the stakes are not stranded); otherwise a caller with no
// winning-side stake has nothing to claim; otherwise the
// caller is paid their pro-rata share of the whole pool. One
// claim per position, ever: the single `claimed` flag covers
// the payout and both refund paths.
//
// Claim changes nothing in the market account (invariant
// M-1): pools, outcome and resolution data are frozen, payouts
// are computed only from the stored pools — never from the
// live vault balance, which stray transfers can inflate — and
// every claim on a market therefore computes against the same
// numbers regardless of claim order. Floor dust stays in the
// vault by design; there is no sweep instruction in this
// version.
// ---------------------------------------------------------------------------

fn process_claim(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let iter = &mut accounts.iter();
    let user = next_account_info(iter)?;
    let config_account = next_account_info(iter)?;
    let market_account = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let user_token = next_account_info(iter)?;
    let position_account = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;

    if !user.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if token_program.key != &spl_token::id() {
        return Err(RugCallError::UnsupportedTokenProgram.into());
    }
    let (config_pda_key, _) = config_pda(program_id);
    if config_account.key != &config_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    let config = load_config(config_account, program_id)?;

    let market = load_market(market_account, program_id)?;
    // Binding, as in BuyPosition: the supplied account must be
    // the PDA its own stored fields derive.
    let (market_pda_key, _) = market_pda(program_id, &market.subject_mint, market.created_at);
    if market_account.key != &market_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    if market.state == STATE_OPEN {
        return Err(RugCallError::ClaimBeforeResolve.into());
    }
    // load_market has already rejected every state other than
    // Open / Resolved / Cancelled, so the market is terminal
    // here.

    let (vault_pda_key, _) = vault_pda(program_id, market_account.key);
    if vault_account.key != &vault_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    // Vault unpack and binding, identical to BuyPosition's.
    let vault_data = TokenAccount::unpack(&vault_account.try_borrow_data()?)?;
    if vault_data.mint != config.collateral_mint {
        return Err(RugCallError::MintMismatch.into());
    }
    if vault_data.owner != *market_account.key {
        return Err(ProgramError::InvalidAccountData);
    }
    let user_data = TokenAccount::unpack(&user_token.try_borrow_data()?)?;
    if user_data.owner != *user.key || user_data.mint != config.collateral_mint {
        return Err(RugCallError::MintMismatch.into());
    }

    let (position_pda_key, _) = position_pda(program_id, market_account.key, user.key);
    if position_account.key != &position_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    // A position account the program does not own was never
    // created: load_position fails with InvalidAccountData (the
    // Day-2 R-3 convention — the frozen enum has no
    // PositionNotFound, and an existing-but-empty position is a
    // different case with a different error, below).
    let mut position = load_position(position_account, program_id)?;
    if position.market != *market_account.key || position.user != *user.key {
        return Err(RugCallError::MintMismatch.into());
    }
    if position.claimed {
        return Err(RugCallError::AlreadyClaimed.into());
    }

    // The frozen branch precedence. All checked math happens
    // here, before the transfer CPI, so a refusal names this
    // program's own error. The refund amount is the position's
    // total stake — both sides summed with checked arithmetic.
    let total_stake = position
        .flagged_amount
        .checked_add(position.survives_amount)
        .ok_or(RugCallError::Overflow)?;
    let amount = if market.state == STATE_CANCELLED {
        if total_stake == 0 {
            return Err(RugCallError::NoClaimableStake.into());
        }
        total_stake
    } else {
        // Resolved. A resolved market's outcome is Flagged or
        // Survives by construction (ResolveMarket accepts
        // nothing else); anything else is crafted state and
        // fails closed rather than guessing a winning side.
        let (winning_pool, winning_stake) = match market.outcome {
            OUTCOME_FLAGGED => (market.flagged_pool, position.flagged_amount),
            OUTCOME_SURVIVES => (market.survives_pool, position.survives_amount),
            _ => return Err(ProgramError::InvalidAccountData),
        };
        if winning_pool == 0 {
            // Refund precedes the winning-stake check on
            // purpose: with no winning side backed there is no
            // winner to pay, so even a losing-side holder is
            // made whole — the alternative strands the entire
            // pool behind an unpayable claim.
            if total_stake == 0 {
                return Err(RugCallError::NoClaimableStake.into());
            }
            total_stake
        } else if winning_stake == 0 {
            return Err(RugCallError::NoClaimableStake.into());
        } else {
            // total_pool is built with checked_add from the
            // stored pools — here, inside the payout branch
            // only, so a pool overflow (unreachable on
            // program-written state; BuyPosition refuses the
            // buy that would create it) can never block a
            // refund, whose amount does not depend on the
            // pools. A None from the helper — inconsistent
            // inputs or a result above u64::MAX — is Overflow,
            // per the error's own doc.
            let total_pool = market
                .flagged_pool
                .checked_add(market.survives_pool)
                .ok_or(RugCallError::Overflow)?;
            parimutuel_payout(winning_stake, total_pool, winning_pool)
                .ok_or(RugCallError::Overflow)?
        }
    };
    // `amount` is never 0 here: the refund paths refused a zero
    // total stake above, and in the payout branch a winning
    // stake of at least 1 with total_pool >= winning_pool
    // forces the floor to at least 1. No zero-amount transfer
    // is ever issued.

    // The vault's token owner is the market PDA (set at
    // CreateMarket), so the market signs — with its stored
    // bump — and the market account is passed as the CPI
    // authority, exactly as BuyPosition passes its user
    // authority.
    let created_bytes = market.created_at.to_le_bytes();
    let signer_seeds: &[&[u8]] = &[
        SEED_MARKET,
        market.subject_mint.as_ref(),
        &created_bytes,
        &[market.bump],
    ];
    invoke_signed(
        &transfer(
            token_program.key,
            vault_account.key,
            user_token.key,
            market_account.key,
            &[],
            amount,
        )?,
        &[
            vault_account.clone(),
            user_token.clone(),
            market_account.clone(),
            token_program.clone(),
        ],
        &[signer_seeds],
    )?;

    // CPI-then-save, BuyPosition's precedent: the flag is
    // written only after the token program accepted the
    // payment. Safe either way under transaction atomicity —
    // a failed CPI would roll back an earlier save too — and
    // the market account is deliberately never saved here
    // (invariant M-1).
    position.claimed = true;
    save_position(position_account, &position)?;
    msg!(
        "RugCall: {} claimed {} from market {}",
        user.key,
        amount,
        market_account.key
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// CancelMarket — the fail-closed exit. The resolver may
// cancel any Open market at any time (its evidence source is
// gone, the subject is delisted, the market was a mistake);
// anyone may cancel once now > expiry + CANCEL_GRACE_SECS
// with no resolution posted, so stakes are never trapped
// behind a dead resolver. Cancelling writes nothing but the
// state byte: the outcome stays NONE, the hash stays zero,
// resolved_at stays 0, and Claim refunds exact stakes from
// the untouched pools. The LiveMarket lock closes here too,
// on the same terms as ResolveMarket.
// ---------------------------------------------------------------------------

fn process_cancel_market(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let iter = &mut accounts.iter();
    let caller = next_account_info(iter)?;
    let config_account = next_account_info(iter)?;
    let market_account = next_account_info(iter)?;
    let live_account = next_account_info(iter)?;
    let creator_account = next_account_info(iter)?;

    if !caller.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (config_pda_key, _) = config_pda(program_id);
    if config_account.key != &config_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    let config = load_config(config_account, program_id)?;
    // Computed here, enforced at the gate below: unlike
    // ResolveMarket there is a legitimate non-resolver path,
    // so being a stranger is not yet a fault.
    let is_resolver = caller.key == &config.resolver;

    let mut market = load_market(market_account, program_id)?;
    let (market_pda_key, _) = market_pda(program_id, &market.subject_mint, market.created_at);
    if market_account.key != &market_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    // Terminal states are final for every caller at every
    // time, so they are named before the grace gate: a
    // stranger cancelling a Resolved market after the window
    // gets 13, not a clock-dependent answer.
    if market.state == STATE_RESOLVED {
        return Err(RugCallError::AlreadyResolved.into());
    }
    if market.state == STATE_CANCELLED {
        return Err(RugCallError::AlreadyCancelled.into());
    }
    if !is_resolver {
        // The resolver path never computes this deadline: a
        // resolver may cancel a boundary market whose deadline
        // is unrepresentable. For anyone else the comparison
        // is strict (now > deadline), and a checked_add that
        // overflows means the deadline sits past every i64
        // clock value — the honest answer is "too early",
        // never a wrapped deadline that would open the gate
        // immediately.
        let now = Clock::get()?.unix_timestamp;
        match market.expiry.checked_add(CANCEL_GRACE_SECS) {
            Some(deadline) if now > deadline => {}
            _ => return Err(RugCallError::CancelTooEarly.into()),
        }
    }

    let (live_pda_key, _) = live_market_pda(program_id, &market.subject_mint);
    if live_account.key != &live_pda_key {
        return Err(RugCallError::InvalidPda.into());
    }
    let live = load_live_market(live_account, program_id)?;
    if live.market != *market_account.key {
        return Err(RugCallError::MintMismatch.into());
    }
    if creator_account.key != &market.creator {
        return Err(RugCallError::MintMismatch.into());
    }

    market.state = STATE_CANCELLED;
    save_market(market_account, &market)?;
    close_live_market(live_account, creator_account)?;
    msg!("RugCall: market {} cancelled", market_account.key);
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests for the mirrored upgrade-authority check. The
// integration suite can only seed a well-formed ProgramData
// account, so the structural rejections — wrong address, wrong
// owner, truncated data, wrong variant, immutable program — are
// pinned here, against this crate's own copy of the check (the
// mirror in this crate must not be able to drift silently from
// the bridge-common original it copies).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    fn account<'a>(
        key: &'a Pubkey,
        owner: &'a Pubkey,
        lamports: &'a mut u64,
        data: &'a mut Vec<u8>,
    ) -> AccountInfo<'a> {
        AccountInfo {
            key,
            is_signer: false,
            is_writable: true,
            lamports: Rc::new(RefCell::new(lamports)),
            data: Rc::new(RefCell::new(data)),
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    fn programdata_bytes(authority: Option<Pubkey>) -> Vec<u8> {
        let mut data = vec![0u8; 45];
        data[0..4].copy_from_slice(&3u32.to_le_bytes());
        data[4..12].copy_from_slice(&123u64.to_le_bytes());
        if let Some(a) = authority {
            data[12] = 1;
            data[13..45].copy_from_slice(a.as_ref());
        }
        data
    }

    fn check(
        program_id: &Pubkey,
        key: &Pubkey,
        owner: &Pubkey,
        data: Vec<u8>,
        payer: &Pubkey,
    ) -> ProgramResult {
        let mut lamports = 1_000_000u64;
        let mut data = data;
        let acct = account(key, owner, &mut lamports, &mut data);
        verify_upgrade_authority(program_id, &acct, payer)
    }

    #[test]
    #[allow(deprecated)]
    fn upgrade_authority_accepts_the_recorded_authority() {
        let loader = solana_program::bpf_loader_upgradeable::id();
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let pd = solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id);
        let authority = Pubkey::new_unique();
        assert_eq!(
            check(
                &program_id,
                &pd,
                &loader,
                programdata_bytes(Some(authority)),
                &authority
            ),
            Ok(())
        );
    }

    #[test]
    #[allow(deprecated)]
    fn upgrade_authority_rejects_a_stranger() {
        let loader = solana_program::bpf_loader_upgradeable::id();
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let pd = solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id);
        let authority = Pubkey::new_unique();
        let stranger = Pubkey::new_unique();
        assert_eq!(
            check(
                &program_id,
                &pd,
                &loader,
                programdata_bytes(Some(authority)),
                &stranger
            ),
            Err(RugCallError::NotUpgradeAuthority.into())
        );
    }

    #[test]
    #[allow(deprecated)]
    fn upgrade_authority_rejects_an_immutable_program() {
        // Option tag 0: no upgrade authority exists, so nobody can
        // initialize — the check fails closed.
        let loader = solana_program::bpf_loader_upgradeable::id();
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let pd = solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id);
        let authority = Pubkey::new_unique();
        assert_eq!(
            check(
                &program_id,
                &pd,
                &loader,
                programdata_bytes(None),
                &authority
            ),
            Err(ProgramError::InvalidAccountData)
        );
    }

    #[test]
    #[allow(deprecated)]
    fn upgrade_authority_rejects_wrong_address_and_wrong_owner() {
        let loader = solana_program::bpf_loader_upgradeable::id();
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let pd = solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id);
        let authority = Pubkey::new_unique();
        // The right bytes at the wrong address prove nothing.
        let wrong = Pubkey::new_unique();
        assert_eq!(
            check(
                &program_id,
                &wrong,
                &loader,
                programdata_bytes(Some(authority)),
                &authority
            ),
            Err(ProgramError::InvalidAccountData)
        );
        // The right address not owned by the loader proves nothing.
        assert_eq!(
            check(
                &program_id,
                &pd,
                &program_id,
                programdata_bytes(Some(authority)),
                &authority
            ),
            Err(ProgramError::InvalidAccountData)
        );
    }

    #[test]
    #[allow(deprecated)]
    fn upgrade_authority_rejects_truncated_and_wrong_variant_data() {
        let loader = solana_program::bpf_loader_upgradeable::id();
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let pd = solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id);
        let authority = Pubkey::new_unique();
        // Truncated: shorter than the 45-byte metadata.
        let mut short = programdata_bytes(Some(authority));
        short.truncate(20);
        assert_eq!(
            check(&program_id, &pd, &loader, short, &authority),
            Err(ProgramError::InvalidAccountData)
        );
        // Wrong variant tag (2 = Program, not 3 = ProgramData).
        let mut wrong_variant = programdata_bytes(Some(authority));
        wrong_variant[0..4].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            check(&program_id, &pd, &loader, wrong_variant, &authority),
            Err(ProgramError::InvalidAccountData)
        );
    }
}
