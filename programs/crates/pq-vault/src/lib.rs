//! PQ Vault — a quantum-safe cold-storage vault for Machine.
//! See programs/PQ-VAULT.md for the design and honest limits.
//!
//! Withdrawals are authorized by WOTS+ one-time signatures
//! (RFC 8391, SHA-256) instead of Ed25519: the vault stores only
//! 32-byte hash commitments, keys are spent strictly in order
//! under program enforcement, and there is no Ed25519 path —
//! recovery, rotation, or otherwise — to the funds. Rotation to
//! a fresh key set is authorized by a one-time signature from
//! the next unconsumed key, and a vault whose keys run out
//! without a chained rotation freezes fail-closed.

pub mod processor;
pub mod state;
pub mod wots;

use solana_program::{entrypoint::ProgramResult, pubkey::Pubkey};

solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[solana_program::account_info::AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    processor::process(program_id, accounts, instruction_data)
}
