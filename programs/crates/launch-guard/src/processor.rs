//! LaunchGuard instruction handlers. Design: programs/LAUNCH-GUARD.md.

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
    let instruction = LaunchInstruction::try_from_slice(data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    match instruction {
        LaunchInstruction::RegisterAndLock {
            lock_amount,
            lock_slots,
        } => process_register(program_id, accounts, lock_amount, lock_slots),
        LaunchInstruction::ExtendLock { additional_slots } => {
            process_extend(program_id, accounts, additional_slots)
        }
        LaunchInstruction::Release {} => process_release(program_id, accounts),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_record(account: &AccountInfo) -> Result<LaunchRecord, ProgramError> {
    let data = account.try_borrow_data()?;
    if data.is_empty() || data[0] != DISCRIMINATOR_LAUNCH {
        return Err(ProgramError::InvalidAccountData);
    }
    LaunchRecord::try_from_slice(&data).map_err(|_| ProgramError::InvalidAccountData)
}

fn save_record(account: &AccountInfo, record: &LaunchRecord) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(record).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

fn create_pda_account<'a>(
    payer: &AccountInfo<'a>,
    new_account: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
    program_id: &Pubkey,
    space: usize,
    seeds: &[&[u8]],
) -> ProgramResult {
    let rent = Rent::get()?;
    let ix = system_instruction::create_account(
        payer.key,
        new_account.key,
        rent.minimum_balance(space),
        space as u64,
        program_id,
    );
    invoke_signed(
        &ix,
        &[payer.clone(), new_account.clone(), system_program.clone()],
        &[seeds],
    )
}

fn launch_pda(program_id: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_LAUNCH, mint.as_ref()], program_id)
}
fn vault_pda(program_id: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_VAULT, mint.as_ref()], program_id)
}
fn authority_pda(program_id: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_AUTHORITY, mint.as_ref()], program_id)
}
fn reputation_pda(program_id: &Pubkey, creator: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_REPUTATION, creator.as_ref()], program_id)
}

fn load_reputation(account: &AccountInfo) -> Result<CreatorReputation, ProgramError> {
    let data = account.try_borrow_data()?;
    if data.is_empty() || data[0] != DISCRIMINATOR_REPUTATION {
        return Err(ProgramError::InvalidAccountData);
    }
    CreatorReputation::try_from_slice(&data).map_err(|_| ProgramError::InvalidAccountData)
}

fn save_reputation(account: &AccountInfo, reputation: &CreatorReputation) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(reputation).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

// ---------------------------------------------------------------------------
// RegisterAndLock
// ---------------------------------------------------------------------------

fn process_register(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    lock_amount: u64,
    lock_slots: u64,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let creator = next_account_info(iter)?;
    let mint_account = next_account_info(iter)?;
    let creator_token = next_account_info(iter)?;
    let record_account = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let authority_account = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;
    let system_program = next_account_info(iter)?;
    let reputation_account = next_account_info(iter)?;

    if !creator.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // Classic SPL Token only (see LaunchError::UnsupportedTokenProgram):
    // the token program passed in must be the classic one, and the
    // mint account must be owned by it. A Token-2022 mint's base layout
    // would otherwise parse while its extensions stayed invisible.
    if token_program.key != &spl_token::id()
        || mint_account.owner != &spl_token::id()
        || creator_token.owner != &spl_token::id()
    {
        return Err(LaunchError::UnsupportedTokenProgram.into());
    }

    // PDAs must be exactly the derived addresses.
    let (record_pda, record_bump) = launch_pda(program_id, mint_account.key);
    let (vault_pda, vault_bump) = vault_pda(program_id, mint_account.key);
    let (authority_pda, _) = authority_pda(program_id, mint_account.key);
    if record_account.key != &record_pda
        || vault_account.key != &vault_pda
        || authority_account.key != &authority_pda
    {
        return Err(LaunchError::InvalidPda.into());
    }

    // The anti-rug core: supply must be provably fixed forever.
    let mint = Mint::unpack(&mint_account.try_borrow_data()?)?;
    if mint.mint_authority.is_some() {
        return Err(LaunchError::MintAuthorityNotRevoked.into());
    }
    if mint.freeze_authority.is_some() {
        return Err(LaunchError::FreezeAuthorityNotRevoked.into());
    }
    if mint.supply == 0 {
        return Err(LaunchError::ZeroSupply.into());
    }

    // Lock terms.
    if lock_slots < MIN_LOCK_SLOTS {
        return Err(LaunchError::LockPeriodTooShort.into());
    }
    if lock_slots > MAX_LOCK_SLOTS {
        return Err(LaunchError::LockPeriodTooLong.into());
    }
    let min_lock = (mint.supply as u128) * (MIN_LOCK_BPS as u128) / (BPS_DENOMINATOR as u128);
    if (lock_amount as u128) < min_lock || lock_amount == 0 {
        return Err(LaunchError::LockTooSmall.into());
    }

    // Creator's source account must be theirs, of this mint, and funded.
    let creator_token_data = TokenAccount::unpack(&creator_token.try_borrow_data()?)?;
    if creator_token_data.owner != *creator.key || creator_token_data.mint != *mint_account.key {
        return Err(LaunchError::MintMismatch.into());
    }
    if creator_token_data.amount < lock_amount {
        return Err(ProgramError::InsufficientFunds);
    }

    // Create the launch record (fails if the mint is already registered).
    create_pda_account(
        creator,
        record_account,
        system_program,
        program_id,
        LaunchRecord::LEN,
        &[SEED_LAUNCH, mint_account.key.as_ref(), &[record_bump]],
    )?;

    // Create the vault token account, owned by the authority PDA.
    let token_space = TokenAccount::LEN;
    let rent = Rent::get()?;
    let vault_ix = system_instruction::create_account(
        creator.key,
        vault_account.key,
        rent.minimum_balance(token_space),
        token_space as u64,
        &spl_token::id(),
    );
    invoke_signed(
        &vault_ix,
        &[creator.clone(), vault_account.clone(), system_program.clone()],
        &[&[SEED_VAULT, mint_account.key.as_ref(), &[vault_bump]]],
    )?;
    invoke(
        &initialize_account3(
            token_program.key,
            vault_account.key,
            mint_account.key,
            &authority_pda,
        )?,
        &[
            vault_account.clone(),
            mint_account.clone(),
            authority_account.clone(),
            token_program.clone(),
        ],
    )?;

    // Move the locked tokens from the creator into the vault.
    invoke(
        &transfer(
            token_program.key,
            creator_token.key,
            vault_account.key,
            creator.key,
            &[],
            lock_amount,
        )?,
        &[
            creator_token.clone(),
            vault_account.clone(),
            creator.clone(),
            token_program.clone(),
        ],
    )?;

    let clock = Clock::get()?;
    let record = LaunchRecord {
        discriminator: DISCRIMINATOR_LAUNCH,
        mint: *mint_account.key,
        creator: *creator.key,
        vault: *vault_account.key,
        total_supply: mint.supply,
        locked_amount: lock_amount,
        lock_until_slot: clock.slot + lock_slots,
        registered_slot: clock.slot,
        state: STATE_LOCKED,
    };
    save_record(record_account, &record)?;

    // Reputation: create the creator's PDA on their first launch,
    // load it on later ones, and count this registration. Runs
    // after the record exists, so a rejected registration can
    // never inflate the count (the transaction would roll back
    // regardless — account writes are atomic with the rest).
    let (rep_pda, rep_bump) = reputation_pda(program_id, creator.key);
    if reputation_account.key != &rep_pda {
        return Err(LaunchError::InvalidPda.into());
    }
    let mut reputation = if reputation_account.owner == program_id {
        load_reputation(reputation_account)?
    } else {
        create_pda_account(
            creator,
            reputation_account,
            system_program,
            program_id,
            CreatorReputation::LEN,
            &[SEED_REPUTATION, creator.key.as_ref(), &[rep_bump]],
        )?;
        CreatorReputation {
            discriminator: DISCRIMINATOR_REPUTATION,
            creator: *creator.key,
            launches_registered: 0,
            locks_completed: 0,
            first_launch_slot: clock.slot,
            last_completed_slot: 0,
        }
    };
    if reputation.creator != *creator.key {
        return Err(ProgramError::InvalidAccountData);
    }
    reputation.launches_registered = reputation
        .launches_registered
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    save_reputation(reputation_account, &reputation)?;

    msg!(
        "LaunchGuard: registered mint {} — {} of {} locked until {}",
        record.mint,
        record.locked_amount,
        record.total_supply,
        record.lock_until_slot
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// ExtendLock — the only direction a lock can move is later.
// ---------------------------------------------------------------------------

fn process_extend(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    additional_slots: u64,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let creator = next_account_info(iter)?;
    let record_account = next_account_info(iter)?;

    if !creator.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let mut record = load_record(record_account)?;
    if record.creator != *creator.key {
        return Err(LaunchError::NotCreator.into());
    }
    if record.state != STATE_LOCKED {
        return Err(LaunchError::AlreadyReleased.into());
    }
    if additional_slots == 0 {
        return Err(LaunchError::LockPeriodTooShort.into());
    }
    let clock = Clock::get()?;
    let new_until = record
        .lock_until_slot
        .checked_add(additional_slots)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if new_until > clock.slot + MAX_LOCK_SLOTS {
        return Err(LaunchError::LockPeriodTooLong.into());
    }
    record.lock_until_slot = new_until;
    save_record(record_account, &record)?;
    msg!("LaunchGuard: lock for {} extended to slot {}", record.mint, new_until);
    let _ = program_id;
    Ok(())
}

// ---------------------------------------------------------------------------
// Release — creator only, only after expiry. No other path moves funds.
// ---------------------------------------------------------------------------

fn process_release(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let iter = &mut accounts.iter();
    let creator = next_account_info(iter)?;
    let record_account = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let creator_token = next_account_info(iter)?;
    let authority_account = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;
    let reputation_account = next_account_info(iter)?;

    if !creator.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if token_program.key != &spl_token::id() {
        return Err(LaunchError::UnsupportedTokenProgram.into());
    }
    let mut record = load_record(record_account)?;
    if record.creator != *creator.key {
        return Err(LaunchError::NotCreator.into());
    }
    if record.state != STATE_LOCKED {
        return Err(LaunchError::AlreadyReleased.into());
    }
    if vault_account.key != &record.vault {
        return Err(LaunchError::InvalidPda.into());
    }
    let (authority_pda, authority_bump) = authority_pda(program_id, &record.mint);
    if authority_account.key != &authority_pda {
        return Err(LaunchError::InvalidPda.into());
    }
    let clock = Clock::get()?;
    if clock.slot < record.lock_until_slot {
        return Err(LaunchError::StillLocked.into());
    }
    let creator_token_data = TokenAccount::unpack(&creator_token.try_borrow_data()?)?;
    if creator_token_data.owner != *creator.key || creator_token_data.mint != record.mint {
        return Err(LaunchError::MintMismatch.into());
    }

    invoke_signed(
        &transfer(
            token_program.key,
            vault_account.key,
            creator_token.key,
            &authority_pda,
            &[],
            record.locked_amount,
        )?,
        &[
            vault_account.clone(),
            creator_token.clone(),
            authority_account.clone(),
            token_program.clone(),
        ],
        &[&[SEED_AUTHORITY, record.mint.as_ref(), &[authority_bump]]],
    )?;

    record.state = STATE_RELEASED;
    save_record(record_account, &record)?;

    // Reputation: this release was only reachable after the full
    // lock period elapsed, so it counts as a completed lock. The
    // PDA was created by this creator's registration and must
    // exist; a missing or foreign account fails the release
    // rather than silently skipping the record.
    let (rep_pda, _) = reputation_pda(program_id, creator.key);
    if reputation_account.key != &rep_pda || reputation_account.owner != program_id {
        return Err(LaunchError::InvalidPda.into());
    }
    let mut reputation = load_reputation(reputation_account)?;
    if reputation.creator != *creator.key {
        return Err(ProgramError::InvalidAccountData);
    }
    reputation.locks_completed = reputation
        .locks_completed
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    reputation.last_completed_slot = clock.slot;
    save_reputation(reputation_account, &reputation)?;

    msg!(
        "LaunchGuard: released {} of {} to creator",
        record.locked_amount,
        record.mint
    );
    Ok(())
}
