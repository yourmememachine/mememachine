//! Solana-side lock program (BRIDGE.md section 2.1).
//!
//! Holds original SPL tokens in a program-owned vault PDA and releases
//! them only on a verified 3-of-5 relayer attestation of a burn on
//! Machine. No individual holds the vault keys.

pub mod processor;

use solana_program::{entrypoint::ProgramResult, pubkey::Pubkey};

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[solana_program::account_info::AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    processor::process(program_id, accounts, instruction_data)
}
