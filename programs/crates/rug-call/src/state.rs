//! RugCall state, instructions, errors and parameters.
//!
//! RugCall is a parimutuel prediction market over rug evidence:
//! for a subject mint (a real Solana mainnet launch, carried here
//! as 32 bytes of data — this program runs on devnet and creates
//! no account for the subject), the market asks whether the
//! evidence engine hard-flags the launch before expiry. Stakes are
//! devnet dUSDC (play collateral with no real value) held in a
//! vault owned by the market PDA. Winners split the whole pool
//! pro-rata to their winning stake:
//!
//! ```text
//! payout = floor(winner_stake * total_pool / winning_pool)
//! ```
//!
//! computed in u128 and paid strictly from the pools recorded in
//! the market account, never from the live vault balance (anyone
//! can transfer extra tokens into a vault; that changes its
//! balance, not the accounting). The program holds no opinion
//! about tokens: resolution is posted by the single resolver key
//! named in Config, bound on-chain to the sha256 hash of the
//! published evidence bundle it resolved on.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

pub const SEED_CONFIG: &[u8] = b"config";
pub const SEED_MARKET: &[u8] = b"market";
pub const SEED_VAULT: &[u8] = b"vault";
pub const SEED_POSITION: &[u8] = b"position";
pub const SEED_LIVE_MARKET: &[u8] = b"live-market";

pub const DISCRIMINATOR_CONFIG: u8 = 1;
pub const DISCRIMINATOR_MARKET: u8 = 2;
pub const DISCRIMINATOR_POSITION: u8 = 3;
pub const DISCRIMINATOR_LIVE_MARKET: u8 = 4;

/// Market lifecycle. A market is created Open; ResolveMarket moves
/// it to Resolved, CancelMarket to Cancelled. Both terminal states
/// are final: no instruction moves a market out of them, and the
/// pools, outcome, evidence hash and resolution time recorded at
/// resolution never change afterwards. Claim is the only
/// instruction that still acts on a terminal market, and it
/// changes nothing in the market account — only the caller's
/// position flag and the vault balance.
pub const STATE_OPEN: u8 = 1;
pub const STATE_RESOLVED: u8 = 2;
pub const STATE_CANCELLED: u8 = 3;

/// Side and outcome share one encoding, so a client building a
/// BuyPosition and the resolver posting a ResolveMarket can never
/// disagree about which byte means what. Zero is never a side:
/// it is the pre-resolution outcome sentinel, so an Open market
/// can never be misread as resolved.
pub const OUTCOME_NONE: u8 = 0;
pub const SIDE_FLAGGED: u8 = 1;
pub const SIDE_SURVIVES: u8 = 2;
pub const OUTCOME_FLAGGED: u8 = SIDE_FLAGGED;
pub const OUTCOME_SURVIVES: u8 = SIDE_SURVIVES;

/// Hard triggers are the evidence engine's five mechanical
/// checks (creator strike history, coordinated first-minute
/// bundle, creator sell-through, liquidity gone, flagged-crew
/// link). Zero means no trigger — the only value consistent with
/// a Survives outcome. A Flagged outcome must name its trigger:
/// a resolution that cannot say which checkable fact fired is
/// not accepted.
pub const HARD_TRIGGER_NONE: u8 = 0;
pub const HARD_TRIGGER_MAX: u8 = 5;

/// Default market lifetime: 24 hours, in wall-clock seconds.
/// RugCall's subject matter (evidence event timestamps, 24-hour
/// windows) is wall-clock by nature, so all times in this program
/// are `Clock::unix_timestamp` values (i64) — not slots.
pub const DEFAULT_MARKET_DURATION_SECS: i64 = 86_400;
/// After expiry, an unresolved market sits in this grace window
/// (24 hours) while the resolver retries its evidence fetch.
/// Once `now > expiry + CANCEL_GRACE_SECS` anyone may cancel the
/// market and every stake is refunded: funds are never trapped
/// in an unresolved market and never paid out on a guess.
pub const CANCEL_GRACE_SECS: i64 = 86_400;
/// Minimum stake per BuyPosition call: 1 dUSDC in raw units
/// (dUSDC has 6 decimals). Dust positions are noise; the check
/// also rejects a zero amount.
pub const MIN_STAKE_RAW: u64 = 1_000_000;
/// No fee is taken in this version. The field exists in Config
/// so the accounting shape is honest about where a fee would
/// live; InitializeConfig always sets it to this constant.
pub const FEE_BPS: u64 = 0;
pub const BPS_DENOMINATOR: u64 = 10_000;

/// Program-wide configuration, created exactly once by
/// InitializeConfig. No instruction edits it afterwards: the
/// resolver key and the collateral mint named here are fixed for
/// the life of this program deployment, so both must be verified
/// before InitializeConfig is called — a wrong value can only be
/// corrected by redeploying the program.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub struct RugCallConfig {
    pub discriminator: u8,
    /// The key that called InitializeConfig. Stored for the
    /// record; no instruction in this version takes orders from
    /// the admin — resolution belongs to the resolver alone.
    pub admin: Pubkey,
    /// The only key whose signature ResolveMarket accepts, and
    /// the only key that may cancel a market before the grace
    /// window ends.
    pub resolver: Pubkey,
    /// The collateral mint (devnet dUSDC). Every vault and every
    /// position payment is in this mint.
    pub collateral_mint: Pubkey,
    /// Always FEE_BPS (0) in this version.
    pub fee_bps: u64,
    /// Market lifetime set at CreateMarket: expiry is
    /// created_at + default_duration_secs.
    pub default_duration_secs: i64,
    /// Minimum stake per BuyPosition call, in raw units.
    pub min_stake: u64,
    pub bump: u8,
}

impl RugCallConfig {
    pub const LEN: usize = 1 + 32 + 32 + 32 + 8 + 8 + 8 + 1;
}

/// One market on one subject mint. PDA seeds:
/// ["market", subject_mint, created_at as i64 little-endian] —
/// the bucket is the exact creation second, read from the Clock
/// sysvar at creation, never supplied by the caller (a supplied
/// bucket could be ground to dodge the duplicate check or squat
/// future addresses). The exact second also means a market that
/// resolves early never blocks the next market on the same mint:
/// its successor simply gets a later timestamp.
///
/// The timestamp seed alone cannot enforce "one live market per
/// mint" — every creation is a distinct PDA, and a program cannot
/// scan for sibling accounts. That rule lives in the LiveMarket
/// lock account below.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub struct Market {
    pub discriminator: u8,
    /// The mainnet mint this market is about (data only).
    pub subject_mint: Pubkey,
    pub creator: Pubkey,
    pub created_at: i64,
    pub expiry: i64,
    /// Total stakes on each side, in raw collateral units. Frozen
    /// the moment the market leaves Open: buys are rejected in
    /// terminal states and Claim never writes these fields, so
    /// every claim on a market computes against the same pools —
    /// payouts cannot depend on claim order.
    pub flagged_pool: u64,
    pub survives_pool: u64,
    /// One of STATE_OPEN / STATE_RESOLVED / STATE_CANCELLED.
    pub state: u8,
    /// OUTCOME_NONE while Open or after a cancel; otherwise
    /// OUTCOME_FLAGGED or OUTCOME_SURVIVES.
    pub outcome: u8,
    /// sha256 of the resolver's published evidence bundle.
    /// All-zero until resolution, and ResolveMarket refuses the
    /// all-zero value: a resolution must be bound to a bundle.
    pub evidence_hash: [u8; 32],
    /// HARD_TRIGGER_NONE (0) unless the outcome is Flagged.
    pub hard_trigger: u8,
    /// Resolution time; 0 until resolved.
    pub resolved_at: i64,
    pub bump: u8,
    pub vault_bump: u8,
}

impl Market {
    pub const LEN: usize = 1 + 32 + 32 + 8 + 8 + 8 + 8 + 1 + 1 + 32 + 1 + 8 + 1 + 1;
}

/// The one-live-market lock for a subject mint, PDA seeds
/// ["live-market", subject_mint]. CreateMarket creates it
/// pointing at the new market and fails if it already exists;
/// ResolveMarket and CancelMarket close it in the same
/// instruction that moves the market out of Open, so the lock's
/// lifetime is the market's Open lifetime by construction, and a
/// follow-up market on the same mint (after a resolution OR a
/// cancel) can always be created. It also gives clients and the
/// resolver an O(1) lookup of the live market for a mint, which
/// the timestamp-seeded Market PDA cannot provide on its own.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub struct LiveMarket {
    pub discriminator: u8,
    pub market: Pubkey,
    pub bump: u8,
}

impl LiveMarket {
    pub const LEN: usize = 1 + 32 + 1;
}

/// One user's stakes in one market, PDA seeds
/// ["position", market, user]. Both sides live in one account so
/// hedging works naturally and a single `claimed` flag covers the
/// payout and both refund paths — one claim per position, ever.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub struct Position {
    pub discriminator: u8,
    pub market: Pubkey,
    pub user: Pubkey,
    pub flagged_amount: u64,
    pub survives_amount: u64,
    pub claimed: bool,
    pub bump: u8,
}

impl Position {
    pub const LEN: usize = 1 + 32 + 32 + 8 + 8 + 1 + 1;
}

/// The parimutuel payout for one winning position:
/// floor(winner_stake * total_pool / winning_pool), computed in
/// u128. Returns None — never a wrapped or truncated number —
/// when the question has no honest answer: the winning pool is
/// zero (Claim's refund branch handles that case before this
/// function is ever called with it), the inputs are inconsistent
/// (a stake larger than its pool, or a total smaller than the
/// winning pool), or the true result does not fit in u64.
///
/// `total_pool` is passed in, not computed here, because the
/// addition flagged_pool + survives_pool is itself an overflow
/// surface: callers must build it with checked arithmetic and
/// refuse the buy that would make the total unrepresentable.
pub fn parimutuel_payout(winner_stake: u64, total_pool: u64, winning_pool: u64) -> Option<u64> {
    if winning_pool == 0 || winner_stake > winning_pool || total_pool < winning_pool {
        return None;
    }
    let payout = (winner_stake as u128) * (total_pool as u128) / (winning_pool as u128);
    u64::try_from(payout).ok()
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub enum RugCallInstruction {
    /// One-time setup: create Config. The payer must be the
    /// program's upgrade authority, proven against the
    /// ProgramData account — the same binding the bridge
    /// programs and LaunchGuard use for their Initialize.
    /// Admin is the payer; fee, duration and minimum stake come
    /// from this crate's constants, not from the caller.
    InitializeConfig {
        resolver: Pubkey,
        collateral_mint: Pubkey,
    },
    /// Permissionless: open a market on a subject mint. Fails if
    /// a live market already exists for the mint. Expiry is
    /// created_at + Config.default_duration_secs.
    CreateMarket { subject_mint: Pubkey },
    /// Stake `amount` of collateral on a side (SIDE_FLAGGED or
    /// SIDE_SURVIVES). The amount moves into the market vault and
    /// both the pool and the caller's position grow by it.
    /// Rejected unless the market is Open and now < expiry, and
    /// unless amount >= Config.min_stake.
    BuyPosition { side: u8, amount: u64 },
    /// Resolver only. Posts the outcome and the evidence hash.
    /// Before expiry only a Flagged outcome is accepted, and a
    /// Flagged outcome must always name its hard trigger; a
    /// Survives outcome requires trigger 0. Irreversible.
    ResolveMarket {
        outcome: u8,
        hard_trigger: u8,
        evidence_hash: [u8; 32],
    },
    /// After resolution: pay the caller their pro-rata share of
    /// the pool, or — when the winning pool is zero, or the
    /// market was cancelled — refund their exact total stake.
    /// One claim per position.
    Claim {},
    /// The fail-closed exit. The resolver may cancel any Open
    /// market; anyone may cancel once now > expiry + grace with
    /// no resolution posted. Afterwards Claim refunds exact
    /// stakes.
    CancelMarket {},
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u32)]
pub enum RugCallError {
    /// InitializeConfig was called after Config already exists.
    /// The configuration is set exactly once.
    ConfigAlreadyInitialized = 1,
    /// An instruction needed Config before InitializeConfig
    /// created it. Fails closed: without Config there is no
    /// resolver or mint to check against.
    ConfigNotInitialized = 2,
    /// InitializeConfig's payer is not the program's upgrade
    /// authority recorded in the ProgramData account.
    NotUpgradeAuthority = 3,
    /// A derived address or bump does not match the account
    /// supplied, on any PDA or the vault.
    InvalidPda = 4,
    /// A token account's mint is not Config.collateral_mint, or
    /// a position's stored market does not match the market
    /// account supplied.
    MintMismatch = 5,
    /// A token account is not owned by the classic SPL Token
    /// program. Token-2022 accounts are rejected: their
    /// extensions are outside this program's accounting.
    UnsupportedTokenProgram = 6,
    /// A live market already exists for this subject mint.
    DuplicateLiveMarket = 7,
    /// A side or outcome byte other than 1 (Flagged) or
    /// 2 (Survives).
    InvalidSide = 8,
    /// BuyPosition amount below Config.min_stake (includes 0).
    BelowMinStake = 9,
    /// BuyPosition on a Resolved or Cancelled market.
    MarketNotOpen = 10,
    /// BuyPosition at or after expiry on an Open market. Buys
    /// require now < expiry; at now == expiry the resolution
    /// path wins the tie — the fail-closed direction.
    BuyAfterExpiry = 11,
    /// The signer is not Config.resolver.
    NotResolver = 12,
    /// ResolveMarket or CancelMarket on a Resolved market.
    /// Resolution is irreversible.
    AlreadyResolved = 13,
    /// ResolveMarket or CancelMarket on a Cancelled market.
    AlreadyCancelled = 14,
    /// Hard trigger above 5, or inconsistent with the outcome:
    /// Flagged requires a trigger in 1..=5, Survives requires 0.
    InvalidHardTrigger = 15,
    /// ResolveMarket posted a Survives outcome before expiry.
    /// Early resolution is Flagged-only: "no trigger yet" is not
    /// evidence of survival until the window closes.
    EarlySurvivesResolution = 16,
    /// Claim on an Open market. Claim is legal only on Resolved
    /// (payout or refund) and Cancelled (refund) markets.
    ClaimBeforeResolve = 17,
    /// The position was already claimed — payout and refund
    /// paths share the one flag, so no second extraction exists.
    AlreadyClaimed = 18,
    /// Resolved market, the caller holds no winning-side stake,
    /// and the refund case (zero winning pool) does not apply;
    /// or a refund claim on a position with zero total stake.
    NoClaimableStake = 19,
    /// A non-resolver tried to cancel at or before
    /// expiry + CANCEL_GRACE_SECS.
    CancelTooEarly = 20,
    /// A checked arithmetic operation overflowed, including a
    /// payout that does not fit in u64.
    Overflow = 21,
    /// A handler that has not landed yet. Every instruction
    /// returns this until its implementation replaces the stub;
    /// no shipped handler may return it.
    NotImplemented = 22,
    /// ResolveMarket posted the all-zero evidence hash, which is
    /// the pre-resolution sentinel: the resolution would be
    /// indistinguishable from an unbound one.
    InvalidEvidenceHash = 23,
}

impl From<RugCallError> for ProgramError {
    fn from(e: RugCallError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    fn sample_config() -> RugCallConfig {
        RugCallConfig {
            discriminator: DISCRIMINATOR_CONFIG,
            admin: key(1),
            resolver: key(2),
            collateral_mint: key(3),
            fee_bps: FEE_BPS,
            default_duration_secs: DEFAULT_MARKET_DURATION_SECS,
            min_stake: MIN_STAKE_RAW,
            bump: 254,
        }
    }

    fn sample_market() -> Market {
        Market {
            discriminator: DISCRIMINATOR_MARKET,
            subject_mint: key(4),
            creator: key(5),
            created_at: 1_700_000_000,
            expiry: 1_700_000_000 + DEFAULT_MARKET_DURATION_SECS,
            flagged_pool: u64::MAX,
            survives_pool: 42,
            state: STATE_OPEN,
            outcome: OUTCOME_NONE,
            evidence_hash: [7; 32],
            hard_trigger: HARD_TRIGGER_NONE,
            resolved_at: 0,
            bump: 253,
            vault_bump: 252,
        }
    }

    fn sample_position() -> Position {
        Position {
            discriminator: DISCRIMINATOR_POSITION,
            market: key(6),
            user: key(7),
            flagged_amount: 400,
            survives_amount: 600,
            claimed: false,
            bump: 251,
        }
    }

    #[test]
    fn config_len_matches_borsh() {
        let config = sample_config();
        let bytes = borsh::to_vec(&config).unwrap();
        assert_eq!(bytes.len(), RugCallConfig::LEN);
        assert_eq!(RugCallConfig::LEN, 122);
        assert_eq!(RugCallConfig::try_from_slice(&bytes).unwrap(), config);
    }

    #[test]
    fn market_len_matches_borsh() {
        // Pools at u64::MAX on purpose: the length must not depend
        // on the values, and nothing may truncate at the extremes.
        let market = sample_market();
        let bytes = borsh::to_vec(&market).unwrap();
        assert_eq!(bytes.len(), Market::LEN);
        assert_eq!(Market::LEN, 142);
        assert_eq!(Market::try_from_slice(&bytes).unwrap(), market);
    }

    #[test]
    fn position_len_matches_borsh() {
        for claimed in [false, true] {
            let position = Position {
                claimed,
                ..sample_position()
            };
            let bytes = borsh::to_vec(&position).unwrap();
            assert_eq!(bytes.len(), Position::LEN);
            assert_eq!(Position::try_from_slice(&bytes).unwrap(), position);
        }
        assert_eq!(Position::LEN, 83);
    }

    #[test]
    fn position_claimed_byte_must_be_zero_or_one() {
        let mut bytes = borsh::to_vec(&sample_position()).unwrap();
        // claimed sits after discriminator + two pubkeys + two u64s.
        bytes[1 + 32 + 32 + 8 + 8] = 2;
        assert!(Position::try_from_slice(&bytes).is_err());
    }

    #[test]
    fn live_market_len_matches_borsh() {
        let live = LiveMarket {
            discriminator: DISCRIMINATOR_LIVE_MARKET,
            market: key(8),
            bump: 250,
        };
        let bytes = borsh::to_vec(&live).unwrap();
        assert_eq!(bytes.len(), LiveMarket::LEN);
        assert_eq!(LiveMarket::LEN, 34);
        assert_eq!(LiveMarket::try_from_slice(&bytes).unwrap(), live);
    }

    #[test]
    fn market_field_offsets_pinned() {
        // The off-chain clients mirror this layout byte for byte;
        // drift must fail here, not in a demo.
        let bytes = borsh::to_vec(&sample_market()).unwrap();
        assert_eq!(bytes[0], DISCRIMINATOR_MARKET);
        assert_eq!(&bytes[1..33], &[4u8; 32]); // subject_mint
        assert_eq!(&bytes[33..65], &[5u8; 32]); // creator
        assert_eq!(&bytes[99..131], &[7u8; 32]); // evidence_hash
        assert_eq!(bytes[131], HARD_TRIGGER_NONE); // hard_trigger
        assert_eq!(bytes[140], 253); // bump
        assert_eq!(bytes[141], 252); // vault_bump
    }

    #[test]
    fn decode_rejects_truncated_and_trailing() {
        let bytes = borsh::to_vec(&sample_market()).unwrap();
        assert!(Market::try_from_slice(&bytes[..Market::LEN - 1]).is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(Market::try_from_slice(&trailing).is_err());
        let config = borsh::to_vec(&sample_config()).unwrap();
        assert!(RugCallConfig::try_from_slice(&config[..RugCallConfig::LEN - 1]).is_err());
    }

    fn roundtrip(ix: RugCallInstruction, ordinal: u8, len: usize) {
        let bytes = borsh::to_vec(&ix).unwrap();
        assert_eq!(bytes[0], ordinal);
        assert_eq!(bytes.len(), len);
        assert_eq!(RugCallInstruction::try_from_slice(&bytes).unwrap(), ix);
        // Every variant truncated by one byte must fail to decode.
        assert!(RugCallInstruction::try_from_slice(&bytes[..len - 1]).is_err());
    }

    #[test]
    fn ix_initialize_config_roundtrip() {
        roundtrip(
            RugCallInstruction::InitializeConfig {
                resolver: key(2),
                collateral_mint: key(3),
            },
            0,
            65,
        );
    }

    #[test]
    fn ix_create_market_roundtrip() {
        roundtrip(
            RugCallInstruction::CreateMarket {
                subject_mint: key(4),
            },
            1,
            33,
        );
    }

    #[test]
    fn ix_buy_position_roundtrip() {
        for side in [SIDE_FLAGGED, SIDE_SURVIVES] {
            roundtrip(
                RugCallInstruction::BuyPosition {
                    side,
                    amount: u64::MAX,
                },
                2,
                10,
            );
        }
    }

    #[test]
    fn ix_resolve_market_roundtrip() {
        roundtrip(
            RugCallInstruction::ResolveMarket {
                outcome: OUTCOME_FLAGGED,
                hard_trigger: 3,
                evidence_hash: [9; 32],
            },
            3,
            35,
        );
    }

    #[test]
    fn ix_claim_and_cancel_roundtrip() {
        roundtrip(RugCallInstruction::Claim {}, 4, 1);
        roundtrip(RugCallInstruction::CancelMarket {}, 5, 1);
    }

    #[test]
    fn ix_unknown_variant_rejected() {
        assert!(RugCallInstruction::try_from_slice(&[6]).is_err());
        assert!(RugCallInstruction::try_from_slice(&[255]).is_err());
    }

    #[test]
    fn payout_normal_split() {
        assert_eq!(parimutuel_payout(100, 1000, 700), Some(142));
        assert_eq!(parimutuel_payout(600, 1000, 700), Some(857));
        // 142 + 857 = 999 of 1000: one unit of floor dust stays in
        // the vault by design; there is no sweep instruction.
    }

    #[test]
    fn payout_exact_division_and_floor() {
        assert_eq!(parimutuel_payout(50, 1000, 500), Some(100));
        assert_eq!(parimutuel_payout(2, 100, 3), Some(66));
        assert_eq!(parimutuel_payout(1, 10, 3), Some(3));
    }

    #[test]
    fn payout_lone_winner_takes_whole_pool() {
        // One winner holding the entire winning side takes the
        // whole pool, including the losing side's stakes.
        assert_eq!(parimutuel_payout(700, 1000, 700), Some(1000));
        assert_eq!(parimutuel_payout(250, 1000, 250), Some(1000));
    }

    #[test]
    fn payout_one_sided_market_returns_exact_stake() {
        // Every stake sits on the side that wins (total == winning
        // pool): each payout is exactly the stake. No windfall
        // exists to pay, and none is invented.
        assert_eq!(parimutuel_payout(200, 500, 500), Some(200));
        assert_eq!(parimutuel_payout(300, 500, 500), Some(300));
    }

    #[test]
    fn payout_zero_winning_pool_has_no_answer() {
        // Claim's refund branch intercepts this case; the helper
        // itself must refuse rather than divide by zero.
        assert_eq!(parimutuel_payout(0, 1000, 0), None);
        assert_eq!(parimutuel_payout(100, 1000, 0), None);
    }

    #[test]
    fn payout_zero_stake_is_zero() {
        // The helper is honest about the math; turning a zero
        // winning stake into a clean failure is Claim's job.
        assert_eq!(parimutuel_payout(0, 1000, 700), Some(0));
    }

    #[test]
    fn payout_u128_scale() {
        let two63 = 9_223_372_036_854_775_808u64;
        assert_eq!(
            parimutuel_payout(two63, u64::MAX, two63),
            Some(u64::MAX)
        );
        assert_eq!(
            parimutuel_payout(u64::MAX, u64::MAX, u64::MAX),
            Some(u64::MAX)
        );
    }

    #[test]
    fn payout_inconsistent_inputs_have_no_answer() {
        // True result would exceed u64::MAX: refuse, never wrap.
        assert_eq!(parimutuel_payout(u64::MAX, u64::MAX, 1), None);
        // Total smaller than the winning pool is not a real market.
        assert_eq!(parimutuel_payout(10, 5, 7), None);
        // A stake larger than its own pool is not a real position.
        assert_eq!(parimutuel_payout(800, 1000, 700), None);
    }
}
