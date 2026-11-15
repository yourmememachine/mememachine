//! LaunchGuard state, instructions, errors and parameters.
//! Design: programs/LAUNCH-GUARD.md.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

pub const SEED_LAUNCH: &[u8] = b"launch";
pub const SEED_VAULT: &[u8] = b"vault";
pub const SEED_AUTHORITY: &[u8] = b"launch-authority";

pub const DISCRIMINATOR_LAUNCH: u8 = 1;

pub const STATE_LOCKED: u8 = 1;
pub const STATE_RELEASED: u8 = 2;

/// Minimum share of total supply the creator must lock: 10%.
pub const MIN_LOCK_BPS: u64 = 1_000;
pub const BPS_DENOMINATOR: u64 = 10_000;
/// Minimum lock period: 30 days at the 400ms target slot time
/// (6,480,000 slots). Locks are slot-based, not wall-clock — the same
/// determinism rule the bridge uses (BRIDGE.md): slot counts cannot
/// drift or be nudged the way validator timestamps can.
pub const MIN_LOCK_SLOTS: u64 = 6_480_000;
/// Maximum lock period (also the cap for extensions): 5 years.
pub const MAX_LOCK_SLOTS: u64 = 394_200_000;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub struct LaunchRecord {
    pub discriminator: u8,
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub vault: Pubkey,
    pub total_supply: u64,
    pub locked_amount: u64,
    pub lock_until_slot: u64,
    pub registered_slot: u64,
    pub state: u8,
}

impl LaunchRecord {
    pub const LEN: usize = 1 + 32 + 32 + 32 + 8 + 8 + 8 + 8 + 1;
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub enum LaunchInstruction {
    /// Verify revoked authorities, create record + vault, lock tokens.
    RegisterAndLock { lock_amount: u64, lock_slots: u64 },
    /// Creator only, while locked: push the release slot later.
    ExtendLock { additional_slots: u64 },
    /// Creator only, after expiry: return the locked tokens.
    Release {},
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u32)]
pub enum LaunchError {
    MintAuthorityNotRevoked = 1,
    FreezeAuthorityNotRevoked = 2,
    ZeroSupply = 3,
    LockTooSmall = 4,
    LockPeriodTooShort = 5,
    LockPeriodTooLong = 6,
    AlreadyReleased = 7,
    StillLocked = 8,
    NotCreator = 9,
    InvalidPda = 10,
    MintMismatch = 11,
}

impl From<LaunchError> for ProgramError {
    fn from(e: LaunchError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
