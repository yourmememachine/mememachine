//! RugCall — a parimutuel prediction market over rug evidence.
//! See the state module for the full model.
//!
//! For a subject mint (a real Solana mainnet launch, carried as
//! data), the market asks whether the evidence engine hard-flags
//! the launch before expiry. Stakes are devnet dUSDC — play
//! collateral with no real value — held in a vault owned by the
//! market PDA. Winners split the pool pro-rata; a market whose
//! evidence cannot be obtained is cancelled and every stake is
//! refunded. Resolution is posted by the single resolver key in
//! Config, bound on-chain to the sha256 hash of the published
//! evidence bundle it resolved on. This program runs on devnet
//! only and holds no real assets.
//!
//! Build status: the state layout, instruction set and error
//! space are final; the instruction handlers land incrementally
//! and each returns RugCallError::NotImplemented until it does.

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
