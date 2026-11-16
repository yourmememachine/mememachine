//! LaunchGuard — Machine's native anti-rug launch standard.
//! See programs/LAUNCH-GUARD.md for the design and honest limits.
//!
//! A launch registers only if the mint's mint and freeze authorities
//! are already revoked (supply is provably fixed forever), and the
//! creator locks at least 10% of supply in a program-owned vault for
//! at least 30 days. There is no admin key, no early release, and no
//! instruction that shortens a lock.

pub mod processor;
pub mod state;

use solana_program::{entrypoint::ProgramResult, pubkey::Pubkey};

solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[solana_program::account_info::AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    processor::process(program_id, accounts, instruction_data)
}
