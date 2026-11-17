//! Machine-side bridge instruction handlers (BRIDGE.md section 2.2).

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
    instruction::{burn as token_burn, mint_to},
    state::{Account as TokenAccount, Mint},
};

pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let instruction = MachineInstruction::try_from_slice(data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    match instruction {
        MachineInstruction::Initialize {
            relayers,
            admins,
            wrapped_mint,
            per_tx_cap,
            daily_cap,
        } => process_initialize(program_id, accounts, relayers, admins, wrapped_mint, per_tx_cap, daily_cap),
        MachineInstruction::MintWrapped { message } => {
            process_mint_wrapped(program_id, accounts, message)
        }
        MachineInstruction::Burn {
            amount,
            destination,
            nonce,
        } => process_burn(program_id, accounts, amount, destination, nonce),
        MachineInstruction::Pause => process_pause(program_id, accounts, true),
        MachineInstruction::Unpause => process_pause(program_id, accounts, false),
        MachineInstruction::SetCap {
            per_tx_cap,
            daily_cap,
        } => process_set_cap(program_id, accounts, per_tx_cap, daily_cap),
        MachineInstruction::SetRelayers { relayers } => {
            process_set_relayers(program_id, accounts, relayers)
        }
        MachineInstruction::KillSwitch => process_kill_switch(program_id, accounts),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_config(account: &AccountInfo) -> Result<MachineConfig, ProgramError> {
    let data = account.try_borrow_data()?;
    if data.is_empty() || data[0] != DISCRIMINATOR_CONFIG {
        return Err(BridgeError::InvalidAccountData.into());
    }
    MachineConfig::try_from_slice(&data).map_err(|_| BridgeError::InvalidAccountData.into())
}

fn save_config(account: &AccountInfo, config: &MachineConfig) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(config).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

/// Collect the pubkeys of all signer accounts after `start`.
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

fn process_initialize(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    relayers: [Pubkey; MAX_RELAYERS],
    admins: [Pubkey; MAX_ADMINS],
    wrapped_mint: Pubkey,
    per_tx_cap: u64,
    daily_cap: u64,
) -> ProgramResult {
    let mut iter = accounts.iter();
    let payer = next_account_info(&mut iter)?;
    let config_account = next_account_info(&mut iter)?;
    let wrapped_mint_account = next_account_info(&mut iter)?;
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
    // Self-audit F-1 (Machine side): the wrapped mint previously
    // came from instruction data with no account validation at all —
    // an initializer could point the bridge at a mint whose mint
    // authority they personally hold. Require the actual account:
    // the key must match, it must be owned by the token program,
    // and it must parse as an initialized mint.
    if *wrapped_mint_account.key != wrapped_mint
        || wrapped_mint_account.owner != &spl_token::id()
    {
        return Err(BridgeError::InvalidAccountData.into());
    }
    Mint::unpack(&wrapped_mint_account.try_borrow_data()?)?;
    // Self-audit F-3: the same member-set validation SetRelayers
    // applies — no default keys, no duplicates, for either set.
    validate_member_set(&relayers)?;
    validate_member_set(&admins)?;
    if per_tx_cap == 0 || daily_cap == 0 || daily_cap < per_tx_cap {
        return Err(BridgeError::InvalidAmount.into());
    }

    create_pda_account(
        payer,
        config_account,
        system_program,
        program_id,
        MachineConfig::LEN,
        &[SEED_CONFIG, &[bump]],
    )?;

    let config = MachineConfig::new(relayers, admins, wrapped_mint, per_tx_cap, daily_cap, bump);
    save_config(config_account, &config)?;
    msg!("Machine bridge initialized; wrapped mint {}", wrapped_mint);
    Ok(())
}

// ---------------------------------------------------------------------------
// MintWrapped
// ---------------------------------------------------------------------------

fn process_mint_wrapped(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    message: BridgeMessage,
) -> ProgramResult {
    let mut iter = accounts.iter();
    let config_account = next_account_info(&mut iter)?;
    let nonce_account = next_account_info(&mut iter)?;
    let mint_account = next_account_info(&mut iter)?;
    let destination_account = next_account_info(&mut iter)?;
    let authority_account = next_account_info(&mut iter)?;
    let payer = next_account_info(&mut iter)?;
    let token_program = next_account_info(&mut iter)?;
    let system_program = next_account_info(&mut iter)?;
    let instructions_sysvar = next_account_info(&mut iter)?;
    let receipt_account = next_account_info(&mut iter)?;

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
    config.check_mint_allowed()?;

    let (expected_config, _) = config_pda(program_id);
    if *config_account.key != expected_config {
        return Err(BridgeError::InvalidAccountData.into());
    }
    if *mint_account.key != config.wrapped_mint {
        return Err(BridgeError::InvalidAccountData.into());
    }
    let (authority_pda, authority_bump) = authority_pda(program_id);
    if *authority_account.key != authority_pda {
        return Err(BridgeError::InvalidAccountData.into());
    }

    let clock = Clock::get()?;
    validate_message(&message, CHAIN_SOLANA, CHAIN_MACHINE, clock.slot)?;
    verify_precompile_attestation(
        &message,
        &config.relayers.relayers,
        config.relayers.quorum,
        instructions_sysvar,
    )?;

    // Replay protection: the nonce PDA must not already exist.
    let nonce_bump = check_nonce_unprocessed(
        nonce_account,
        program_id,
        DIR_SOLANA_TO_MACHINE,
        message.nonce,
    )?;

    // The destination token account must belong to the message recipient
    // and hold the wrapped mint.
    let dest_state = TokenAccount::unpack(&destination_account.try_borrow_data()?)?;
    if dest_state.owner != message.recipient {
        return Err(BridgeError::Unauthorized.into());
    }
    if dest_state.mint != config.wrapped_mint {
        return Err(BridgeError::InvalidAccountData.into());
    }

    // Absolute caps, enforced on-chain (slot-windowed daily total).
    config.daily_minted = config.daily_minted.check_and_record(
        message.amount,
        config.per_tx_cap,
        config.daily_cap,
        clock.slot,
    )?;
    config.total_outstanding = config
        .total_outstanding
        .checked_add(message.amount)
        .ok_or(BridgeError::ArithmeticOverflow)?;

    // Mint via the bridge-authority PDA. This is the ONLY mint path.
    let mint_ix = mint_to(
        token_program.key,
        mint_account.key,
        destination_account.key,
        authority_account.key,
        &[],
        message.amount,
    )?;
    invoke_signed(
        &mint_ix,
        &[
            mint_account.clone(),
            destination_account.clone(),
            authority_account.clone(),
            token_program.clone(),
        ],
        &[&[SEED_AUTHORITY, &[authority_bump]]],
    )?;

    // Mark the nonce processed.
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    create_pda_account(
        payer,
        nonce_account,
        system_program,
        program_id,
        NonceRecord::LEN,
        &[SEED_NONCE, &[DIR_SOLANA_TO_MACHINE], &message.nonce.to_le_bytes(), &[nonce_bump]],
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

    // Crossing receipt: a permanent, public proof this crossing
    // completed, written in the same transaction as the mint. The
    // nonce gate above already guarantees this (direction, nonce)
    // is fresh, so the receipt PDA cannot exist yet; verify the
    // caller passed the right account and create it.
    let (expected_receipt, receipt_bump) =
        receipt_pda(program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    if *receipt_account.key != expected_receipt {
        return Err(BridgeError::InvalidAccountData.into());
    }
    create_pda_account(
        payer,
        receipt_account,
        system_program,
        program_id,
        CrossingReceipt::LEN,
        &[SEED_RECEIPT, &[DIR_SOLANA_TO_MACHINE], &message.nonce.to_le_bytes(), &[receipt_bump]],
    )?;
    let receipt = CrossingReceipt {
        discriminator: DISCRIMINATOR_RECEIPT,
        direction: DIR_SOLANA_TO_MACHINE,
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

    save_config(config_account, &config)?;
    msg!(
        "MintWrapped nonce={} amount={} recipient={} slot={}",
        message.nonce,
        message.amount,
        message.recipient,
        clock.slot
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Burn
// ---------------------------------------------------------------------------

fn process_burn(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    amount: u64,
    destination: Pubkey,
    nonce: u64,
) -> ProgramResult {
    let mut iter = accounts.iter();
    let config_account = next_account_info(&mut iter)?;
    let user_token_account = next_account_info(&mut iter)?;
    let mint_account = next_account_info(&mut iter)?;
    let record_account = next_account_info(&mut iter)?;
    let user = next_account_info(&mut iter)?;
    let payer = next_account_info(&mut iter)?;
    let token_program = next_account_info(&mut iter)?;
    let system_program = next_account_info(&mut iter)?;

    if *config_account.owner != *program_id {
        return Err(BridgeError::InvalidAccountData.into());
    }
    // Token program pinned — see process_mint_wrapped (self-audit K-1).
    if token_program.key != &spl_token::id() {
        return Err(BridgeError::InvalidAccountData.into());
    }
    let mut config = load_config(config_account)?;
    // Kill-switch does NOT stop burns: burning is the exit path.
    config.check_burn_allowed()?;
    if amount == 0 {
        return Err(BridgeError::InvalidAmount.into());
    }
    // Burning more than the tracked outstanding supply would mean the
    // books no longer balance against the Solana vault — refuse.
    config.total_outstanding = config
        .total_outstanding
        .checked_sub(amount)
        .ok_or(BridgeError::ArithmeticOverflow)?;
    if !user.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let record_bump = check_nonce_unprocessed(
        record_account,
        program_id,
        DIR_MACHINE_TO_SOLANA,
        nonce,
    )?;

    let user_state = TokenAccount::unpack(&user_token_account.try_borrow_data()?)?;
    if user_state.owner != *user.key || user_state.mint != config.wrapped_mint {
        return Err(BridgeError::Unauthorized.into());
    }

    let burn_ix = token_burn(
        token_program.key,
        user_token_account.key,
        mint_account.key,
        user.key,
        &[],
        amount,
    )?;
    invoke(
        &burn_ix,
        &[
            user_token_account.clone(),
            mint_account.clone(),
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
        &[SEED_NONCE, &[DIR_MACHINE_TO_SOLANA], &nonce.to_le_bytes(), &[record_bump]],
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

    save_config(config_account, &config)?;
    msg!(
        "Burn nonce={} amount={} destination={} slot={}",
        nonce,
        amount,
        destination,
        clock.slot
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
        // 2-of-3 admins OR any 2 of 5 relayers may pause.
        verify_pause_authority(
            &signers,
            &config.admins.admins,
            config.admins.threshold,
            &config.relayers.relayers,
        )?;
        config.paused = true;
        msg!("Machine bridge paused");
    } else {
        // Unpause is multisig-only (BRIDGE.md 4.3).
        verify_admins(&signers, &config.admins.admins, config.admins.threshold)?;
        config.paused = false;
        msg!("Machine bridge unpaused");
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

fn process_kill_switch(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let mut iter = accounts.iter();
    let config_account = next_account_info(&mut iter)?;
    let signers = signer_pubkeys(&accounts[1..], 0);
    if *config_account.owner != *program_id {
        return Err(BridgeError::InvalidAccountData.into());
    }
    let mut config = load_config(config_account)?;
    verify_admins(&signers, &config.admins.admins, config.admins.threshold)?;
    config.killed = true;
    save_config(config_account, &config)?;
    msg!("KILL-SWITCH ENGAGED: minting is permanently disabled; burns remain open");
    Ok(())
}
