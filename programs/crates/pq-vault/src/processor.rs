//! PQ Vault instruction handlers. Design: programs/PQ-VAULT.md.
//!
//! The security shape in one paragraph: funds move in exactly
//! one handler (ExecuteWithdrawal), and that handler reaches its
//! transfer only after the buffer's digest has been re-derived
//! from the transfer's actual parameters and matched, and the
//! WOTS+ signature has verified against the commitment stored
//! for the vault's next key. The key index is incremented and
//! saved before the transfer runs. Every other handler either
//! moves no funds (buffer upload, abort) or is gated on a zero
//! balance (CloseVault) or on a one-time signature (RotateKeys).

use borsh::BorshDeserialize;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
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
use crate::wots;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_vault(account: &AccountInfo, program_id: &Pubkey) -> Result<VaultAccount, ProgramError> {
    // The vault must be one of ours. An account owned by another
    // program can carry attacker-chosen bytes — a valid
    // discriminator and forged commitments — and without this
    // check a handler would trust them. Fail here, before work.
    if account.owner != program_id {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = account.try_borrow_data()?;
    if data.len() < VaultAccount::LEN || data[..8] != DISCRIMINATOR_VAULT {
        return Err(ProgramError::InvalidAccountData);
    }
    VaultAccount::try_from_slice(&data[..VaultAccount::LEN])
        .map_err(|_| ProgramError::InvalidAccountData)
}

/// Load a vault and bind it to its canonical address: the vault
/// for (owner, mint) lives only at its derived PDA, so a
/// program-owned account holding a valid vault for some other
/// pair is not this vault.
fn load_vault_checked(
    account: &AccountInfo,
    program_id: &Pubkey,
) -> Result<VaultAccount, ProgramError> {
    let vault = load_vault(account, program_id)?;
    let (pda, _) = vault_pda(program_id, &vault.owner, &vault.mint);
    if account.key != &pda {
        return Err(PqError::InvalidPda.into());
    }
    Ok(vault)
}

fn save_vault(account: &AccountInfo, vault: &VaultAccount) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    let bytes = borsh::to_vec(vault).map_err(|_| ProgramError::AccountDataTooSmall)?;
    data[..bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

/// Check a signature buffer account is one of ours and sits at
/// the PDA its own header binds it to.
fn check_buffer(account: &AccountInfo, program_id: &Pubkey) -> Result<(), ProgramError> {
    if account.owner != program_id {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = account.try_borrow_data()?;
    if data.len() != SigBuffer::LEN || data[..8] != DISCRIMINATOR_SIG {
        return Err(ProgramError::InvalidAccountData);
    }
    let (pda, _) = sig_pda(program_id, &SigBuffer::vault(&data), SigBuffer::key_index(&data));
    if account.key != &pda {
        return Err(PqError::InvalidPda.into());
    }
    Ok(())
}

/// Close a signature buffer: all lamports to `recipient`, the
/// account reassigned to the system program and shrunk to zero.
/// The buffer's rent is returned in full, per the design.
fn close_buffer(buffer: &AccountInfo, recipient: &AccountInfo) -> ProgramResult {
    let lamports = buffer.lamports();
    **recipient.try_borrow_mut_lamports()? = recipient
        .lamports()
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    **buffer.try_borrow_mut_lamports()? = 0;
    buffer.assign(&solana_program::system_program::ID);
    buffer.resize(0)?;
    Ok(())
}

/// Create a PDA-owned account, adopting a pre-funded shell if one
/// exists. Same create-or-adopt pattern as the bridge programs
/// and LaunchGuard (self-audit F-2): the vault, token-vault and
/// buffer PDAs are deterministic, so anyone could send 1 lamport
/// to one before its creation and permanently block it — plain
/// `create_account` refuses an account that already holds
/// lamports. A pre-existing account is adopted only if it is a
/// pristine shell (lamports > 0, system-owned, no data) — the
/// only state a bare transfer can produce.
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

fn vault_pda(program_id: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_VAULT, owner.as_ref(), mint.as_ref()],
        program_id,
    )
}
fn token_vault_pda(program_id: &Pubkey, vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_TOKEN_VAULT, vault.as_ref()], program_id)
}
fn authority_pda(program_id: &Pubkey, vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_AUTHORITY, vault.as_ref()], program_id)
}
fn sig_pda(program_id: &Pubkey, vault: &Pubkey, key_index: u32) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_SIG, vault.as_ref(), &key_index.to_le_bytes()],
        program_id,
    )
}

fn check_token_program(token_program: &AccountInfo) -> ProgramResult {
    if token_program.key != &spl_token::id() {
        return Err(PqError::UnsupportedTokenProgram.into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let instruction =
        PqInstruction::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    match instruction {
        PqInstruction::CreateVault { commitments } => {
            process_create_vault(program_id, accounts, &commitments)
        }
        PqInstruction::CreateSigBuffer { key_index, digest } => {
            process_create_sig_buffer(program_id, accounts, key_index, &digest)
        }
        PqInstruction::WriteSigChunk { offset, bytes } => {
            process_write_sig_chunk(program_id, accounts, offset, &bytes)
        }
        PqInstruction::ExecuteWithdrawal { amount } => {
            process_execute_withdrawal(program_id, accounts, amount)
        }
        PqInstruction::RotateKeys { new_commitments } => {
            process_rotate_keys(program_id, accounts, &new_commitments)
        }
        PqInstruction::AbortSigBuffer {} => process_abort_sig_buffer(program_id, accounts),
        PqInstruction::CloseVault {} => process_close_vault(program_id, accounts),
    }
}

// ---------------------------------------------------------------------------
// CreateVault
// ---------------------------------------------------------------------------

fn process_create_vault(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    commitments: &[[u8; 32]],
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let owner = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let token_vault_account = next_account_info(iter)?;
    let mint_account = next_account_info(iter)?;
    let authority_account = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;
    let system_program = next_account_info(iter)?;

    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    check_token_program(token_program)?;
    if mint_account.owner != &spl_token::id() {
        return Err(PqError::UnsupportedTokenProgram.into());
    }
    // The mint must be a real, initialized classic SPL mint.
    Mint::unpack(&mint_account.try_borrow_data()?)?;

    if commitments.is_empty() || commitments.len() > MAX_KEYS {
        return Err(PqError::InvalidCommitmentCount.into());
    }

    let (vault_pda, vault_bump) = vault_pda(program_id, owner.key, mint_account.key);
    let (token_vault_pda, token_vault_bump) = token_vault_pda(program_id, &vault_pda);
    let (authority_pda, _) = authority_pda(program_id, &vault_pda);
    if vault_account.key != &vault_pda
        || token_vault_account.key != &token_vault_pda
        || authority_account.key != &authority_pda
    {
        return Err(PqError::InvalidPda.into());
    }

    create_or_adopt_account(
        owner,
        vault_account,
        system_program,
        program_id,
        VaultAccount::LEN,
        &[
            SEED_VAULT,
            owner.key.as_ref(),
            mint_account.key.as_ref(),
            &[vault_bump],
        ],
    )?;

    // The token vault: a standard SPL token account for the mint,
    // owned by the authority PDA. Only this program, signing as
    // that PDA, can ever move its funds.
    create_or_adopt_account(
        owner,
        token_vault_account,
        system_program,
        &spl_token::id(),
        TokenAccount::LEN,
        &[SEED_TOKEN_VAULT, vault_pda.as_ref(), &[token_vault_bump]],
    )?;
    invoke(
        &initialize_account3(
            token_program.key,
            token_vault_account.key,
            mint_account.key,
            &authority_pda,
        )?,
        &[
            token_vault_account.clone(),
            mint_account.clone(),
            authority_account.clone(),
            token_program.clone(),
        ],
    )?;

    let mut stored = [[0u8; 32]; MAX_KEYS];
    stored[..commitments.len()].copy_from_slice(commitments);
    let vault = VaultAccount {
        discriminator: DISCRIMINATOR_VAULT,
        owner: *owner.key,
        mint: *mint_account.key,
        token_vault: token_vault_pda,
        keys_total: commitments.len() as u8,
        next_key_index: 0,
        state: STATE_ACTIVE,
        commitments: stored,
        vault_bump,
        token_vault_bump,
    };
    save_vault(vault_account, &vault)?;

    msg!(
        "PQ Vault: created vault {} for mint {} with {} one-time keys",
        vault_pda,
        mint_account.key,
        vault.keys_total
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// CreateSigBuffer
// ---------------------------------------------------------------------------

fn process_create_sig_buffer(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    key_index: u32,
    digest: &[u8; 32],
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let payer = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let buffer_account = next_account_info(iter)?;
    let system_program = next_account_info(iter)?;

    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let vault = load_vault_checked(vault_account, program_id)?;
    if vault.state != STATE_ACTIVE {
        return Err(PqError::VaultClosed.into());
    }
    if vault.next_key_index >= vault.keys_total as u32 {
        return Err(PqError::KeysExhausted.into());
    }
    if key_index != vault.next_key_index {
        return Err(PqError::WrongKeyIndex.into());
    }
    let (buffer_pda, buffer_bump) = sig_pda(program_id, vault_account.key, key_index);
    if buffer_account.key != &buffer_pda {
        return Err(PqError::InvalidPda.into());
    }

    create_or_adopt_account(
        payer,
        buffer_account,
        system_program,
        program_id,
        SigBuffer::LEN,
        &[
            SEED_SIG,
            vault_account.key.as_ref(),
            &key_index.to_le_bytes(),
            &[buffer_bump],
        ],
    )?;

    let mut data = buffer_account.try_borrow_mut_data()?;
    SigBuffer::write_header(&mut data, vault_account.key, key_index, digest, 0);
    msg!(
        "PQ Vault: signature buffer created for vault {} key {}",
        vault_account.key,
        key_index
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// WriteSigChunk
// ---------------------------------------------------------------------------

fn process_write_sig_chunk(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    offset: u16,
    bytes: &[u8],
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let buffer_account = next_account_info(iter)?;

    check_buffer(buffer_account, program_id)?;
    if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
        return Err(PqError::ChunkOverflow.into());
    }
    let mut data = buffer_account.try_borrow_mut_data()?;
    let written = SigBuffer::written_len(&data);
    if offset != written {
        return Err(PqError::ChunkOutOfOrder.into());
    }
    let end = (offset as usize)
        .checked_add(bytes.len())
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if end > wots::SIG_BYTES {
        return Err(PqError::ChunkOverflow.into());
    }
    SigBuffer::signature_mut(&mut data)[offset as usize..end].copy_from_slice(bytes);
    SigBuffer::set_written_len(&mut data, end as u16);
    Ok(())
}

// ---------------------------------------------------------------------------
// ExecuteWithdrawal — the only handler that moves funds.
// ---------------------------------------------------------------------------

fn process_execute_withdrawal(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    amount: u64,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let payer = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let token_vault_account = next_account_info(iter)?;
    let destination_account = next_account_info(iter)?;
    let authority_account = next_account_info(iter)?;
    let buffer_account = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;

    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    check_token_program(token_program)?;
    let mut vault = load_vault_checked(vault_account, program_id)?;
    if vault.state != STATE_ACTIVE {
        return Err(PqError::VaultClosed.into());
    }
    if vault.next_key_index >= vault.keys_total as u32 {
        return Err(PqError::KeysExhausted.into());
    }
    if amount == 0 {
        return Err(PqError::ZeroAmount.into());
    }
    let key_index = vault.next_key_index;

    // Every account must be exactly the one its position means.
    let (token_vault_pda, _) = token_vault_pda(program_id, vault_account.key);
    let (authority_pda, authority_bump) = authority_pda(program_id, vault_account.key);
    let (buffer_pda, _) = sig_pda(program_id, vault_account.key, key_index);
    if token_vault_account.key != &token_vault_pda
        || token_vault_account.key != &vault.token_vault
        || authority_account.key != &authority_pda
        || buffer_account.key != &buffer_pda
    {
        return Err(PqError::InvalidPda.into());
    }
    check_buffer(buffer_account, program_id)?;

    // The destination must be a token account of this mint, and
    // not the vault itself (a self-transfer would burn the
    // one-time key while moving nothing).
    if destination_account.key == &token_vault_pda {
        return Err(PqError::InvalidDestination.into());
    }
    if destination_account.owner != &spl_token::id() {
        return Err(PqError::UnsupportedTokenProgram.into());
    }
    let destination = TokenAccount::unpack(&destination_account.try_borrow_data()?)?;
    if destination.mint != vault.mint {
        return Err(PqError::MintMismatch.into());
    }
    let vault_tokens = TokenAccount::unpack(&token_vault_account.try_borrow_data()?)?;
    if vault_tokens.mint != vault.mint || vault_tokens.owner != authority_pda {
        return Err(ProgramError::InvalidAccountData);
    }
    if vault_tokens.amount < amount {
        return Err(PqError::InsufficientBalance.into());
    }

    // The digest is re-derived from THIS execution's parameters —
    // never trusted from the buffer — and must equal the digest
    // the buffer was bound to at creation. Then the signature in
    // the buffer must verify against the stored commitment for
    // the next key, as that key.
    let digest = withdrawal_digest(
        vault_account.key,
        key_index,
        destination_account.key,
        amount,
        &vault.mint,
    );
    {
        let data = buffer_account.try_borrow_data()?;
        if SigBuffer::vault(&data) != *vault_account.key
            || SigBuffer::key_index(&data) != key_index
        {
            return Err(PqError::WrongKeyIndex.into());
        }
        if SigBuffer::written_len(&data) as usize != wots::SIG_BYTES {
            return Err(PqError::BufferIncomplete.into());
        }
        if SigBuffer::digest(&data) != digest {
            return Err(PqError::DigestMismatch.into());
        }
        if !wots::verify(
            SigBuffer::signature(&data),
            &digest,
            key_index,
            &vault.commitments[key_index as usize],
        ) {
            return Err(PqError::SignatureInvalid.into());
        }
    }

    // State first, fail-closed: the key is consumed before any
    // token moves, in the same transaction. A failed transfer
    // rolls the whole transaction back; a succeeded one can
    // never be replayed, because this index is never accepted
    // again.
    vault.next_key_index = key_index
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    save_vault(vault_account, &vault)?;

    invoke_signed(
        &transfer(
            token_program.key,
            token_vault_account.key,
            destination_account.key,
            &authority_pda,
            &[],
            amount,
        )?,
        &[
            token_vault_account.clone(),
            destination_account.clone(),
            authority_account.clone(),
            token_program.clone(),
        ],
        &[&[
            SEED_AUTHORITY,
            vault_account.key.as_ref(),
            &[authority_bump],
        ]],
    )?;

    close_buffer(buffer_account, payer)?;

    msg!(
        "PQ Vault: withdrew {} of {} from vault {} with key {}",
        amount,
        vault.mint,
        vault_account.key,
        key_index
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// RotateKeys
// ---------------------------------------------------------------------------

fn process_rotate_keys(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    new_commitments: &[[u8; 32]],
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let payer = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let buffer_account = next_account_info(iter)?;

    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if new_commitments.is_empty() || new_commitments.len() > MAX_KEYS {
        return Err(PqError::InvalidCommitmentCount.into());
    }
    let mut vault = load_vault_checked(vault_account, program_id)?;
    if vault.state != STATE_ACTIVE {
        return Err(PqError::VaultClosed.into());
    }
    if vault.next_key_index >= vault.keys_total as u32 {
        // Rotation is authorized by the next unconsumed key.
        // With none left, there is no path to new keys — the
        // freeze the design specifies, not an oversight.
        return Err(PqError::KeysExhausted.into());
    }
    let key_index = vault.next_key_index;
    let (buffer_pda, _) = sig_pda(program_id, vault_account.key, key_index);
    if buffer_account.key != &buffer_pda {
        return Err(PqError::InvalidPda.into());
    }
    check_buffer(buffer_account, program_id)?;

    let set_hash = commitment_set_hash(new_commitments);
    let digest = rotation_digest(vault_account.key, key_index, &set_hash);
    {
        let data = buffer_account.try_borrow_data()?;
        if SigBuffer::vault(&data) != *vault_account.key
            || SigBuffer::key_index(&data) != key_index
        {
            return Err(PqError::WrongKeyIndex.into());
        }
        if SigBuffer::written_len(&data) as usize != wots::SIG_BYTES {
            return Err(PqError::BufferIncomplete.into());
        }
        if SigBuffer::digest(&data) != digest {
            return Err(PqError::DigestMismatch.into());
        }
        if !wots::verify(
            SigBuffer::signature(&data),
            &digest,
            key_index,
            &vault.commitments[key_index as usize],
        ) {
            return Err(PqError::SignatureInvalid.into());
        }
    }

    // The authorizing key is consumed by replacing the whole
    // set atomically: its commitment ceases to exist, so the
    // signature that authorized this rotation can never be
    // replayed, and the new set starts at index 0.
    let mut stored = [[0u8; 32]; MAX_KEYS];
    stored[..new_commitments.len()].copy_from_slice(new_commitments);
    vault.commitments = stored;
    vault.keys_total = new_commitments.len() as u8;
    vault.next_key_index = 0;
    save_vault(vault_account, &vault)?;

    close_buffer(buffer_account, payer)?;

    msg!(
        "PQ Vault: vault {} rotated to a fresh set of {} keys, authorized by key {}",
        vault_account.key,
        vault.keys_total,
        key_index
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// AbortSigBuffer
// ---------------------------------------------------------------------------

fn process_abort_sig_buffer(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let iter = &mut accounts.iter();
    let owner = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let buffer_account = next_account_info(iter)?;

    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let vault = load_vault_checked(vault_account, program_id)?;
    if vault.owner != *owner.key {
        return Err(PqError::NotOwner.into());
    }
    check_buffer(buffer_account, program_id)?;
    {
        let data = buffer_account.try_borrow_data()?;
        if SigBuffer::vault(&data) != *vault_account.key {
            return Err(PqError::InvalidPda.into());
        }
    }
    close_buffer(buffer_account, owner)?;
    msg!(
        "PQ Vault: signature buffer for vault {} aborted by owner",
        vault_account.key
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// CloseVault
// ---------------------------------------------------------------------------

fn process_close_vault(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let iter = &mut accounts.iter();
    let owner = next_account_info(iter)?;
    let vault_account = next_account_info(iter)?;
    let token_vault_account = next_account_info(iter)?;
    let authority_account = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;

    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    check_token_program(token_program)?;
    let mut vault = load_vault_checked(vault_account, program_id)?;
    if vault.owner != *owner.key {
        return Err(PqError::NotOwner.into());
    }
    if vault.state != STATE_ACTIVE {
        return Err(PqError::VaultClosed.into());
    }
    let (token_vault_pda, _) = token_vault_pda(program_id, vault_account.key);
    let (authority_pda, authority_bump) = authority_pda(program_id, vault_account.key);
    if token_vault_account.key != &token_vault_pda
        || token_vault_account.key != &vault.token_vault
        || authority_account.key != &authority_pda
    {
        return Err(PqError::InvalidPda.into());
    }
    let vault_tokens = TokenAccount::unpack(&token_vault_account.try_borrow_data()?)?;
    if vault_tokens.amount != 0 {
        return Err(PqError::VaultNotEmpty.into());
    }

    invoke_signed(
        &close_account(
            token_program.key,
            token_vault_account.key,
            owner.key,
            &authority_pda,
            &[],
        )?,
        &[
            token_vault_account.clone(),
            owner.clone(),
            authority_account.clone(),
            token_program.clone(),
        ],
        &[&[
            SEED_AUTHORITY,
            vault_account.key.as_ref(),
            &[authority_bump],
        ]],
    )?;

    vault.state = STATE_CLOSED;
    save_vault(vault_account, &vault)?;
    msg!("PQ Vault: vault {} closed by owner", vault_account.key);
    Ok(())
}
