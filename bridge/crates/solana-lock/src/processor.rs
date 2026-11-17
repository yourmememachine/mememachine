//! Solana-side lock instruction handlers (BRIDGE.md section 2.1).

use borsh::BorshDeserialize;
use solana_program::program_pack::Pack;
use bridge_common::*;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    msg,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::Sysvar,
    system_instruction,
};
use spl_token::{
    instruction::transfer as token_transfer,
    state::Account as TokenAccount,
};

pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let instruction = LockInstruction::try_from_slice(data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    match instruction {
        LockInstruction::Initialize {
            relayers,
            admins,
            source_mint,
            vault,
            per_tx_cap,
            daily_cap,
            vault_ceiling,
        } => process_initialize(
            program_id,
            accounts,
            relayers,
            admins,
            source_mint,
            vault,
            per_tx_cap,
            daily_cap,
            vault_ceiling,
        ),
        LockInstruction::Deposit {
            amount,
            destination,
            nonce,
        } => process_deposit(program_id, accounts, amount, destination, nonce),
        LockInstruction::Unlock { message } => process_unlock(program_id, accounts, message),
        LockInstruction::Pause => process_pause(program_id, accounts, true),
        LockInstruction::Unpause => process_pause(program_id, accounts, false),
        LockInstruction::SetCap {
            per_tx_cap,
            daily_cap,
        } => process_set_cap(program_id, accounts, per_tx_cap, daily_cap),
        LockInstruction::SetRelayers { relayers } => {
            process_set_relayers(program_id, accounts, relayers)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_config(account: &AccountInfo) -> Result<LockConfig, ProgramError> {
    let data = account.try_borrow_data()?;
    if data.is_empty() || data[0] != DISCRIMINATOR_CONFIG {
        return Err(BridgeError::InvalidAccountData.into());
    }
    LockConfig::try_from_slice(&data).map_err(|_| BridgeError::InvalidAccountData.into())
}

fn save_config(account: &AccountInfo, config: &LockConfig) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(config).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

fn signer_pubkeys(accounts: &[AccountInfo], start: usize) -> Vec<Pubkey> {
    accounts[start..]
        .iter()
        .filter(|a| a.is_signer)
        .map(|a| *a.key)
        .collect()
}

/// Create a program PDA, adopting a pre-funded shell if one exists.
///
/// Self-audit F-2 (2026-11-16): every PDA address here is
/// deterministic, so anyone could send 1 lamport to a nonce, receipt,
/// or config PDA before its crossing/deployment and permanently
/// block it — plain `create_account` refuses an account that already
/// holds lamports, and there was no recovery path. Create-or-adopt
/// removes the grief: a pre-existing account is adopted only if it
/// is a pristine shell (lamports > 0, owned by the system program,
/// no data) — the only state a bare transfer can produce. It is
/// topped up to rent-exempt if needed, then allocated and assigned
/// with the PDA's own signature, exactly as if created fresh. Any
/// other pre-existing state (program-owned, data-carrying, or owned
/// by a third program) still errors.
#[allow(deprecated)]
fn create_pda_account<'a>(
    payer: &AccountInfo<'a>,
    new_account: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
    program_id: &Pubkey,
    space: usize,
    seeds: &[&[u8]],
) -> ProgramResult {
    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(space);
    if new_account.lamports() > 0 {
        if *new_account.owner != solana_program::system_program::ID
            || !new_account.data_is_empty()
        {
            return Err(BridgeError::InvalidAccountData.into());
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
            &system_instruction::assign(new_account.key, program_id),
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
        program_id,
    );
    invoke_signed(
        &ix,
        &[payer.clone(), new_account.clone(), system_program.clone()],
        &[seeds],
    )
}

// ---------------------------------------------------------------------------
// Initialize
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn process_initialize(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    relayers: [Pubkey; MAX_RELAYERS],
    admins: [Pubkey; MAX_ADMINS],
    source_mint: Pubkey,
    vault: Pubkey,
    per_tx_cap: u64,
    daily_cap: u64,
    vault_ceiling: u64,
) -> ProgramResult {
    let mut iter = accounts.iter();
    let payer = next_account_info(&mut iter)?;
    let config_account = next_account_info(&mut iter)?;
    let vault_account = next_account_info(&mut iter)?;
    let system_program = next_account_info(&mut iter)?;
    let programdata_account = next_account_info(&mut iter)?;

    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // Self-audit F-1: initialization is bound to the program's
    // upgrade authority. Without this, the first caller after
    // deployment — anyone — set the relayer set, the admin multisig,
    // and the caps.
    verify_upgrade_authority(program_id, programdata_account, payer.key)?;
    let (config_pda, bump) = config_pda(program_id);
    if *config_account.key != config_pda {
        return Err(BridgeError::InvalidAccountData.into());
    }
    if config_account.owner == program_id {
        return Err(BridgeError::AccountAlreadyInitialized.into());
    }
    if *vault_account.key != vault {
        return Err(BridgeError::InvalidAccountData.into());
    }
    // The vault must be a token account owned by the vault-authority PDA.
    // The vault is created by the deployer before initialization; the
    // program only verifies ownership, it never takes custody from a key.
    let (vault_authority, vault_authority_bump) = authority_pda(program_id);
    let vault_state = TokenAccount::unpack(&vault_account.try_borrow_data()?)?;
    if vault_state.owner != vault_authority || vault_state.mint != source_mint {
        return Err(BridgeError::InvalidAccountData.into());
    }
    // Self-audit F-3: the same member-set validation SetRelayers
    // applies — no default keys, no duplicates, for either set.
    validate_member_set(&relayers)?;
    validate_member_set(&admins)?;
    if per_tx_cap == 0 || daily_cap == 0 || daily_cap < per_tx_cap || vault_ceiling == 0 {
        return Err(BridgeError::InvalidAmount.into());
    }

    create_pda_account(
        payer,
        config_account,
        system_program,
        program_id,
        LockConfig::LEN,
        &[SEED_CONFIG, &[bump]],
    )?;

    let config = LockConfig::new(
        relayers,
        admins,
        source_mint,
        vault,
        vault_authority_bump,
        per_tx_cap,
        daily_cap,
        vault_ceiling,
        bump,
    );
    save_config(config_account, &config)?;
    msg!("Lock program initialized; vault {}", vault);
    Ok(())
}

// ---------------------------------------------------------------------------
// Deposit
// ---------------------------------------------------------------------------

fn process_deposit(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    amount: u64,
    destination: Pubkey,
    nonce: u64,
) -> ProgramResult {
    let mut iter = accounts.iter();
    let config_account = next_account_info(&mut iter)?;
    let user_token_account = next_account_info(&mut iter)?;
    let vault_account = next_account_info(&mut iter)?;
    let record_account = next_account_info(&mut iter)?;
    let user = next_account_info(&mut iter)?;
    let payer = next_account_info(&mut iter)?;
    let token_program = next_account_info(&mut iter)?;
    let system_program = next_account_info(&mut iter)?;

    if *config_account.owner != *program_id {
        return Err(BridgeError::InvalidAccountData.into());
    }
    // Pin the token program (self-audit K-1 hardening): substitution
    // already failed inside spl-token's checked CPI builders, but
    // that safety rested on a dependency's behavior — check it here,
    // explicitly, the way LaunchGuard does.
    if token_program.key != &spl_token::id() {
        return Err(BridgeError::InvalidAccountData.into());
    }
    let mut config = load_config(config_account)?;
    config.check_active()?;
    if amount == 0 {
        return Err(BridgeError::InvalidAmount.into());
    }
    if !user.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if *vault_account.key != config.vault {
        return Err(BridgeError::InvalidAccountData.into());
    }

    // Vault ceiling (BRIDGE.md 4.4): refuse deposits past the ceiling.
    let new_total = config
        .total_locked
        .checked_add(amount)
        .ok_or(BridgeError::ArithmeticOverflow)?;
    if new_total > config.vault_ceiling {
        return Err(BridgeError::VaultCeilingExceeded.into());
    }

    let record_bump = check_nonce_unprocessed(
        record_account,
        program_id,
        DIR_SOLANA_TO_MACHINE,
        nonce,
    )?;

    let user_state = TokenAccount::unpack(&user_token_account.try_borrow_data()?)?;
    if user_state.owner != *user.key || user_state.mint != config.source_mint {
        return Err(BridgeError::Unauthorized.into());
    }

    let transfer_ix = token_transfer(
        token_program.key,
        user_token_account.key,
        vault_account.key,
        user.key,
        &[],
        amount,
    )?;
    invoke(
        &transfer_ix,
        &[
            user_token_account.clone(),
            vault_account.clone(),
            user.clone(),
            token_program.clone(),
        ],
    )?;

    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    create_pda_account(
        payer,
        record_account,
        system_program,
        program_id,
        EventRecord::LEN,
        &[SEED_NONCE, &[DIR_SOLANA_TO_MACHINE], &nonce.to_le_bytes(), &[record_bump]],
    )?;
    let clock = Clock::get()?;
    let record = EventRecord {
        discriminator: DISCRIMINATOR_EVENT_RECORD,
        nonce,
        amount,
        destination,
        slot: clock.slot,
    };
    let mut data = record_account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(&record).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    drop(data);

    config.total_locked = new_total;
    save_config(config_account, &config)?;
    msg!(
        "Deposit nonce={} amount={} destination={} slot={} total_locked={}",
        nonce,
        amount,
        destination,
        clock.slot,
        new_total
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Unlock
// ---------------------------------------------------------------------------

fn process_unlock(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    message: BridgeMessage,
) -> ProgramResult {
    let mut iter = accounts.iter();
    let config_account = next_account_info(&mut iter)?;
    let vault_account = next_account_info(&mut iter)?;
    let destination_account = next_account_info(&mut iter)?;
    let nonce_account = next_account_info(&mut iter)?;
    let authority_account = next_account_info(&mut iter)?;
    let payer = next_account_info(&mut iter)?;
    let token_program = next_account_info(&mut iter)?;
    let system_program = next_account_info(&mut iter)?;
    let instructions_sysvar = next_account_info(&mut iter)?;
    let receipt_account = next_account_info(&mut iter)?;

    if *config_account.owner != *program_id {
        return Err(BridgeError::InvalidAccountData.into());
    }
    // Token program pinned — see process_deposit (self-audit K-1).
    if token_program.key != &spl_token::id() {
        return Err(BridgeError::InvalidAccountData.into());
    }
    let mut config = load_config(config_account)?;
    config.check_active()?;

    let (expected_config, _) = config_pda(program_id);
    if *config_account.key != expected_config {
        return Err(BridgeError::InvalidAccountData.into());
    }
    if *vault_account.key != config.vault {
        return Err(BridgeError::InvalidAccountData.into());
    }
    let (authority_pda, authority_bump) = authority_pda(program_id);
    if *authority_account.key != authority_pda {
        return Err(BridgeError::InvalidAccountData.into());
    }

    let clock = Clock::get()?;
    validate_message(&message, CHAIN_MACHINE, CHAIN_SOLANA, clock.slot)?;
    verify_precompile_attestation(
        &message,
        &config.relayers.relayers,
        config.relayers.quorum,
        instructions_sysvar,
    )?;

    let nonce_bump = check_nonce_unprocessed(
        nonce_account,
        program_id,
        DIR_MACHINE_TO_SOLANA,
        message.nonce,
    )?;

    let dest_state = TokenAccount::unpack(&destination_account.try_borrow_data()?)?;
    if dest_state.owner != message.recipient {
        return Err(BridgeError::Unauthorized.into());
    }
    if dest_state.mint != config.source_mint {
        return Err(BridgeError::InvalidAccountData.into());
    }
    if message.amount > config.total_locked {
        return Err(BridgeError::InvalidAmount.into());
    }

    config.daily_unlocked = config.daily_unlocked.check_and_record(
        message.amount,
        config.per_tx_cap,
        config.daily_cap,
        clock.slot,
    )?;
    // Dynamic percentage layer: at most 1% of the vault's locked total
    // per unlock, at most 5% per window — or the tightened limits
    // (a quarter of those) while the post-unpause recovery window is
    // running. `total_locked` here is the pre-unlock total, which
    // includes this amount.
    let (per_tx_bps, daily_bps) = config.dynamic_bps(clock.slot);
    check_dynamic_caps_with_bps(
        message.amount,
        config.daily_unlocked.used,
        config.total_locked,
        per_tx_bps,
        daily_bps,
    )?;

    let transfer_ix = token_transfer(
        token_program.key,
        vault_account.key,
        destination_account.key,
        authority_account.key,
        &[],
        message.amount,
    )?;
    invoke_signed(
        &transfer_ix,
        &[
            vault_account.clone(),
            destination_account.clone(),
            authority_account.clone(),
            token_program.clone(),
        ],
        &[&[SEED_AUTHORITY, &[authority_bump]]],
    )?;

    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    create_pda_account(
        payer,
        nonce_account,
        system_program,
        program_id,
        NonceRecord::LEN,
        &[SEED_NONCE, &[DIR_MACHINE_TO_SOLANA], &message.nonce.to_le_bytes(), &[nonce_bump]],
    )?;
    let record = NonceRecord {
        discriminator: DISCRIMINATOR_NONCE_RECORD,
        nonce: message.nonce,
        source_tx: message.source_tx,
        amount: message.amount,
        slot: clock.slot,
    };
    let mut data = nonce_account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(&record).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    drop(data);

    // Crossing receipt, the mirror of the Machine side's: permanent
    // public proof this unlock completed, in the same transaction.
    let (expected_receipt, receipt_bump) =
        receipt_pda(program_id, DIR_MACHINE_TO_SOLANA, message.nonce);
    if *receipt_account.key != expected_receipt {
        return Err(BridgeError::InvalidAccountData.into());
    }
    create_pda_account(
        payer,
        receipt_account,
        system_program,
        program_id,
        CrossingReceipt::LEN,
        &[SEED_RECEIPT, &[DIR_MACHINE_TO_SOLANA], &message.nonce.to_le_bytes(), &[receipt_bump]],
    )?;
    let receipt = CrossingReceipt {
        discriminator: DISCRIMINATOR_RECEIPT,
        direction: DIR_MACHINE_TO_SOLANA,
        nonce: message.nonce,
        message_hash: message.hash(),
        source_tx: message.source_tx,
        amount: message.amount,
        recipient: message.recipient,
        slot: clock.slot,
        timestamp: clock.unix_timestamp,
    };
    let mut data = receipt_account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(&receipt).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    drop(data);

    config.total_locked = config
        .total_locked
        .checked_sub(message.amount)
        .ok_or(BridgeError::ArithmeticOverflow)?;
    save_config(config_account, &config)?;
    msg!(
        "Unlock nonce={} amount={} recipient={} slot={} total_locked={}",
        message.nonce,
        message.amount,
        message.recipient,
        clock.slot,
        config.total_locked
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin instructions
// ---------------------------------------------------------------------------

fn process_pause(program_id: &Pubkey, accounts: &[AccountInfo], pause: bool) -> ProgramResult {
    let mut iter = accounts.iter();
    let config_account = next_account_info(&mut iter)?;
    let signers = signer_pubkeys(&accounts[1..], 0);
    if *config_account.owner != *program_id {
        return Err(BridgeError::InvalidAccountData.into());
    }
    let mut config = load_config(config_account)?;
    if pause {
        verify_pause_authority(
            &signers,
            &config.admins.admins,
            config.admins.threshold,
            &config.relayers.relayers,
        )?;
        config.paused = true;
        msg!("Lock program paused");
    } else {
        // Unpause is multisig-only (BRIDGE.md 4.3). Every unpause
        // also starts the tightened-cap recovery window: whatever
        // caused the pause, the bridge reopens at a quarter of its
        // normal dynamic limits for one full cap window and then
        // relaxes on its own — no second decision to forget.
        verify_admins(&signers, &config.admins.admins, config.admins.threshold)?;
        config.paused = false;
        let now = Clock::get()?.slot;
        config.tightened_until_slot = now + TIGHTENING_WINDOW_SLOTS;
        msg!(
            "Lock program unpaused; dynamic caps tightened until slot {}",
            config.tightened_until_slot
        );
    }
    save_config(config_account, &config)?;
    Ok(())
}

fn process_set_cap(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    per_tx_cap: u64,
    daily_cap: u64,
) -> ProgramResult {
    let mut iter = accounts.iter();
    let config_account = next_account_info(&mut iter)?;
    let signers = signer_pubkeys(&accounts[1..], 0);
    if *config_account.owner != *program_id {
        return Err(BridgeError::InvalidAccountData.into());
    }
    let mut config = load_config(config_account)?;
    verify_admins(&signers, &config.admins.admins, config.admins.threshold)?;
    if per_tx_cap == 0 || daily_cap == 0 || daily_cap < per_tx_cap {
        return Err(BridgeError::InvalidAmount.into());
    }
    config.per_tx_cap = per_tx_cap;
    config.daily_cap = daily_cap;
    save_config(config_account, &config)?;
    msg!("Caps updated: per-tx={} daily={}", per_tx_cap, daily_cap);
    Ok(())
}

fn process_set_relayers(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    relayers: [Pubkey; MAX_RELAYERS],
) -> ProgramResult {
    let mut iter = accounts.iter();
    let config_account = next_account_info(&mut iter)?;
    let signers = signer_pubkeys(&accounts[1..], 0);
    if *config_account.owner != *program_id {
        return Err(BridgeError::InvalidAccountData.into());
    }
    let mut config = load_config(config_account)?;
    verify_admins(&signers, &config.admins.admins, config.admins.threshold)?;
    // Shared with Initialize (self-audit F-3): identical checks,
    // identical errors.
    validate_member_set(&relayers)?;
    config.relayers.relayers = relayers;
    save_config(config_account, &config)?;
    msg!("Relayer set updated");
    Ok(())
}
