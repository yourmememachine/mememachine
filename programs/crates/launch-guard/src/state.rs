//! LaunchGuard state, instructions, errors and parameters.
//! Design: programs/LAUNCH-GUARD.md.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

pub const SEED_LAUNCH: &[u8] = b"launch";
pub const SEED_VAULT: &[u8] = b"vault";
pub const SEED_AUTHORITY: &[u8] = b"launch-authority";
pub const SEED_REPUTATION: &[u8] = b"reputation";
pub const SEED_CONFIG: &[u8] = b"config";

pub const DISCRIMINATOR_LAUNCH: u8 = 1;
pub const DISCRIMINATOR_REPUTATION: u8 = 2;
pub const DISCRIMINATOR_CONFIG: u8 = 3;

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
/// Maximum lock period: 5 years, measured from registration.
/// Extensions are capped at registered_slot + MAX_LOCK_SLOTS, so
/// no sequence of extensions can push a lock's total duration
/// past this.
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

/// A creator's public track record, one PDA per creator, readable
/// by anyone and by any other program. Counters only ever move in
/// RegisterAndLock (registered += 1) and Release (completed += 1,
/// which the program only reaches after the full lock period has
/// elapsed) — there is no instruction that decrements, resets, or
/// edits them, and no admin key. A creator with completions has
/// provably waited out every lock they finished.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub struct CreatorReputation {
    pub discriminator: u8,
    pub creator: Pubkey,
    /// Protected launches registered under LaunchGuard.
    pub launches_registered: u64,
    /// Locks held for their full period and released.
    pub locks_completed: u64,
    pub first_launch_slot: u64,
    /// Slot of the most recent completion (0 = none yet).
    pub last_completed_slot: u64,
}

impl CreatorReputation {
    pub const LEN: usize = 1 + 32 + 8 + 8 + 8 + 8;
}

/// Program-wide configuration, created exactly once by
/// `Initialize`. No instruction in this program edits it
/// afterwards, so the dust floor below is set once at
/// deployment, not an admin setting. (That holds for this
/// program binary: whoever holds the upgrade authority can
/// replace the binary itself, as with any upgradeable program
/// — see LAUNCH-GUARD.md.)
///
/// The floor exists because the 10% rule alone admits dust
/// launches: a 19-token supply satisfies it with a 2-token lock,
/// and a completed dust lock counts the same in a creator's
/// reputation as a large one (see LAUNCH-GUARD.md). A launch now
/// must lock at least `min_lock_amount` raw units on top of the
/// 10% share. The VALUE is deliberately not in the code: it is
/// chosen when the program is initialized at deployment, with
/// the auditor's input — the same deployment-gate treatment the
/// bridge gives its minimum-transfer floor (THREAT-MODEL.md).
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub struct GuardConfig {
    pub discriminator: u8,
    /// Dust floor: minimum lock size, in raw token units, that
    /// `RegisterAndLock` accepts on top of the 10%-of-supply
    /// minimum. Zero means the floor is set to nothing and only
    /// the 10% rule binds.
    pub min_lock_amount: u64,
}

impl GuardConfig {
    pub const LEN: usize = 1 + 8;
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub enum LaunchInstruction {
    /// Verify revoked authorities, create record + vault, lock tokens.
    RegisterAndLock { lock_amount: u64, lock_slots: u64 },
    /// Creator only, while locked: push the release slot later.
    ExtendLock { additional_slots: u64 },
    /// Creator only, after expiry: return the locked tokens.
    Release {},
    /// Creator only, and only after Release has completed: sweep
    /// any residual balance out of the vault and close it. Anyone
    /// can transfer extra tokens into the vault (standard SPL), and
    /// Release moves exactly `locked_amount`, so without this the
    /// excess would be stuck forever. Because the instruction is
    /// gated on the record's Released state — which Release sets
    /// only after the full locked amount has left the vault — it
    /// can never touch the lock itself.
    CloseVault {},
    /// One-time setup: create the config account carrying the
    /// dust floor (`GuardConfig::min_lock_amount`). The caller
    /// must be the program's upgrade authority, proven against
    /// the ProgramData account — the same binding the bridge
    /// programs' Initialize uses (self-audit F-1). Appended last
    /// so the existing variants keep their borsh indices. There
    /// is no instruction that changes the floor afterwards.
    Initialize { min_lock_amount: u64 },
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
    /// The mint (or a token account) is not owned by the classic SPL
    /// Token program. Token-2022 mints are rejected outright in v1:
    /// their extensions (permanent delegate, transfer hooks, mint
    /// close authority, …) can reintroduce exactly the control this
    /// standard exists to remove, and the classic Mint layout this
    /// program parses does not describe them.
    UnsupportedTokenProgram = 12,
    /// CloseVault was called before Release completed the lock.
    /// The vault stays sealed — residual recovery exists only once
    /// the locked amount itself is already out.
    NotReleased = 13,
    /// The lock satisfies the 10%-of-supply minimum but is below
    /// the dust floor set at Initialize (`GuardConfig`). The two
    /// minimums are reported separately so a registrant can tell
    /// which one rejected the lock.
    LockBelowDustFloor = 14,
    /// RegisterAndLock was called before Initialize created the
    /// config account. Registration fails closed: without the
    /// config there is no floor to check against, and silently
    /// skipping the check would make the floor optional.
    ConfigNotInitialized = 15,
    /// Initialize's payer is not the program's upgrade authority
    /// recorded in the ProgramData account.
    NotUpgradeAuthority = 16,
    /// Initialize was called after the config account already
    /// exists. The floor is set exactly once; no instruction in
    /// this program changes, raises, or lowers it afterwards.
    ConfigAlreadyInitialized = 17,
}

impl From<LaunchError> for ProgramError {
    fn from(e: LaunchError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
