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
    instruction::{close_account, initialize_account3, transfer},
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
        LaunchInstruction::CloseVault {} => process_close_vault(program_id, accounts),
        LaunchInstruction::Initialize { min_lock_amount } => {
            process_initialize(program_id, accounts, min_lock_amount)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_record(account: &AccountInfo, program_id: &Pubkey) -> Result<LaunchRecord, ProgramError> {
    // The record must be one of ours. An account owned by another
    // program can carry attacker-chosen bytes — a valid discriminator
    // and a forged mint/vault/creator/lock — and without this check
    // the release path would trust it until the runtime rejected the
    // write at the end of the instruction. Fail here, before any work.
    if account.owner != program_id {
        return Err(ProgramError::InvalidAccountData);
    }
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
    create_or_adopt_account(
        payer,
        new_account,
        system_program,
        program_id,
        space,
        seeds,
    )
}

/// Create a PDA-owned account, adopting a pre-funded shell if one
/// exists.
///
/// Self-audit F-2 (2026-11-16): the record, vault, and reputation
/// PDAs are deterministic, so anyone could send 1 lamport to one
/// before its registration and permanently block that mint's (or
/// creator's) LaunchGuard registration — plain `create_account`
/// refuses an account that already holds lamports, and there was no
/// recovery path. Create-or-adopt removes the grief: a pre-existing
/// account is adopted only if it is a pristine shell (lamports > 0,
/// owned by the system program, no data) — the only state a bare
/// transfer can produce. It is topped up to rent-exempt if needed,
/// then allocated and assigned with the PDA's own signature,
/// exactly as if created fresh. Any other pre-existing state still
/// errors.
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
fn config_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_CONFIG], program_id)
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
        return Err(LaunchError::NotUpgradeAuthority.into());
    }
    Ok(())
}

fn load_config(account: &AccountInfo, program_id: &Pubkey) -> Result<GuardConfig, ProgramError> {
    // An account the program does not own at the config PDA is a
    // not-yet-created config (system-owned shell or nothing at
    // all): Initialize has not run, and registration fails closed
    // rather than skipping the floor check.
    if account.owner != program_id {
        return Err(LaunchError::ConfigNotInitialized.into());
    }
    let data = account.try_borrow_data()?;
    if data.is_empty() || data[0] != DISCRIMINATOR_CONFIG {
        return Err(ProgramError::InvalidAccountData);
    }
    GuardConfig::try_from_slice(&data).map_err(|_| ProgramError::InvalidAccountData)
}

fn save_config(account: &AccountInfo, config: &GuardConfig) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(config).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
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
// Initialize — one-time setup. Creates the config account carrying
// the dust floor. Bound to the program's upgrade authority (proven
// against ProgramData, the bridge F-1 pattern): the floor is a
// deployment decision, and the upgrade authority is the key that
// already controls the program binary at the loader level, so this
// grants it no power it does not structurally have. What it does
// NOT create is an admin: after this one call there is no
// instruction that changes the floor, and the authority has no
// path to any launch, vault, or reputation account.
// ---------------------------------------------------------------------------

fn process_initialize(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    min_lock_amount: u64,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let authority = next_account_info(iter)?;
    let config_account = next_account_info(iter)?;
    let programdata_account = next_account_info(iter)?;
    let system_program = next_account_info(iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (config_pda, config_bump) = config_pda(program_id);
    if config_account.key != &config_pda {
        return Err(LaunchError::InvalidPda.into());
    }
    // Exactly once. A program-owned config account means the floor
    // is already set — refuse, whatever bytes it carries, rather
    // than letting a second call reinterpret them.
    if config_account.owner == program_id {
        return Err(LaunchError::ConfigAlreadyInitialized.into());
    }
    verify_upgrade_authority(program_id, programdata_account, authority.key)?;
    // Create-or-adopt like every other PDA here (self-audit F-2):
    // the config address is just as predictable as the record's,
    // and a dust pre-fund must not be able to brick setup forever.
    create_pda_account(
        authority,
        config_account,
        system_program,
        program_id,
        GuardConfig::LEN,
        &[SEED_CONFIG, &[config_bump]],
    )?;
    save_config(
        config_account,
        &GuardConfig {
            discriminator: DISCRIMINATOR_CONFIG,
            min_lock_amount,
        },
    )?;
    msg!(
        "LaunchGuard: initialized — dust floor {} raw units",
        min_lock_amount
    );
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
    let config_account = next_account_info(iter)?;

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

    // The dust floor, from the config Initialize created. Loaded
    // before any account is created: an uninitialized config fails
    // the registration closed, and a wrong address is InvalidPda —
    // there is no account a registrant can substitute to skip it.
    let (config_pda, _) = config_pda(program_id);
    if config_account.key != &config_pda {
        return Err(LaunchError::InvalidPda.into());
    }
    let config = load_config(config_account, program_id)?;

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
    // The 10% floor rounds UP (ceiling division, self-audit F-4):
    // floor division let a 19-token supply register with a 1-token
    // lock — 5.26%, below the advertised minimum. A floor that
    // protects the protocol must never round in the user's favor.
    // u128 throughout: supply * MIN_LOCK_BPS (1,000) plus the
    // BPS_DENOMINATOR - 1 (9,999) ceiling term cannot overflow.
    let min_lock = ((mint.supply as u128) * (MIN_LOCK_BPS as u128)
        + (BPS_DENOMINATOR as u128 - 1))
        / (BPS_DENOMINATOR as u128);
    if (lock_amount as u128) < min_lock || lock_amount == 0 {
        return Err(LaunchError::LockTooSmall.into());
    }
    // The dust floor binds on top of the share: the effective
    // minimum is max(10% of supply rounded up, floor). Checked
    // after the share so a lock failing both is reported as the
    // share failure it primarily is, and a lock passing the share
    // but failing the floor names the floor.
    if lock_amount < config.min_lock_amount {
        return Err(LaunchError::LockBelowDustFloor.into());
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
    // Create-or-adopt like every other PDA here (self-audit F-2):
    // the vault address is just as predictable as the record's.
    create_or_adopt_account(
        creator,
        vault_account,
        system_program,
        &spl_token::id(),
        TokenAccount::LEN,
        &[SEED_VAULT, mint_account.key.as_ref(), &[vault_bump]],
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
    let mut record = load_record(record_account, program_id)?;
    // The record for a mint lives only at its derived PDA: a
    // program-owned account holding a valid record for some other
    // address is not this mint's record.
    let (record_pda, _) = launch_pda(program_id, &record.mint);
    if record_account.key != &record_pda {
        return Err(LaunchError::InvalidPda.into());
    }
    if record.creator != *creator.key {
        return Err(LaunchError::NotCreator.into());
    }
    if record.state != STATE_LOCKED {
        return Err(LaunchError::AlreadyReleased.into());
    }
    if additional_slots == 0 {
        return Err(LaunchError::LockPeriodTooShort.into());
    }
    let new_until = record
        .lock_until_slot
        .checked_add(additional_slots)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    // The 5-year maximum is a maximum lock PERIOD, measured from
    // registration — the same basis RegisterAndLock applies to the
    // initial lock_slots. Capping against clock.slot + MAX instead
    // made it a sliding window: every extension re-opened a fresh
    // 5 years, so the total duration was unbounded (code-review
    // debate round 3, 2026-11-18; reproduced before the fix: a
    // max-length lock extended by 1 slot after a 1,000-slot warp
    // reached MAX_LOCK_SLOTS + 1 total).
    let max_until = record
        .registered_slot
        .checked_add(MAX_LOCK_SLOTS)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if new_until > max_until {
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
    let mut record = load_record(record_account, program_id)?;
    // The record for a mint lives only at its derived PDA. Without
    // this, a program-owned account holding a valid-looking record
    // for another address could stand in for this mint's record.
    let (record_pda, _) = launch_pda(program_id, &record.mint);
    if record_account.key != &record_pda {
        return Err(LaunchError::InvalidPda.into());
    }
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

// ---------------------------------------------------------------------------
// CloseVault — creator only, only after Release. Sweeps whatever extra
// tokens were sent into the vault (Release moves exactly
// locked_amount) back to the creator and closes the vault account,
// returning its rent. The Released-state gate is the whole safety
// argument: Release sets that state only after the full locked
// amount has left the vault, so nothing reachable from here can be
// part of the lock, and while a launch is still locked this
// instruction always fails.
// ---------------------------------------------------------------------------

fn process_close_vault(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let iter = &mut accounts.iter();
    let creator = next_account_info(iter)?;
    let record_account = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let creator_token = next_account_info(iter)?;
    let authority_account = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;

    if !creator.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if token_program.key != &spl_token::id() {
        return Err(LaunchError::UnsupportedTokenProgram.into());
    }
    let record = load_record(record_account, program_id)?;
    // Same binding as ExtendLock and Release: the record for a mint
    // lives only at its derived PDA.
    let (record_pda, _) = launch_pda(program_id, &record.mint);
    if record_account.key != &record_pda {
        return Err(LaunchError::InvalidPda.into());
    }
    if record.creator != *creator.key {
        return Err(LaunchError::NotCreator.into());
    }
    if record.state != STATE_RELEASED {
        return Err(LaunchError::NotReleased.into());
    }
    if vault_account.key != &record.vault {
        return Err(LaunchError::InvalidPda.into());
    }
    let (authority_pda, authority_bump) = authority_pda(program_id, &record.mint);
    if authority_account.key != &authority_pda {
        return Err(LaunchError::InvalidPda.into());
    }
    let creator_token_data = TokenAccount::unpack(&creator_token.try_borrow_data()?)?;
    if creator_token_data.owner != *creator.key || creator_token_data.mint != record.mint {
        return Err(LaunchError::MintMismatch.into());
    }
    // The vault must still be the token account registration created:
    // this mint, owned by the authority PDA. Anything else at that
    // address is not ours to sweep or close.
    let vault_data = TokenAccount::unpack(&vault_account.try_borrow_data()?)?;
    if vault_data.mint != record.mint || vault_data.owner != authority_pda {
        return Err(ProgramError::InvalidAccountData);
    }
    let residual = vault_data.amount;
    let seeds: &[&[u8]] = &[SEED_AUTHORITY, record.mint.as_ref(), &[authority_bump]];
    if residual > 0 {
        invoke_signed(
            &transfer(
                token_program.key,
                vault_account.key,
                creator_token.key,
                &authority_pda,
                &[],
                residual,
            )?,
            &[
                vault_account.clone(),
                creator_token.clone(),
                authority_account.clone(),
                token_program.clone(),
            ],
            &[seeds],
        )?;
    }
    invoke_signed(
        &close_account(
            token_program.key,
            vault_account.key,
            creator.key,
            &authority_pda,
            &[],
        )?,
        &[
            vault_account.clone(),
            creator.clone(),
            authority_account.clone(),
            token_program.clone(),
        ],
        &[seeds],
    )?;

    msg!(
        "LaunchGuard: vault for {} closed — {} residual tokens returned to creator",
        record.mint,
        residual
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests for the mirrored upgrade-authority check. The
// integration suite can only seed a well-formed ProgramData
// account, so the structural rejections — wrong address, wrong
// owner, truncated data, wrong variant, immutable program — are
// pinned here, against this crate's own copy of the check
// (war-room review of the dust floor, 2026-11-19: the mirror in
// this crate must not be able to drift silently from the
// bridge-common original it copies).
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
            Err(LaunchError::NotUpgradeAuthority.into())
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
