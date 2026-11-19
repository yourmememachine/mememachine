//! LaunchGuard — Machine's native anti-rug launch standard.
//! See programs/LAUNCH-GUARD.md for the design and honest limits.
//!
//! A launch registers only if the mint's mint and freeze authorities
//! are already revoked (supply is provably fixed forever), and the
//! creator locks at least 10% of supply in a program-owned vault for
//! at least 30 days. There is no admin key over launches, locks,
//! vaults, or reputation, no early release, and no instruction that
//! shortens a lock. The one privileged call is the one-time
//! Initialize below.
//!
//! Registration also enforces the dust floor set at Initialize: a
//! minimum lock size in raw units, on top of the 10% share, carried
//! by a config account that a one-time Initialize creates (bound to
//! the program's upgrade authority, like the bridge programs). The
//! floor may be set to zero, in which case only the 10% share
//! binds. No instruction changes the floor afterwards; its value
//! is a deployment-gate decision, not a constant in this code.

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
