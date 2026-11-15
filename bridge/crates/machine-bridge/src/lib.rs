//! Machine-side bridge program (BRIDGE.md section 2.2).
//!
//! Mints wrapped SPL tokens on Machine against quorum attestations of
//! locks on Solana mainnet, and burns them for the reverse direction.
//! Minting is ONLY reachable through a verified 3-of-5 relayer attestation
//! — there is no admin mint instruction anywhere in this program.

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
