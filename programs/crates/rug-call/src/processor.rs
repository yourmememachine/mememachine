//! RugCall instruction handlers.
//!
//! The dispatch and the instruction set are final; each handler
//! is a stub returning RugCallError::NotImplemented until its
//! implementation lands. The state layout they will operate on
//! is in the state module and does not change when they land.

use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
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

fn process_initialize_config(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    _resolver: Pubkey,
    _collateral_mint: Pubkey,
) -> ProgramResult {
    Err(RugCallError::NotImplemented.into())
}

fn process_create_market(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    _subject_mint: Pubkey,
) -> ProgramResult {
    Err(RugCallError::NotImplemented.into())
}

fn process_buy_position(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    _side: u8,
    _amount: u64,
) -> ProgramResult {
    Err(RugCallError::NotImplemented.into())
}

fn process_resolve_market(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    _outcome: u8,
    _hard_trigger: u8,
    _evidence_hash: [u8; 32],
) -> ProgramResult {
    Err(RugCallError::NotImplemented.into())
}

fn process_claim(_program_id: &Pubkey, _accounts: &[AccountInfo]) -> ProgramResult {
    Err(RugCallError::NotImplemented.into())
}

fn process_cancel_market(_program_id: &Pubkey, _accounts: &[AccountInfo]) -> ProgramResult {
    Err(RugCallError::NotImplemented.into())
}
