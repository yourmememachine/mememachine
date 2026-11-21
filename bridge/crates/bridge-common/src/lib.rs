//! Shared logic for the Machine bridge programs.
//!
//! Implements BRIDGE.md exactly: the versioned cross-chain message format,
//! attestation verification (3-of-5 relayers), replay protection, caps,
//! and the pause / kill-switch authority model. Contains no chain-specific
//! behavior — both the Solana-side lock program and the Machine-side bridge
//! program build on this crate.
//!
//! Design notes (deviations from BRIDGE.md are flagged here, not hidden):
//! - Unpause is multisig-only. BRIDGE.md section 4.3 says pause "is not
//!   reversible by relayers — only the multisig can unpause". The task
//!   brief suggested relayers could lift a pause; the design wins.
//! - Relayer-set changes are a multisig-only instruction (`SetRelayers`).
//!   BRIDGE.md section 3.3 says a faulty relayer "is removed from the set
//!   by multisig"; there is no separate relayer-change instruction in the
//!   design, so it follows the same authority model as `set_cap`.
//! - Caps are enforced on two layers. Absolute per-tx and daily caps are
//!   set at initialization (the deployment gate in BRIDGE.md section 5
//!   requires absolutes). On top of those, BRIDGE.md section 4.4's
//!   percentages are enforced dynamically on Solana-side unlocks: each
//!   unlock at most 1% of the vault's locked total, each day's unlocks
//!   at most 5% (see `check_dynamic_caps` for why the mint side, whose
//!   supply is created by the mints themselves, uses absolutes only).
//!   The daily window is slot-based (216,000 slots), not wall-clock:
//!   a slot cannot be skewed by a validator. Attestation expiry is
//!   the deliberate exception — it is a coarse freshness deadline,
//!   not an accounting window, and slots are not comparable across
//!   two independent chains (see `validate_message`).
//! - On-chain attestation verification uses the ed25519 precompile +
//!   instructions sysvar (the Solana-standard path): the submitting
//!   transaction carries an ed25519-program instruction whose
//!   signatures the runtime itself verifies; the bridge programs parse
//!   that instruction from the sysvar and check that at least a quorum
//!   of registered relayers signed this message's hash. The dalek-based
//!   `verify_attestation` remains for off-chain use by the relayer,
//!   which re-verifies peer attestations before signing.

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use solana_program::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
};

// ---------------------------------------------------------------------------
// Constants (BRIDGE.md section 2.3, 3.1, 4.3, 4.4)
// ---------------------------------------------------------------------------

/// Chain identifiers in the signed message.
pub const CHAIN_SOLANA: u8 = 1;
pub const CHAIN_MACHINE: u8 = 2;

/// Current message format version. V2 (2026-11-20): attestation
/// expiry moved from a source-chain slot to a unix timestamp, and
/// the message gained the source event's timestamp (see
/// `validate_message`).
pub const MESSAGE_VERSION: u8 = 2;

/// Domain separator mixed into every message hash.
pub const DOMAIN_SEPARATOR: &[u8] = b"MACHINE-BRIDGE-V2";

/// Relayer set size and quorum (BRIDGE.md section 3.1: 5 relayers, 3-of-5).
pub const MAX_RELAYERS: usize = 5;
pub const QUORUM: u8 = 3;

/// Admin multisig: 3 admins, threshold 2. No single admin can act alone.
pub const MAX_ADMINS: usize = 3;
pub const ADMIN_THRESHOLD: u8 = 2;

/// Pause can also be triggered by any 2 of 5 relayers (BRIDGE.md 4.3:
//  low threshold for stopping, high threshold for moving funds).
pub const PAUSE_RELAYER_THRESHOLD: u8 = 2;

/// Slots per daily-cap window: 216,000 slots at the 400 ms target slot
/// time is 24 hours. Slot-based windows are deterministic — unlike the
/// Clock sysvar's timestamp, a slot cannot be skewed by a validator.
pub const SLOTS_PER_DAY: u64 = 216_000;

/// Maximum attestation lifetime in seconds: a message's
/// `expiry_timestamp - source_timestamp` may not exceed this (the
/// 2026-11-18 bound of 1,000,000 slots at 400 ms, restated in
/// seconds). Matches the relayer config cap on `expiry_window_secs`;
/// enforced in `validate_message`, the one shared validator every
/// verification path funnels through, so a quorum-signed message
/// with a far-future expiry cannot become a permanent bearer
/// instrument.
pub const MAX_ATTESTATION_LIFETIME_SECS: i64 = 400_000;

/// How far a message's source timestamp may lie ahead of the
/// destination chain's clock before it is rejected as post-dated.
/// Source and destination clocks are independent, and this
/// allowance is an operational bound, not a measured constant:
/// validator clocks on both chains must be kept within it of each
/// other, and a direction whose clocks diverge by more than this
/// halts fail-closed (check 4 rejects every message) until they
/// are brought back in line. On Solana the destination clock is a
/// stake-weighted median of validator votes; on Machine's current
/// single-node localnet it is that one validator's own clock —
/// the bridge deployment gate (BRIDGE.md section 5) is what keeps
/// that net from being a bridge endpoint.
pub const MAX_SOURCE_TIMESTAMP_SKEW_SECS: i64 = 3_600;

/// Basis-point denominator for the dynamic percentage caps.
pub const BPS_DENOMINATOR: u64 = 10_000;
/// Dynamic per-transaction cap: 1% of the reference supply (BRIDGE.md
/// section 4.4).
pub const DYNAMIC_PER_TX_BPS: u64 = 100;
/// Dynamic daily cap: 5% of the reference supply (BRIDGE.md 4.4).
pub const DYNAMIC_DAILY_BPS: u64 = 500;
/// Tightened dynamic caps (BRIDGE.md 4.4): after any unpause, the
/// bridge reopens at a quarter of the normal dynamic limits for one
/// cap window, then relaxes automatically. An anomaly that was
/// serious enough to pause the bridge should not be followed by a
/// full-speed reopening.
pub const TIGHTENED_PER_TX_BPS: u64 = 25;
pub const TIGHTENED_DAILY_BPS: u64 = 125;
/// How long the tightened caps apply after an unpause: one full
/// dynamic-cap window.
pub const TIGHTENING_WINDOW_SLOTS: u64 = SLOTS_PER_DAY;

/// Cross-chain direction, used in nonce PDA seeds so the two directions
/// can never share a nonce registry.
pub const DIR_SOLANA_TO_MACHINE: u8 = 0;
pub const DIR_MACHINE_TO_SOLANA: u8 = 1;

/// PDA seeds.
pub const SEED_CONFIG: &[u8] = b"config";
pub const SEED_NONCE: &[u8] = b"nonce";
pub const SEED_AUTHORITY: &[u8] = b"bridge-authority";
pub const SEED_RECEIPT: &[u8] = b"receipt";

/// Account discriminators (first byte of every account's data).
pub const DISCRIMINATOR_CONFIG: u8 = 1;
pub const DISCRIMINATOR_NONCE_RECORD: u8 = 2;
pub const DISCRIMINATOR_EVENT_RECORD: u8 = 3;
pub const DISCRIMINATOR_RECEIPT: u8 = 4;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeError {
    InvalidVersion,
    WrongSourceChain,
    WrongDestChain,
    ExpiredAttestation,
    InvalidSignature,
    UnknownRelayer,
    DuplicateRelayerSignature,
    QuorumNotReached,
    NonceAlreadyProcessed,
    AmountExceedsPerTxCap,
    AmountExceedsDailyCap,
    VaultCeilingExceeded,
    BridgePaused,
    BridgeKilled,
    Unauthorized,
    InvalidState,
    AccountAlreadyInitialized,
    InvalidAccountData,
    InvalidAmount,
    ArithmeticOverflow,
    AmountExceedsDynamicPerTxCap,
    AmountExceedsDynamicDailyCap,
    /// The transaction carries no usable ed25519-precompile instruction
    /// attesting this message (missing, malformed, or wrong sysvar).
    MissingAttestation,
}

impl From<BridgeError> for ProgramError {
    fn from(e: BridgeError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for BridgeError {}

// ---------------------------------------------------------------------------
// Message format (BRIDGE.md section 2.3)
// ---------------------------------------------------------------------------

/// Canonical cross-chain event message. Every field is part of the signed
/// hash, so an attestation cannot be replayed across directions, amounts,
/// recipients, or source transactions.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BridgeMessage {
    pub version: u8,
    pub source_chain: u8,
    pub dest_chain: u8,
    pub nonce: u64,
    pub source_mint: Pubkey,
    pub sender: Pubkey,
    pub recipient: Pubkey,
    pub amount: u64,
    pub source_tx: [u8; 64],
    pub source_slot: u64,
    /// Unix timestamp at which this attestation stops being
    /// executable, on the destination chain's Clock. Derived by the
    /// relayer as `source_timestamp + window` (BRIDGE.md 2.4).
    pub expiry_timestamp: i64,
    /// Unix timestamp of the source event (its block time on the
    /// source chain), as observed by every relayer from the source
    /// transaction itself — so all relayers derive the identical
    /// value and sign the identical message.
    pub source_timestamp: i64,
}

/// Fixed-size canonical encoding: 1 + 1 + 1 + 8 + 32 + 32 + 32 + 8 + 64 + 8
/// + 8 + 8 = 203 bytes. Fixed layout means no parsing ambiguity.
pub const MESSAGE_CANONICAL_LEN: usize = 203;

impl BridgeMessage {
    pub fn canonical_bytes(&self) -> [u8; MESSAGE_CANONICAL_LEN] {
        let mut out = [0u8; MESSAGE_CANONICAL_LEN];
        let mut o = 0;
        out[o] = self.version;
        o += 1;
        out[o] = self.source_chain;
        o += 1;
        out[o] = self.dest_chain;
        o += 1;
        out[o..o + 8].copy_from_slice(&self.nonce.to_le_bytes());
        o += 8;
        out[o..o + 32].copy_from_slice(self.source_mint.as_ref());
        o += 32;
        out[o..o + 32].copy_from_slice(self.sender.as_ref());
        o += 32;
        out[o..o + 32].copy_from_slice(self.recipient.as_ref());
        o += 32;
        out[o..o + 8].copy_from_slice(&self.amount.to_le_bytes());
        o += 8;
        out[o..o + 64].copy_from_slice(&self.source_tx);
        o += 64;
        out[o..o + 8].copy_from_slice(&self.source_slot.to_le_bytes());
        o += 8;
        out[o..o + 8].copy_from_slice(&self.expiry_timestamp.to_le_bytes());
        o += 8;
        out[o..o + 8].copy_from_slice(&self.source_timestamp.to_le_bytes());
        out
    }

    /// The message hash relayers sign: sha256(domain_separator || canonical).
    pub fn hash(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(DOMAIN_SEPARATOR);
        h.update(self.canonical_bytes());
        h.finalize().into()
    }
}

/// One relayer's signature over a message hash.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RelayerSignature {
    pub relayer: Pubkey,
    pub signature: [u8; 64],
}

// ---------------------------------------------------------------------------
// Attestation verification (BRIDGE.md sections 2.4, 3.1)
// ---------------------------------------------------------------------------

/// The non-signature attestation checks, shared by the on-chain
/// (precompile) and off-chain (dalek) verification paths: version,
/// chain binding, and the expiry rules below.
///
/// Expiry is timestamp-based (debate round 7 finding A-F1, fixed
/// 2026-11-20). The V1 rule compared the destination chain's slot
/// against an expiry derived on the source chain; the two chains'
/// slot sequences are independent, so one direction's attestations
/// always expired once slot heights diverged and the other
/// direction's never fired. Both chains are Agave and both Clocks
/// expose `unix_timestamp`, so expiry is now denominated in the
/// one quantity both chains share: time. The destination Clock is
/// trusted as that chain's time: on Solana it is a stake-weighted
/// median of validator votes, but on a small validator set — and
/// above all on Machine's current single-node localnet — it is
/// only as honest as that set's own clocks, and every check below
/// shifts with it. That trust is accepted deliberately (the cap
/// windows stay slot-based: they are precision accounting,
/// continuously monetizable if skewed; expiry is a coarse
/// one-shot freshness check), and the deployment gate in
/// BRIDGE.md section 5 keeps the single-node net from being a
/// bridge endpoint.
///
/// A positivity pre-check runs before the four checks: neither
/// the signed `source_timestamp` nor the destination clock reading
/// may be <= 0. Every check below bounds a *difference* of
/// timestamps, so a triple sitting entirely in a non-positive
/// range — or a destination clock that is not reporting time at
/// all — satisfied all four while naming no real time; the
/// relayer already vetoes non-positive block times when it builds
/// a message, and the shared validator holds the same invariant
/// (external seats 2026-11-21). With the pre-check and check 1,
/// `expiry_timestamp` is necessarily positive as well, so it
/// needs no condition of its own.
///
/// Four checks, all against the signed fields plus the destination
/// clock (`current_timestamp`):
/// 1. Lifetime bound: `expiry_timestamp - source_timestamp` must
///    lie in (0, MAX_ATTESTATION_LIFETIME_SECS] — the source-basis
///    bound against a permanent bearer instrument (debate round
///    2026-11-18, A-2/B-F2), restated in seconds.
/// 2. Expiry: the destination time must not be past
///    `expiry_timestamp`.
/// 3. Destination ceiling: `expiry_timestamp` may not lie more than
///    MAX_ATTESTATION_LIFETIME_SECS ahead of the destination time,
///    so even a quorum-signed far-future expiry is useless.
/// 4. Post-dating: `source_timestamp` may not lie more than
///    MAX_SOURCE_TIMESTAMP_SKEW_SECS ahead of the destination time.
///    This bounds future-stamping to the skew allowance (a message
///    may become executable up to that allowance before its claimed
///    source time); check 3 is what caps the total span of
///    destination time over which any message can execute.
pub fn validate_message(
    message: &BridgeMessage,
    expected_source: u8,
    expected_dest: u8,
    current_timestamp: i64,
) -> Result<(), BridgeError> {
    if message.version != MESSAGE_VERSION {
        return Err(BridgeError::InvalidVersion);
    }
    if message.source_chain != expected_source {
        return Err(BridgeError::WrongSourceChain);
    }
    if message.dest_chain != expected_dest {
        return Err(BridgeError::WrongDestChain);
    }
    // Positivity pre-check (see the doc comment): the four checks
    // below bound differences only, so without this a non-positive
    // timestamp range validates.
    if message.source_timestamp <= 0 || current_timestamp <= 0 {
        return Err(BridgeError::ExpiredAttestation);
    }
    match message.expiry_timestamp.checked_sub(message.source_timestamp) {
        Some(lifetime) if lifetime > 0 && lifetime <= MAX_ATTESTATION_LIFETIME_SECS => {}
        _ => return Err(BridgeError::ExpiredAttestation),
    }
    if current_timestamp > message.expiry_timestamp {
        return Err(BridgeError::ExpiredAttestation);
    }
    match message.expiry_timestamp.checked_sub(current_timestamp) {
        Some(ahead) if ahead <= MAX_ATTESTATION_LIFETIME_SECS => {}
        _ => return Err(BridgeError::ExpiredAttestation),
    }
    match message.source_timestamp.checked_sub(current_timestamp) {
        Some(ahead) if ahead <= MAX_SOURCE_TIMESTAMP_SKEW_SECS => {}
        _ => return Err(BridgeError::ExpiredAttestation),
    }
    Ok(())
}

/// Verify a quorum attestation over `message` (off-chain path, used by
/// the relayer to re-verify peer attestations before signing).
///
/// Checks, in order: version, chain binding, expiry against the
/// current destination timestamp, then each signature (relayer must
/// be registered, signature must verify over the message hash, each
/// relayer counted at most once), and finally that distinct valid
/// signatures reach `quorum`.
pub fn verify_attestation(
    message: &BridgeMessage,
    signatures: &[RelayerSignature],
    relayers: &[Pubkey; MAX_RELAYERS],
    quorum: u8,
    expected_source: u8,
    expected_dest: u8,
    current_timestamp: i64,
) -> Result<(), BridgeError> {
    validate_message(message, expected_source, expected_dest, current_timestamp)?;

    // Debate round 2026-11-18 (A-3/B-F3): a zero (or impossible)
    // quorum must never verify — with quorum 0 an empty signature
    // list passed this check. These shared verifiers are the last
    // line for caller-supplied and deserialized values alike.
    if quorum == 0 || quorum as usize > MAX_RELAYERS {
        return Err(BridgeError::QuorumNotReached);
    }

    let hash = message.hash();
    let mut distinct_valid: u8 = 0;
    let mut seen = [false; MAX_RELAYERS];

    for sig in signatures {
        // Find the relayer's index in the registered set.
        let idx = relayers
            .iter()
            .position(|r| r == &sig.relayer)
            .ok_or(BridgeError::UnknownRelayer)?;
        if seen[idx] {
            return Err(BridgeError::DuplicateRelayerSignature);
        }
        seen[idx] = true;

        let verifying_key =
            VerifyingKey::from_bytes(&sig.relayer.to_bytes()).map_err(|_| BridgeError::InvalidSignature)?;
        let signature = Signature::from_bytes(&sig.signature);
        verifying_key
            .verify_strict(&hash, &signature)
            .map_err(|_| BridgeError::InvalidSignature)?;
        distinct_valid = distinct_valid.saturating_add(1);
    }

    if distinct_valid < quorum {
        return Err(BridgeError::QuorumNotReached);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// On-chain attestation verification: ed25519 precompile + instructions
// sysvar (BRIDGE.md sections 2.4, 3.1)
// ---------------------------------------------------------------------------

/// Build the ed25519-program instruction that carries `signatures` over
/// `message_hash` for the bridge programs to verify against. One
/// instruction holds every signature; all entries point at a single
/// copy of the message. Layout follows the ed25519 program's format:
/// a 2-byte header (count, padding), one 14-byte offsets record per
/// signature, then the payload region.
pub fn build_ed25519_instruction(
    message_hash: &[u8; 32],
    signatures: &[RelayerSignature],
) -> solana_program::instruction::Instruction {
    use solana_program::instruction::Instruction;
    // Debate round 2026-11-18 (A-7/B-F6): the count is written as a
    // u8 and the offsets as u16s, so an oversized set silently
    // truncated into a malformed instruction. The only legitimate
    // input is a registered relayer set's signatures; refuse more
    // loudly instead of corrupting them.
    assert!(
        signatures.len() <= MAX_RELAYERS,
        "ed25519 attestation instruction holds at most MAX_RELAYERS signatures"
    );
    let n = signatures.len();
    let data_start = 2 + 14 * n;
    let mut data = Vec::with_capacity(data_start + 96 * n + 32);
    data.push(n as u8);
    data.push(0);
    let message_offset = data_start + 96 * n;
    for i in 0..n {
        let pubkey_offset = data_start + 96 * i;
        let signature_offset = pubkey_offset + 32;
        let fields: [u16; 7] = [
            signature_offset as u16,
            u16::MAX,
            pubkey_offset as u16,
            u16::MAX,
            message_offset as u16,
            32,
            u16::MAX,
        ];
        for f in fields {
            data.extend_from_slice(&f.to_le_bytes());
        }
    }
    for sig in signatures {
        data.extend_from_slice(sig.relayer.as_ref());
        data.extend_from_slice(&sig.signature);
    }
    data.extend_from_slice(message_hash);
    Instruction {
        program_id: solana_program::ed25519_program::ID,
        accounts: vec![],
        data,
    }
}

/// Resolve one data slice referenced by an ed25519 offsets record.
/// Only self-references resolve: index `u16::MAX` or the ed25519
/// instruction's own index, reading from that instruction's data —
/// the only layout `build_ed25519_instruction` produces. Cross-
/// instruction references are rejected rather than half-supported.
fn ed25519_slice<'a>(
    own_data: &'a [u8],
    own_index: usize,
    index: u16,
    offset: u16,
    size: usize,
) -> Result<&'a [u8], BridgeError> {
    if index == u16::MAX || index as usize == own_index {
        let start = offset as usize;
        return own_data
            .get(start..start + size)
            .ok_or(BridgeError::MissingAttestation);
    }
    Err(BridgeError::MissingAttestation)
}

/// On-chain attestation verification (the Solana-standard path).
///
/// The transaction must carry, before the bridge instruction, an
/// ed25519-program instruction attesting `message`'s hash. The runtime
/// verifies those signatures itself before any instruction executes
/// (an invalid signature fails the whole transaction), so this function
/// only establishes the binding: it parses every preceding ed25519
/// instruction from the instructions sysvar and requires that at least
/// `quorum` DISTINCT registered relayers signed exactly this message's
/// hash. Entries by unregistered keys are ignored — they cannot help
/// reach quorum. Callers run `validate_message` first.
/// Bounds-checked instructions-sysvar parsing. The upstream
/// helpers (`load_current_index_checked`,
/// `load_instruction_at_checked`) slice the sysvar data without
/// length checks and panic on malformed input — found by fuzzing
/// (bridge/fuzz, 2026-11-14): empty sysvar data panics inside
/// `load_current_index` at `data[len - 2..len]`. The sysvar is
/// transaction-supplied data, so every read here is checked;
/// malformed shapes return None, which the caller maps to
/// MissingAttestation. Layout: u16 instruction count, count × u16
/// offsets, the serialized instructions, and the current
/// instruction index as a u16 at the very end. A serialized
/// instruction is: u16 account count, per account (u8 flags +
/// 32-byte pubkey), 32-byte program id, u16 data length, data.
fn sysvar_current_index(data: &[u8]) -> Option<usize> {
    if data.len() < 2 {
        return None;
    }
    let bytes: [u8; 2] = data[data.len() - 2..].try_into().ok()?;
    Some(u16::from_le_bytes(bytes) as usize)
}

fn sysvar_instruction_at(data: &[u8], index: usize) -> Option<(Pubkey, &[u8])> {
    fn take<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
        let end = pos.checked_add(n)?;
        let slice = data.get(*pos..end)?;
        *pos = end;
        Some(slice)
    }
    if data.len() < 2 {
        return None;
    }
    let count = u16::from_le_bytes(data[0..2].try_into().ok()?) as usize;
    if index >= count {
        return None;
    }
    let offsets_end = 2usize.checked_add(count.checked_mul(2)?)?;
    if data.len() < offsets_end {
        return None;
    }
    let off_pos = 2 + index * 2;
    let start = u16::from_le_bytes(data[off_pos..off_pos + 2].try_into().ok()?) as usize;
    let mut pos = start;
    let num_accounts =
        u16::from_le_bytes(take(data, &mut pos, 2)?.try_into().ok()?) as usize;
    for _ in 0..num_accounts {
        take(data, &mut pos, 1)?; // flags
        take(data, &mut pos, 32)?; // account pubkey
    }
    let program_id = Pubkey::new_from_array(take(data, &mut pos, 32)?.try_into().ok()?);
    let data_len = u16::from_le_bytes(take(data, &mut pos, 2)?.try_into().ok()?) as usize;
    let ix_data = take(data, &mut pos, data_len)?;
    Some((program_id, ix_data))
}

pub fn verify_precompile_attestation(
    message: &BridgeMessage,
    relayers: &[Pubkey; MAX_RELAYERS],
    quorum: u8,
    instructions_sysvar: &solana_program::account_info::AccountInfo,
) -> Result<(), BridgeError> {
    use solana_program::sysvar::instructions as ix_sysvar;
    // Debate round 2026-11-18 (A-3/B-F3): quorum 0 verified with no
    // ed25519 instruction in the transaction at all.
    if quorum == 0 || quorum as usize > MAX_RELAYERS {
        return Err(BridgeError::QuorumNotReached);
    }
    if *instructions_sysvar.key != ix_sysvar::ID {
        return Err(BridgeError::MissingAttestation);
    }
    let sysvar_data = instructions_sysvar
        .try_borrow_data()
        .map_err(|_| BridgeError::MissingAttestation)?;
    let current_index =
        sysvar_current_index(&sysvar_data).ok_or(BridgeError::MissingAttestation)?;
    let hash = message.hash();
    let mut seen = [false; MAX_RELAYERS];
    let mut distinct: u8 = 0;
    let mut saw_hash_entry = false;

    for idx in 0..current_index {
        let Some((program_id, data)) = sysvar_instruction_at(&sysvar_data, idx) else {
            return Err(BridgeError::MissingAttestation);
        };
        if program_id != solana_program::ed25519_program::ID {
            continue;
        }
        if data.len() < 2 || data[1] != 0 {
            continue;
        }
        let count = data[0] as usize;
        if data.len() < 2 + 14 * count {
            continue;
        }
        for i in 0..count {
            let base = 2 + 14 * i;
            let field = |j: usize| -> u16 {
                u16::from_le_bytes([data[base + 2 * j], data[base + 2 * j + 1]])
            };
            let (sig_off, sig_ix) = (field(0), field(1));
            let (pk_off, pk_ix) = (field(2), field(3));
            let (msg_off, msg_size, msg_ix) = (field(4), field(5), field(6));
            // The signature bytes are the precompile's concern; only
            // their location must be well-formed.
            if ed25519_slice(data, idx, sig_ix, sig_off, 64).is_err() {
                continue;
            }
            let Ok(pk_bytes) = ed25519_slice(data, idx, pk_ix, pk_off, 32)
            else {
                continue;
            };
            let Ok(msg_bytes) =
                ed25519_slice(data, idx, msg_ix, msg_off, msg_size as usize)
            else {
                continue;
            };
            if msg_bytes != hash.as_slice() {
                continue;
            }
            saw_hash_entry = true;
            let pubkey = Pubkey::new_from_array(pk_bytes.try_into().map_err(|_| BridgeError::MissingAttestation)?);
            if let Some(pos) = relayers.iter().position(|r| r == &pubkey) {
                if !seen[pos] {
                    seen[pos] = true;
                    distinct = distinct.saturating_add(1);
                }
            }
        }
    }

    if distinct >= quorum {
        return Ok(());
    }
    if distinct > 0 {
        return Err(BridgeError::QuorumNotReached);
    }
    if saw_hash_entry {
        // Signatures over this hash exist, but none are from the
        // registered relayer set.
        return Err(BridgeError::UnknownRelayer);
    }
    Err(BridgeError::MissingAttestation)
}

// ---------------------------------------------------------------------------
// Caps (BRIDGE.md section 4.4)
// ---------------------------------------------------------------------------

/// Daily usage tracker. The window is the slot-day number
/// (`slot / SLOTS_PER_DAY`); the first operation in a new window resets
/// the counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize, Default)]
pub struct DailyUsage {
    pub window_start: u64,
    pub used: u64,
}

impl DailyUsage {
    /// Check `amount` against the absolute caps and return the updated
    /// usage. Pure function — fully unit-testable.
    pub fn check_and_record(
        &self,
        amount: u64,
        per_tx_cap: u64,
        daily_cap: u64,
        current_slot: u64,
    ) -> Result<DailyUsage, BridgeError> {
        if amount == 0 {
            return Err(BridgeError::InvalidAmount);
        }
        if amount > per_tx_cap {
            return Err(BridgeError::AmountExceedsPerTxCap);
        }
        let window = current_slot / SLOTS_PER_DAY;
        let used = if window == self.window_start {
            self.used
        } else {
            0
        };
        let new_used = used.checked_add(amount).ok_or(BridgeError::ArithmeticOverflow)?;
        if new_used > daily_cap {
            return Err(BridgeError::AmountExceedsDailyCap);
        }
        Ok(DailyUsage {
            window_start: window,
            used: new_used,
        })
    }
}

/// The dynamic percentage layer (BRIDGE.md section 4.4): a single
/// operation may move at most `DYNAMIC_PER_TX_BPS` of the reference
/// supply, and one window's total at most `DYNAMIC_DAILY_BPS` of it.
///
/// Enforced by the Solana-side lock program on unlocks, where the
/// reference supply is the vault's locked total — an independent
/// quantity built up by deposits, which are not percentage-capped.
/// It is deliberately NOT applied to Machine-side mints: the wrapped
/// supply is created by those very mints, so a percentage-of-supply
/// rule there has no independent reference (any first mint would be
/// 100% of the supply it creates). Mint-side flow is bounded by the
/// absolute caps and the quorum instead. `window_total_after` is the
/// caller's daily usage including this operation, as returned by
/// `DailyUsage::check_and_record`.
pub fn check_dynamic_caps(
    amount: u64,
    window_total_after: u64,
    reference_supply: u64,
) -> Result<(), BridgeError> {
    check_dynamic_caps_with_bps(
        amount,
        window_total_after,
        reference_supply,
        DYNAMIC_PER_TX_BPS,
        DYNAMIC_DAILY_BPS,
    )
}

/// The dynamic-cap check with explicit limits, so the unlock path
/// can pass the tightened post-unpause limits (see
/// `LockConfig::dynamic_bps`) without a second implementation.
pub fn check_dynamic_caps_with_bps(
    amount: u64,
    window_total_after: u64,
    reference_supply: u64,
    per_tx_bps: u64,
    daily_bps: u64,
) -> Result<(), BridgeError> {
    if (amount as u128) * (BPS_DENOMINATOR as u128)
        > (reference_supply as u128) * (per_tx_bps as u128)
    {
        return Err(BridgeError::AmountExceedsDynamicPerTxCap);
    }
    if (window_total_after as u128) * (BPS_DENOMINATOR as u128)
        > (reference_supply as u128) * (daily_bps as u128)
    {
        return Err(BridgeError::AmountExceedsDynamicDailyCap);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Authority checks (BRIDGE.md sections 3.3, 4.3)
// ---------------------------------------------------------------------------

/// Count how many of `signer_pubkeys` are members of `set`, each counted
/// at most once.
pub fn count_signers_in_set(signer_pubkeys: &[Pubkey], set: &[Pubkey]) -> u8 {
    let mut count: u8 = 0;
    // The largest set we ever check has MAX_RELAYERS entries.
    let mut seen = [false; MAX_RELAYERS];
    for signer in signer_pubkeys {
        if let Some(idx) = set.iter().position(|m| m == signer) {
            if idx < seen.len() && !seen[idx] {
                seen[idx] = true;
                count = count.saturating_add(1);
            }
        }
    }
    count
}

/// Admin multisig check: at least `threshold` distinct admins signed.
pub fn verify_admins(
    signer_pubkeys: &[Pubkey],
    admins: &[Pubkey; MAX_ADMINS],
    threshold: u8,
) -> Result<(), BridgeError> {
    // Debate round 2026-11-18 (A-3/B-F3): threshold 0 authorized
    // with zero signers.
    if threshold == 0 || threshold as usize > MAX_ADMINS {
        return Err(BridgeError::Unauthorized);
    }
    if count_signers_in_set(signer_pubkeys, admins) < threshold {
        return Err(BridgeError::Unauthorized);
    }
    Ok(())
}

/// Validate a member set (relayers or admins) at initialization or
/// rotation: no default (unset) keys and no duplicates. One shared
/// validator on purpose — `Initialize` previously checked only for
/// default keys while `SetRelayers` also rejected duplicates, so a
/// config could be initialized with a silently weaker set than its
/// size suggests (self-audit F-3, 2026-11-16). Error mapping matches
/// the historical `SetRelayers` behavior exactly: a default key is
/// `InvalidState`, a duplicate is `DuplicateRelayerSignature`.
pub fn validate_member_set(members: &[Pubkey]) -> Result<(), BridgeError> {
    for (i, m) in members.iter().enumerate() {
        if m == &Pubkey::default() {
            return Err(BridgeError::InvalidState);
        }
        if members[..i].contains(m) {
            return Err(BridgeError::DuplicateRelayerSignature);
        }
    }
    Ok(())
}

/// Verify that `payer` is the recorded upgrade authority of
/// `program_id`, using the program's ProgramData account under the
/// BPF upgradeable loader. `Initialize` in both bridge programs runs
/// this check: initialization is bound to whoever deployed the
/// program, so a stranger cannot seize a freshly deployed bridge's
/// config in the deployment window (self-audit F-1, 2026-11-16).
///
/// The account must be exactly the canonical ProgramData address for
/// this program, owned by the upgradeable loader, and hold
/// `UpgradeableLoaderState::ProgramData` metadata with an upgrade
/// authority present (not `None` — an immutable program has no one
/// authorized to initialize a bridge against it) equal to `payer`.
///
/// The metadata is the loader's bincode layout, parsed by hand with
/// every read bounds-checked (bridge-common takes no bincode
/// dependency): a 4-byte variant tag (ProgramData = 3), the 8-byte
/// last-modified slot, a 1-byte authority option tag, and the
/// 32-byte authority when present — 45 bytes total
/// (`UpgradeableLoaderState::size_of_programdata_metadata`).
#[allow(deprecated)]
pub fn verify_upgrade_authority(
    program_id: &Pubkey,
    programdata_account: &AccountInfo,
    payer: &Pubkey,
) -> Result<(), BridgeError> {
    use solana_program::bpf_loader_upgradeable;
    let expected = bpf_loader_upgradeable::get_program_data_address(program_id);
    if *programdata_account.key != expected {
        return Err(BridgeError::InvalidAccountData);
    }
    if *programdata_account.owner != bpf_loader_upgradeable::id() {
        return Err(BridgeError::InvalidAccountData);
    }
    let data = programdata_account
        .try_borrow_data()
        .map_err(|_| BridgeError::InvalidAccountData)?;
    // Variant tags follow the enum's declaration order in
    // solana-loader-v3-interface: Uninitialized 0, Buffer 1,
    // Program 2, ProgramData 3.
    const PROGRAMDATA_METADATA_LEN: usize = 45;
    const VARIANT_PROGRAM_DATA: u32 = 3;
    if data.len() < PROGRAMDATA_METADATA_LEN
        || u32::from_le_bytes(data[0..4].try_into().unwrap()) != VARIANT_PROGRAM_DATA
    {
        return Err(BridgeError::InvalidAccountData);
    }
    // Authority option tag at byte 12 (after tag + slot). Anything
    // other than 1 (Some) — including 0 (None, an immutable program)
    // — means there is no upgrade authority to bind to.
    if data[12] != 1 {
        return Err(BridgeError::InvalidAccountData);
    }
    let authority = Pubkey::new_from_array(data[13..45].try_into().unwrap());
    if authority != *payer {
        return Err(BridgeError::Unauthorized);
    }
    Ok(())
}

/// Pause authority: 2-of-3 admins OR any 2 of 5 relayers. Unpause is
/// admins-only (see module docs — BRIDGE.md 4.3).
pub fn verify_pause_authority(
    signer_pubkeys: &[Pubkey],
    admins: &[Pubkey; MAX_ADMINS],
    admin_threshold: u8,
    relayers: &[Pubkey; MAX_RELAYERS],
) -> Result<(), BridgeError> {
    // Debate round 2026-11-18 (A-3/B-F3): admin_threshold 0
    // authorized with zero signers.
    if admin_threshold == 0 || admin_threshold as usize > MAX_ADMINS {
        return Err(BridgeError::Unauthorized);
    }
    let admin_count = count_signers_in_set(signer_pubkeys, admins);
    if admin_count >= admin_threshold {
        return Ok(());
    }
    let relayer_count = count_signers_in_set(signer_pubkeys, relayers);
    if relayer_count >= PAUSE_RELAYER_THRESHOLD {
        return Ok(());
    }
    Err(BridgeError::Unauthorized)
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Relayer set + quorum, shared by both programs.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RelayerSet {
    pub relayers: [Pubkey; MAX_RELAYERS],
    pub quorum: u8,
}

/// Admin multisig, shared by both programs.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct AdminSet {
    pub admins: [Pubkey; MAX_ADMINS],
    pub threshold: u8,
}

/// Machine-side bridge config (BRIDGE.md section 2.2).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct MachineConfig {
    pub discriminator: u8,
    pub paused: bool,
    /// Kill-switch: permanent. Once set, no mint can ever happen again.
    pub killed: bool,
    pub relayers: RelayerSet,
    pub admins: AdminSet,
    pub wrapped_mint: Pubkey,
    /// The Solana-side mint this deployment wraps (debate round
    /// 2026-11-18, scope 4, finding A-1/B-F1: the leftover B2).
    /// `MintWrapped` binds `message.source_mint` to this value,
    /// mirroring the Unlock-side binding added in the 2026-11-17
    /// round — without it, a quorum attestation for a deposit of a
    /// different asset sharing the relayer set minted this
    /// deployment's wrapped tokens unbacked.
    pub source_mint: Pubkey,
    pub per_tx_cap: u64,
    pub daily_cap: u64,
    pub daily_minted: DailyUsage,
    /// Wrapped supply currently outstanding (minted minus burned),
    /// tracked on-chain so the Machine side can be reconciled against
    /// the Solana vault's locked total at any slot.
    pub total_outstanding: u64,
    pub bump: u8,
}

impl MachineConfig {
    pub const LEN: usize = 1 + 1 + 1 + (32 * 5 + 1) + (32 * 3 + 1) + 32 + 32 + 8 + 8 + (8 + 8) + 8 + 1;

    pub fn new(
        relayers: [Pubkey; MAX_RELAYERS],
        admins: [Pubkey; MAX_ADMINS],
        wrapped_mint: Pubkey,
        source_mint: Pubkey,
        per_tx_cap: u64,
        daily_cap: u64,
        bump: u8,
    ) -> Self {
        Self {
            discriminator: DISCRIMINATOR_CONFIG,
            paused: false,
            killed: false,
            relayers: RelayerSet {
                relayers,
                quorum: QUORUM,
            },
            admins: AdminSet {
                admins,
                threshold: ADMIN_THRESHOLD,
            },
            wrapped_mint,
            source_mint,
            per_tx_cap,
            daily_cap,
            daily_minted: DailyUsage::default(),
            total_outstanding: 0,
            bump,
        }
    }

    pub fn check_mint_allowed(&self) -> Result<(), BridgeError> {
        if self.killed {
            return Err(BridgeError::BridgeKilled);
        }
        if self.paused {
            return Err(BridgeError::BridgePaused);
        }
        Ok(())
    }

    pub fn check_burn_allowed(&self) -> Result<(), BridgeError> {
        // The kill-switch stops minting only; burns (the exit path) stay
        // open so users can always leave. Pause stops everything.
        if self.paused {
            return Err(BridgeError::BridgePaused);
        }
        Ok(())
    }
}

/// Solana-side lock config (BRIDGE.md section 2.1).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LockConfig {
    pub discriminator: u8,
    pub paused: bool,
    pub relayers: RelayerSet,
    pub admins: AdminSet,
    pub source_mint: Pubkey,
    pub vault: Pubkey,
    pub vault_authority_bump: u8,
    pub per_tx_cap: u64,
    pub daily_cap: u64,
    pub vault_ceiling: u64,
    pub total_locked: u64,
    pub daily_unlocked: DailyUsage,
    /// Slot until which the tightened dynamic caps apply (set by
    /// every unpause; 0 = not tightened). See `dynamic_bps`.
    pub tightened_until_slot: u64,
    pub bump: u8,
}

impl LockConfig {
    pub const LEN: usize =
        1 + 1 + (32 * 5 + 1) + (32 * 3 + 1) + 32 + 32 + 1 + 8 + 8 + 8 + 8 + (8 + 8) + 8 + 1;

    pub fn new(
        relayers: [Pubkey; MAX_RELAYERS],
        admins: [Pubkey; MAX_ADMINS],
        source_mint: Pubkey,
        vault: Pubkey,
        vault_authority_bump: u8,
        per_tx_cap: u64,
        daily_cap: u64,
        vault_ceiling: u64,
        bump: u8,
    ) -> Self {
        Self {
            discriminator: DISCRIMINATOR_CONFIG,
            paused: false,
            relayers: RelayerSet {
                relayers,
                quorum: QUORUM,
            },
            admins: AdminSet {
                admins,
                threshold: ADMIN_THRESHOLD,
            },
            source_mint,
            vault,
            vault_authority_bump,
            per_tx_cap,
            daily_cap,
            vault_ceiling,
            total_locked: 0,
            daily_unlocked: DailyUsage::default(),
            tightened_until_slot: 0,
            bump,
        }
    }

    pub fn check_active(&self) -> Result<(), BridgeError> {
        if self.paused {
            return Err(BridgeError::BridgePaused);
        }
        Ok(())
    }

    /// The dynamic-cap limits in force at `slot`: the tightened
    /// pair while the post-unpause recovery window is running, the
    /// normal pair afterwards. Pure function of the stored deadline,
    /// so the relaxation needs no transaction and cannot be skipped.
    pub fn dynamic_bps(&self, slot: u64) -> (u64, u64) {
        if slot < self.tightened_until_slot {
            (TIGHTENED_PER_TX_BPS, TIGHTENED_DAILY_BPS)
        } else {
            (DYNAMIC_PER_TX_BPS, DYNAMIC_DAILY_BPS)
        }
    }
}

/// One processed cross-chain nonce. The PDA's existence IS the replay
/// protection; the stored fields are audit data.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct NonceRecord {
    pub discriminator: u8,
    pub nonce: u64,
    pub source_tx: [u8; 64],
    pub amount: u64,
    pub slot: u64,
}

impl NonceRecord {
    pub const LEN: usize = 1 + 8 + 64 + 8 + 8;
}

/// One recorded deposit (lock side) or burn (Machine side). The PDA's
/// existence prevents the same (direction, nonce) event being recorded
/// twice.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EventRecord {
    pub discriminator: u8,
    pub nonce: u64,
    pub amount: u64,
    pub destination: Pubkey,
    pub slot: u64,
}

impl EventRecord {
    pub const LEN: usize = 1 + 8 + 8 + 32 + 8;
}

/// A permanent, non-transferable proof that one crossing completed,
/// written by the destination-side program (MintWrapped on Machine,
/// Unlock on Solana) as part of the same transaction that moves the
/// value. The PDA is derived from (direction, nonce), so anyone can
/// locate the receipt for any crossing and verify it against the
/// message: if this account exists with these fields, the crossing
/// happened. The destination transaction's own signature cannot be
/// stored — a program cannot observe it — so the binding to the
/// destination transaction is made off-chain by indexers, which can
/// resolve this PDA's creating transaction. Everything the program
/// itself can prove is here.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CrossingReceipt {
    pub discriminator: u8,
    /// Direction of the crossing (DIR_* constants).
    pub direction: u8,
    pub nonce: u64,
    /// Hash of the canonical BridgeMessage that was attested.
    pub message_hash: [u8; 32],
    /// Source-chain transaction identifier carried by the message.
    pub source_tx: [u8; 64],
    pub amount: u64,
    /// Who received the minted / unlocked tokens.
    pub recipient: Pubkey,
    /// Destination-chain slot at completion.
    pub slot: u64,
    /// Destination-chain unix timestamp at completion.
    pub timestamp: i64,
}

impl CrossingReceipt {
    pub const LEN: usize = 1 + 1 + 8 + 32 + 64 + 8 + 32 + 8 + 8;
}

// ---------------------------------------------------------------------------
// PDA helpers
// ---------------------------------------------------------------------------

pub fn config_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_CONFIG], program_id)
}

pub fn nonce_pda(program_id: &Pubkey, direction: u8, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_NONCE, &[direction], &nonce.to_le_bytes()], program_id)
}

pub fn receipt_pda(program_id: &Pubkey, direction: u8, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_RECEIPT, &[direction], &nonce.to_le_bytes()], program_id)
}

/// Replay-protection gate shared by every value-moving path (deposit,
/// burn, mint, unlock): the account supplied for (direction, nonce)
/// must be exactly that nonce's PDA, and it must not already be owned
/// by the program — ownership means a record was created there, i.e.
/// this nonce has already been processed. Returns the PDA bump for
/// the caller's create-account seeds.
///
/// This check lives in one shared helper on purpose:
/// `verify_precompile_attestation` verifies signatures only, so the
/// nonce check is a separate step a caller could forget. An external
/// review (2026-11-15) flagged that split; the four processors
/// previously each inlined the check. Routing all of them through
/// this function makes the gate impossible to half-apply.
pub fn check_nonce_unprocessed(
    nonce_account: &AccountInfo,
    program_id: &Pubkey,
    direction: u8,
    nonce: u64,
) -> Result<u8, BridgeError> {
    let (pda, bump) = nonce_pda(program_id, direction, nonce);
    if *nonce_account.key != pda {
        return Err(BridgeError::InvalidAccountData);
    }
    if *nonce_account.owner == *program_id {
        return Err(BridgeError::NonceAlreadyProcessed);
    }
    Ok(bump)
}

pub fn authority_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_AUTHORITY], program_id)
}

// ---------------------------------------------------------------------------
// Instructions (shared wire format)
// ---------------------------------------------------------------------------

/// Machine-side instructions (BRIDGE.md section 2.2).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum MachineInstruction {
    Initialize {
        relayers: [Pubkey; MAX_RELAYERS],
        admins: [Pubkey; MAX_ADMINS],
        wrapped_mint: Pubkey,
        source_mint: Pubkey,
        per_tx_cap: u64,
        daily_cap: u64,
    },
    /// Attestation signatures travel in the transaction's ed25519
    /// precompile instruction, not in this payload.
    MintWrapped {
        message: BridgeMessage,
    },
    Burn {
        amount: u64,
        destination: Pubkey,
        nonce: u64,
    },
    Pause,
    Unpause,
    SetCap {
        per_tx_cap: u64,
        daily_cap: u64,
    },
    SetRelayers {
        relayers: [Pubkey; MAX_RELAYERS],
    },
    /// One-way kill-switch: permanently disables minting. Burns stay open.
    KillSwitch,
}

/// Solana-side instructions (BRIDGE.md section 2.1).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum LockInstruction {
    Initialize {
        relayers: [Pubkey; MAX_RELAYERS],
        admins: [Pubkey; MAX_ADMINS],
        source_mint: Pubkey,
        vault: Pubkey,
        per_tx_cap: u64,
        daily_cap: u64,
        vault_ceiling: u64,
    },
    Deposit {
        amount: u64,
        destination: Pubkey,
        nonce: u64,
    },
    /// Attestation signatures travel in the transaction's ed25519
    /// precompile instruction, not in this payload.
    Unlock {
        message: BridgeMessage,
    },
    Pause,
    Unpause,
    SetCap {
        per_tx_cap: u64,
        daily_cap: u64,
    },
    SetRelayers {
        relayers: [Pubkey; MAX_RELAYERS],
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    fn test_message() -> BridgeMessage {
        BridgeMessage {
            version: MESSAGE_VERSION,
            source_chain: CHAIN_SOLANA,
            dest_chain: CHAIN_MACHINE,
            nonce: 7,
            source_mint: Pubkey::new_unique(),
            sender: Pubkey::new_unique(),
            recipient: Pubkey::new_unique(),
            amount: 1_000_000,
            source_tx: [9u8; 64],
            source_slot: 1234,
            expiry_timestamp: 1_700_010_000,
            source_timestamp: 1_700_000_000,
        }
    }

    fn make_relayers() -> ([SigningKey; 5], [Pubkey; 5]) {
        let mut rng = OsRng;
        let mut keys = Vec::new();
        let mut pubs = Vec::new();
        for _ in 0..5 {
            let sk = SigningKey::generate(&mut rng);
            pubs.push(Pubkey::new_from_array(sk.verifying_key().to_bytes()));
            keys.push(sk);
        }
        (keys.try_into().unwrap(), pubs.try_into().unwrap())
    }

    fn sign_all(msg: &BridgeMessage, keys: &[SigningKey; 5], n: usize) -> Vec<RelayerSignature> {
        let hash = msg.hash();
        keys.iter()
            .take(n)
            .map(|sk| RelayerSignature {
                relayer: Pubkey::new_from_array(sk.verifying_key().to_bytes()),
                signature: sk.sign(&hash).to_bytes(),
            })
            .collect()
    }

    #[test]
    fn canonical_encoding_is_stable_and_203_bytes() {
        let msg = test_message();
        let bytes = msg.canonical_bytes();
        assert_eq!(bytes.len(), MESSAGE_CANONICAL_LEN);
        assert_eq!(bytes[0], MESSAGE_VERSION);
        assert_eq!(bytes[1], CHAIN_SOLANA);
        assert_eq!(bytes[2], CHAIN_MACHINE);
        assert_eq!(u64::from_le_bytes(bytes[3..11].try_into().unwrap()), 7);
        // Same message hashes the same; any field change changes the hash.
        let mut msg2 = msg.clone();
        msg2.amount += 1;
        assert_ne!(msg.hash(), msg2.hash());
        let mut msg3 = msg.clone();
        msg3.nonce += 1;
        assert_ne!(msg.hash(), msg3.hash());
        let mut msg4 = msg.clone();
        msg4.dest_chain = CHAIN_SOLANA;
        assert_ne!(msg.hash(), msg4.hash());
    }

    #[test]
    fn valid_3_of_5_attestation_passes() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let sigs = sign_all(&msg, &keys, 3);
        assert!(verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000).is_ok());
    }

    #[test]
    fn valid_5_of_5_attestation_passes() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let sigs = sign_all(&msg, &keys, 5);
        assert!(verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000).is_ok());
    }

    #[test]
    fn two_of_5_fails_quorum() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let sigs = sign_all(&msg, &keys, 2);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000),
            Err(BridgeError::QuorumNotReached)
        );
    }

    #[test]
    fn duplicate_relayer_signature_rejected() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let mut sigs = sign_all(&msg, &keys, 2);
        sigs.push(sigs[0].clone()); // same relayer twice
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000),
            Err(BridgeError::DuplicateRelayerSignature)
        );
    }

    #[test]
    fn unknown_relayer_rejected() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let mut sigs = sign_all(&msg, &keys, 3);
        sigs[0].relayer = Pubkey::new_unique(); // not in the set
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000),
            Err(BridgeError::UnknownRelayer)
        );
    }

    #[test]
    fn tampered_signature_rejected() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let mut sigs = sign_all(&msg, &keys, 3);
        sigs[1].signature[0] ^= 0xff;
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000),
            Err(BridgeError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_direction_rejected() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let sigs = sign_all(&msg, &keys, 3);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_MACHINE, CHAIN_SOLANA, 1_700_005_000),
            Err(BridgeError::WrongSourceChain)
        );
    }

    #[test]
    fn af1_honest_attestation_survives_diverged_slot_heights() {
        // A-F1 regression (debate round 7): under V1 this exact
        // shape — a Machine -> Solana message with source_slot
        // 50_000 and a source-basis expiry — was rejected at a
        // destination slot of 380_000_000, because the destination
        // chain's independent slot sequence was compared against a
        // source-chain expiry. Expiry is timestamp-based now: the
        // destination's slot height plays no part. The attestation
        // below is fresh (validated 20,000 s after its source event,
        // inside its 40,000 s window) and must pass; the same
        // message validated after its window must fail — in this
        // direction the V1 check never fired at all once the
        // destination's slots lagged the source's.
        let mut msg = test_message();
        msg.source_chain = CHAIN_MACHINE;
        msg.dest_chain = CHAIN_SOLANA;
        msg.source_slot = 50_000;
        msg.source_timestamp = 1_700_000_000;
        msg.expiry_timestamp = 1_700_040_000;
        assert_eq!(
            validate_message(&msg, CHAIN_MACHINE, CHAIN_SOLANA, 1_700_020_000),
            Ok(())
        );
        assert_eq!(
            validate_message(&msg, CHAIN_MACHINE, CHAIN_SOLANA, 1_700_040_001),
            Err(BridgeError::ExpiredAttestation)
        );
    }

    #[test]
    fn af1_far_future_and_postdated_messages_rejected() {
        // A-F1 fix guards: a quorum-signed message whose expiry lies
        // more than MAX_ATTESTATION_LIFETIME_SECS ahead of the
        // destination clock is a bearer instrument even if its
        // internal lifetime is within bounds (its source is stamped
        // far in the future); the post-dating guard rejects it too.
        let mut msg = test_message();
        msg.source_timestamp = 1_700_000_000 + 10 * MAX_ATTESTATION_LIFETIME_SECS;
        msg.expiry_timestamp = msg.source_timestamp + 40_000;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_000_000),
            Err(BridgeError::ExpiredAttestation)
        );
        // Post-dated by just over the skew tolerance, with an expiry
        // the destination ceiling alone would still allow.
        let mut msg = test_message();
        msg.source_timestamp = 1_700_000_000 + MAX_SOURCE_TIMESTAMP_SKEW_SECS + 1;
        msg.expiry_timestamp = msg.source_timestamp + 40_000;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_000_000),
            Err(BridgeError::ExpiredAttestation)
        );
        // Post-dated within the skew tolerance passes (clock drift
        // between two honest chains must not false-reject).
        let mut msg = test_message();
        msg.source_timestamp = 1_700_000_000 + MAX_SOURCE_TIMESTAMP_SKEW_SECS;
        msg.expiry_timestamp = msg.source_timestamp + 40_000;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_000_000),
            Ok(())
        );
    }

    #[test]
    fn af1_destination_ceiling_isolated() {
        // War room 2026-11-21 (F-TEST-1): the destination ceiling
        // (check 3) needs a case no other check can reject. The
        // source is post-dated by exactly the skew tolerance (check
        // 4 passes) and the lifetime is exactly the cap (check 1
        // passes), so only the ceiling decides: at the source time
        // the expiry is exactly MAX ahead of the destination clock
        // and the message passes; one second earlier it is one
        // second too far ahead and the ceiling alone rejects it.
        let mut msg = test_message();
        msg.source_timestamp = 1_700_000_000 + MAX_SOURCE_TIMESTAMP_SKEW_SECS;
        msg.expiry_timestamp = msg.source_timestamp + MAX_ATTESTATION_LIFETIME_SECS;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, msg.source_timestamp),
            Ok(())
        );
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, msg.source_timestamp - 1),
            Err(BridgeError::ExpiredAttestation)
        );
    }

    #[test]
    fn af1_zero_lifetime_rejected() {
        // War room 2026-11-21 (F-TEST-3): expiry == source is a
        // zero lifetime, rejected by check 1's `> 0` alone —
        // validated at the source time itself, so the expiry check
        // (current > expiry) cannot fire and the ceiling and
        // post-dating checks both pass trivially.
        let mut msg = test_message();
        msg.expiry_timestamp = msg.source_timestamp;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, msg.source_timestamp),
            Err(BridgeError::ExpiredAttestation)
        );
    }

    #[test]
    fn af1_nonpositive_source_timestamp_rejected() {
        // External seats 2026-11-21 (ChatGPT + Gemini), reconciled:
        // validate_message had no positivity guard. A source
        // timestamp of 0 passes all four checks when the whole
        // triple sits in a small epoch-relative frame (lifetime
        // 40_000 in (0, MAX]; current 20_000 not past expiry; the
        // expiry only 20_000 ahead of the destination clock; the
        // source behind the clock, which check 4 allows) — the
        // guard is the only rejection.
        let mut msg = test_message();
        msg.source_timestamp = 0;
        msg.expiry_timestamp = 40_000;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, 20_000),
            Err(BridgeError::ExpiredAttestation)
        );
    }

    #[test]
    fn af1_negative_timestamp_range_rejected() {
        // External seats 2026-11-21, reconciled: a fully negative
        // timestamp range satisfies every difference-based check
        // (lifetime 50; destination clock not past expiry; expiry
        // 25 ahead of the clock; source 25 behind it) and validated
        // before the positivity guard.
        let mut msg = test_message();
        msg.source_timestamp = -100;
        msg.expiry_timestamp = -50;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, -75),
            Err(BridgeError::ExpiredAttestation)
        );
    }

    #[test]
    fn af1_nonpositive_current_timestamp_rejected() {
        // External seats 2026-11-21, reconciled: the destination
        // clock reading itself must be positive — a clock at or
        // below 0 is not reporting time, and no freshness decision
        // may rest on it. With a small positive source the four
        // checks all pass at current 0 and -5 (lifetime 40_000;
        // clock not past expiry; expiry ~41_000 ahead, within the
        // ceiling; source ~1_000 ahead, within the skew), so the
        // guard is the only rejection.
        let mut msg = test_message();
        msg.source_timestamp = 1_000;
        msg.expiry_timestamp = 41_000;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, 0),
            Err(BridgeError::ExpiredAttestation)
        );
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, -5),
            Err(BridgeError::ExpiredAttestation)
        );
        // Pinning only (already rejected before the guard, by the
        // destination ceiling): a non-positive clock against an
        // epoch-scale message puts the expiry more than
        // MAX_ATTESTATION_LIFETIME_SECS ahead of the clock.
        let mut msg = test_message();
        msg.expiry_timestamp = msg.source_timestamp + 40_000;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, 0),
            Err(BridgeError::ExpiredAttestation)
        );
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, -5),
            Err(BridgeError::ExpiredAttestation)
        );
    }

    #[test]
    fn af1_smallest_positive_timestamps_still_validate() {
        // Boundary sanity for the positivity guard: it is a
        // `<= 0` rejection, not a minimum-age or epoch floor — the
        // smallest positive source and destination readings, with
        // a lifetime of 40_000, still validate.
        let mut msg = test_message();
        msg.source_timestamp = 1;
        msg.expiry_timestamp = 40_001;
        assert_eq!(
            validate_message(&msg, CHAIN_SOLANA, CHAIN_MACHINE, 1),
            Ok(())
        );
    }

    #[test]
    fn expired_attestation_rejected() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let sigs = sign_all(&msg, &keys, 3);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_010_001),
            Err(BridgeError::ExpiredAttestation)
        );
    }

    #[test]
    fn debate_unbounded_attestation_lifetime_rejected() {
        // Debate round 2026-11-18 (A-2/B-F2): an unbounded expiry
        // was a permanent bearer instrument; an expiry at or before
        // the source time was also accepted. The bound is
        // source-basis: 0 < expiry - source <=
        // MAX_ATTESTATION_LIFETIME_SECS (restated in seconds with
        // the V2 timestamp expiry, 2026-11-20).
        let (keys, pubs) = make_relayers();
        let mut msg = test_message();
        msg.expiry_timestamp = i64::MAX;
        let sigs = sign_all(&msg, &keys, 3);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000),
            Err(BridgeError::ExpiredAttestation)
        );
        let mut msg = test_message();
        msg.expiry_timestamp = msg.source_timestamp - 1;
        let sigs = sign_all(&msg, &keys, 3);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_000_500),
            Err(BridgeError::ExpiredAttestation)
        );
        // Exactly at the cap passes; one second past it fails.
        let mut msg = test_message();
        msg.expiry_timestamp = msg.source_timestamp + MAX_ATTESTATION_LIFETIME_SECS;
        let sigs = sign_all(&msg, &keys, 3);
        assert!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000)
                .is_ok()
        );
        msg.expiry_timestamp += 1;
        let sigs = sign_all(&msg, &keys, 3);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000),
            Err(BridgeError::ExpiredAttestation)
        );
    }

    #[test]
    fn debate_zero_quorum_and_thresholds_rejected() {
        // Debate round 2026-11-18 (A-3/B-F3): quorum/threshold 0
        // verified with no signatures at all.
        let (_keys, pubs) = make_relayers();
        let msg = test_message();
        assert_eq!(
            verify_attestation(&msg, &[], &pubs, 0, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000),
            Err(BridgeError::QuorumNotReached)
        );
        assert_eq!(
            verify_attestation(&msg, &[], &pubs, 6, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000),
            Err(BridgeError::QuorumNotReached)
        );
        let admins = [Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique()];
        assert_eq!(verify_admins(&[], &admins, 0), Err(BridgeError::Unauthorized));
        assert_eq!(
            verify_pause_authority(&[], &admins, 0, &pubs),
            Err(BridgeError::Unauthorized)
        );
    }

    #[test]
    #[should_panic(expected = "at most MAX_RELAYERS signatures")]
    fn debate_ed25519_builder_refuses_oversized_set() {
        // Debate round 2026-11-18 (A-7/B-F6): the count byte silently
        // truncated for >255 signatures (and offsets for far fewer
        // than the format can honestly hold beyond the relayer set).
        let sigs: Vec<RelayerSignature> = (0..6)
            .map(|_| RelayerSignature {
                relayer: Pubkey::new_unique(),
                signature: [1u8; 64],
            })
            .collect();
        let _ = build_ed25519_instruction(&[9u8; 32], &sigs);
    }

    #[test]
    fn wrong_version_rejected() {
        let (keys, pubs) = make_relayers();
        let mut msg = test_message();
        msg.version = 1; // the retired V1 format must not verify
        let sigs = sign_all(&msg, &keys, 3);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 1_700_005_000),
            Err(BridgeError::InvalidVersion)
        );
    }

    #[test]
    fn daily_cap_window_resets() {
        let usage = DailyUsage::default();
        // Window 0: use 800 of 1000 daily cap.
        let usage = usage.check_and_record(800, 10_000, 1_000, 100).unwrap();
        assert_eq!(usage.used, 800);
        // Same window: another 300 exceeds the daily cap.
        assert_eq!(
            usage.check_and_record(300, 10_000, 1_000, 200),
            Err(BridgeError::AmountExceedsDailyCap)
        );
        // Next window (one slot-day later): window resets, 300 is fine.
        let usage = usage
            .check_and_record(300, 10_000, 1_000, SLOTS_PER_DAY + 100)
            .unwrap();
        assert_eq!(usage.used, 300);
        // Per-transaction cap.
        assert_eq!(
            usage.check_and_record(10_001, 10_000, 1_000_000, SLOTS_PER_DAY + 200),
            Err(BridgeError::AmountExceedsPerTxCap)
        );
        // Zero amount.
        assert_eq!(
            usage.check_and_record(0, 10_000, 1_000_000, SLOTS_PER_DAY + 200),
            Err(BridgeError::InvalidAmount)
        );
    }

    #[test]
    fn max_amounts_and_overflow_are_rejected() {
        let usage = DailyUsage::default();
        // u64::MAX against ordinary caps dies at the per-tx layer.
        assert_eq!(
            usage.check_and_record(u64::MAX, 10_000, 1_000_000, 200),
            Err(BridgeError::AmountExceedsPerTxCap)
        );
        // Even with maximal caps, the window accumulator must not
        // wrap: MAX - 10 already used, +11 overflows, +10 lands
        // exactly on u64::MAX and is allowed by a u64::MAX cap.
        let usage = DailyUsage {
            window_start: 7,
            used: u64::MAX - 10,
        };
        assert_eq!(
            usage.check_and_record(11, u64::MAX, u64::MAX, 7 * SLOTS_PER_DAY),
            Err(BridgeError::ArithmeticOverflow)
        );
        let updated = usage
            .check_and_record(10, u64::MAX, u64::MAX, 7 * SLOTS_PER_DAY)
            .unwrap();
        assert_eq!(updated.used, u64::MAX);
    }

    #[test]
    fn dynamic_percentage_caps_bind() {
        // Reference supply 10,000: dynamic per-tx cap is 1% = 100,
        // dynamic daily cap is 5% = 500.
        // Exactly 1% in one operation, 100 total for the window: fine.
        assert!(check_dynamic_caps(100, 100, 10_000).is_ok());
        // 101 in one operation exceeds 1%.
        assert_eq!(
            check_dynamic_caps(101, 101, 10_000),
            Err(BridgeError::AmountExceedsDynamicPerTxCap)
        );
        // A small operation whose window total passes 5% fails on the
        // daily layer even though the operation itself is under 1%.
        assert_eq!(
            check_dynamic_caps(1, 501, 10_000),
            Err(BridgeError::AmountExceedsDynamicDailyCap)
        );
        // Exactly 5% for the window passes.
        assert!(check_dynamic_caps(100, 500, 10_000).is_ok());
    }

    #[test]
    fn receipt_serializes_to_declared_len() {
        let receipt = CrossingReceipt {
            discriminator: DISCRIMINATOR_RECEIPT,
            direction: DIR_SOLANA_TO_MACHINE,
            nonce: 42,
            message_hash: [9u8; 32],
            source_tx: [7u8; 64],
            amount: 1_000,
            recipient: Pubkey::new_from_array([5u8; 32]),
            slot: 123_456,
            timestamp: 1_700_000_000,
        };
        let bytes = borsh::to_vec(&receipt).unwrap();
        assert_eq!(bytes.len(), CrossingReceipt::LEN);
        let back = CrossingReceipt::try_from_slice(&bytes).unwrap();
        assert_eq!(receipt, back);
    }

    #[test]
    fn tightened_dynamic_caps_bind_at_a_quarter() {
        // Reference supply 10,000: tightened per-tx is 0.25% = 25,
        // tightened window total is 1.25% = 125.
        let (ptx, daily) = (TIGHTENED_PER_TX_BPS, TIGHTENED_DAILY_BPS);
        assert!(check_dynamic_caps_with_bps(25, 25, 10_000, ptx, daily).is_ok());
        assert_eq!(
            check_dynamic_caps_with_bps(26, 26, 10_000, ptx, daily),
            Err(BridgeError::AmountExceedsDynamicPerTxCap)
        );
        assert_eq!(
            check_dynamic_caps_with_bps(1, 126, 10_000, ptx, daily),
            Err(BridgeError::AmountExceedsDynamicDailyCap)
        );
        // The same amounts pass under the normal limits — the
        // tightening is a real restriction, not a relabeling.
        assert!(check_dynamic_caps(26, 126, 10_000).is_ok());
    }

    #[test]
    fn lock_config_tightening_window_selects_bps() {
        let (_keys, relayers) = make_relayers();
        let admins = [Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique()];
        let mut cfg = LockConfig::new(
            relayers,
            admins,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            255,
            1000,
            10_000,
            1_000_000,
            254,
        );
        // The config still serializes to exactly its declared LEN
        // with the new field included.
        let bytes = borsh::to_vec(&cfg).unwrap();
        assert_eq!(bytes.len(), LockConfig::LEN);
        // Not tightened: normal limits at every slot.
        assert_eq!(cfg.dynamic_bps(0), (DYNAMIC_PER_TX_BPS, DYNAMIC_DAILY_BPS));
        assert_eq!(
            cfg.dynamic_bps(u64::MAX),
            (DYNAMIC_PER_TX_BPS, DYNAMIC_DAILY_BPS)
        );
        // Tightened until slot 500: tightened limits before it,
        // normal limits from it on — relaxation is automatic.
        cfg.tightened_until_slot = 500;
        assert_eq!(cfg.dynamic_bps(499), (TIGHTENED_PER_TX_BPS, TIGHTENED_DAILY_BPS));
        assert_eq!(cfg.dynamic_bps(500), (DYNAMIC_PER_TX_BPS, DYNAMIC_DAILY_BPS));
    }

    #[test]
    fn ed25519_instruction_layout() {
        let (_keys, pubs) = make_relayers();
        let sigs: Vec<RelayerSignature> = pubs[..3]
            .iter()
            .map(|p| RelayerSignature {
                relayer: *p,
                signature: [7u8; 64],
            })
            .collect();
        let hash = [9u8; 32];
        let ix = build_ed25519_instruction(&hash, &sigs);
        assert_eq!(ix.program_id, solana_program::ed25519_program::ID);
        assert!(ix.accounts.is_empty());
        // Header: count + padding.
        assert_eq!(ix.data[0], 3);
        assert_eq!(ix.data[1], 0);
        // Data region starts at 2 + 14*3 = 44; message after 3*96 bytes.
        let data_start = 44usize;
        let message_offset = data_start + 3 * 96;
        assert_eq!(ix.data.len(), message_offset + 32);
        assert_eq!(&ix.data[message_offset..], &hash);
        for i in 0..3 {
            let base = 2 + 14 * i;
            let field =
                |j: usize| u16::from_le_bytes([ix.data[base + 2 * j], ix.data[base + 2 * j + 1]]);
            assert_eq!(field(0) as usize, data_start + 96 * i + 32); // signature
            assert_eq!(field(1), u16::MAX);
            assert_eq!(field(2) as usize, data_start + 96 * i); // pubkey
            assert_eq!(field(3), u16::MAX);
            assert_eq!(field(4) as usize, message_offset);
            assert_eq!(field(5), 32);
            assert_eq!(field(6), u16::MAX);
            let pk_at = data_start + 96 * i;
            assert_eq!(&ix.data[pk_at..pk_at + 32], pubs[i].as_ref());
        }
    }

    #[test]
    fn admin_threshold_enforced() {
        let admins = [Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique()];
        assert!(verify_admins(&admins[..2], &admins, ADMIN_THRESHOLD).is_ok());
        assert_eq!(
            verify_admins(&admins[..1], &admins, ADMIN_THRESHOLD),
            Err(BridgeError::Unauthorized)
        );
        // Same admin twice does not count twice.
        assert_eq!(
            verify_admins(&[admins[0], admins[0]], &admins, ADMIN_THRESHOLD),
            Err(BridgeError::Unauthorized)
        );
        // A non-admin does not count.
        assert_eq!(
            verify_admins(&[admins[0], Pubkey::new_unique()], &admins, ADMIN_THRESHOLD),
            Err(BridgeError::Unauthorized)
        );
    }

    #[test]
    fn pause_authority_two_relayers_or_two_admins() {
        let admins = [Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique()];
        let (_keys, relayers) = make_relayers();
        // Two admins.
        assert!(verify_pause_authority(&admins[..2], &admins, ADMIN_THRESHOLD, &relayers).is_ok());
        // Two relayers.
        assert!(verify_pause_authority(&relayers[..2], &admins, ADMIN_THRESHOLD, &relayers).is_ok());
        // One relayer only.
        assert_eq!(
            verify_pause_authority(&relayers[..1], &admins, ADMIN_THRESHOLD, &relayers),
            Err(BridgeError::Unauthorized)
        );
        // One admin + one relayer: neither threshold met.
        assert_eq!(
            verify_pause_authority(&[admins[0], relayers[0]], &admins, ADMIN_THRESHOLD, &relayers),
            Err(BridgeError::Unauthorized)
        );
    }

    #[test]
    fn kill_switch_blocks_mint_but_not_burn() {
        let (_keys, relayers) = make_relayers();
        let admins = [Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique()];
        let mut cfg = MachineConfig::new(relayers, admins, Pubkey::new_unique(), Pubkey::new_unique(), 1000, 10_000, 255);
        assert!(cfg.check_mint_allowed().is_ok());
        assert!(cfg.check_burn_allowed().is_ok());
        cfg.killed = true;
        assert_eq!(cfg.check_mint_allowed(), Err(BridgeError::BridgeKilled));
        assert!(cfg.check_burn_allowed().is_ok());
        cfg.paused = true;
        assert_eq!(cfg.check_burn_allowed(), Err(BridgeError::BridgePaused));
    }

    #[test]
    fn config_serializes_to_declared_len() {
        let (_keys, relayers) = make_relayers();
        let admins = [Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique()];
        let cfg = MachineConfig::new(relayers, admins, Pubkey::new_unique(), Pubkey::new_unique(), 1000, 10_000, 255);
        let bytes = borsh::to_vec(&cfg).unwrap();
        assert_eq!(bytes.len(), MachineConfig::LEN);
        let back = MachineConfig::try_from_slice(&bytes).unwrap();
        assert_eq!(cfg, back);
    }
}

#[cfg(test)]
mod sysvar_parser_tests {
    use super::*;
    use solana_program::account_info::AccountInfo;
    use std::{cell::RefCell, rc::Rc};

    fn relayer_pubs() -> [Pubkey; MAX_RELAYERS] {
        [
            Pubkey::new_from_array([11u8; 32]),
            Pubkey::new_from_array([22u8; 32]),
            Pubkey::new_from_array([33u8; 32]),
            Pubkey::new_from_array([44u8; 32]),
            Pubkey::new_from_array([55u8; 32]),
        ]
    }

    fn test_message() -> BridgeMessage {
        BridgeMessage {
            version: MESSAGE_VERSION,
            source_chain: CHAIN_SOLANA,
            dest_chain: CHAIN_MACHINE,
            nonce: 1,
            source_mint: Pubkey::new_from_array([66u8; 32]),
            sender: Pubkey::new_from_array([77u8; 32]),
            recipient: Pubkey::new_from_array([88u8; 32]),
            amount: 1_000,
            source_tx: [9u8; 64],
            source_slot: 100,
            expiry_timestamp: 1_700_010_000,
            source_timestamp: 1_700_000_000,
        }
    }

    /// Serialize instructions into instructions-sysvar bytes: u16
    /// count, count x u16 offsets, each instruction as (u16 account
    /// count = 0, 32-byte program id, u16 data length, data), and the
    /// current instruction index as a trailing u16.
    fn sysvar_bytes(ixs: &[(Pubkey, Vec<u8>)], current_index: u16) -> Vec<u8> {
        let mut offsets = Vec::with_capacity(ixs.len());
        let mut pos = 2 + 2 * ixs.len();
        for (_, data) in ixs {
            offsets.push(pos as u16);
            pos += 2 + 32 + 2 + data.len();
        }
        let mut out = Vec::with_capacity(pos + 2);
        out.extend_from_slice(&(ixs.len() as u16).to_le_bytes());
        for off in offsets {
            out.extend_from_slice(&off.to_le_bytes());
        }
        for (program_id, data) in ixs {
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(program_id.as_ref());
            out.extend_from_slice(&(data.len() as u16).to_le_bytes());
            out.extend_from_slice(data);
        }
        out.extend_from_slice(&current_index.to_le_bytes());
        out
    }

    /// ed25519-program instruction data carrying dummy signatures
    /// from the given relayers (indices into `relayer_pubs()`) over
    /// `hash`. The parser never sees real signature validity — the
    /// runtime checks signatures on-chain — so dummy bytes suffice.
    fn ed25519_data(hash: &[u8; 32], relayers: &[usize]) -> Vec<u8> {
        let pubs = relayer_pubs();
        let sigs: Vec<RelayerSignature> = relayers
            .iter()
            .map(|&i| RelayerSignature {
                relayer: pubs[i],
                signature: [7u8; 64],
            })
            .collect();
        build_ed25519_instruction(hash, &sigs).data
    }

    fn verify_with_sysvar(data: Vec<u8>) -> Result<(), BridgeError> {
        verify_msg_with_sysvar(&test_message(), data)
    }

    fn verify_msg_with_sysvar(message: &BridgeMessage, data: Vec<u8>) -> Result<(), BridgeError> {
        let pubs = relayer_pubs();
        let key = solana_program::sysvar::instructions::ID;
        let owner = solana_program::sysvar::ID;
        let mut lamports = 0u64;
        let mut data = data;
        let account = AccountInfo {
            key: &key,
            is_signer: false,
            is_writable: false,
            lamports: Rc::new(RefCell::new(&mut lamports)),
            data: Rc::new(RefCell::new(&mut data)),
            owner: &owner,
            executable: false,
            rent_epoch: 0,
        };
        verify_precompile_attestation(message, &pubs, 3, &account)
    }

    #[test]
    fn debate_precompile_zero_quorum_rejected() {
        // Debate round 2026-11-18 (A-3/B-F3): with quorum 0 the
        // precompile path returned Ok with no ed25519 instruction in
        // the transaction at all.
        let pubs = relayer_pubs();
        let key = solana_program::sysvar::instructions::ID;
        let owner = solana_program::sysvar::ID;
        let mut lamports = 0u64;
        let mut data = sysvar_bytes(&[], 0);
        let account = AccountInfo {
            key: &key,
            is_signer: false,
            is_writable: false,
            lamports: Rc::new(RefCell::new(&mut lamports)),
            data: Rc::new(RefCell::new(&mut data)),
            owner: &owner,
            executable: false,
            rent_epoch: 0,
        };
        assert_eq!(
            verify_precompile_attestation(&test_message(), &pubs, 0, &account),
            Err(BridgeError::QuorumNotReached)
        );
    }

    #[test]
    fn malformed_sysvar_is_missing_attestation_never_a_panic() {
        // Regression test for the fuzz-found panic (bridge/fuzz,
        // 2026-11-14): the upstream load_current_index sliced
        // data[len-2..len] and panicked on short sysvar data. Every
        // malformed shape must be MissingAttestation instead.
        let cases: Vec<Vec<u8>> = vec![
            vec![],                              // empty (the fuzz crash)
            vec![0],                             // shorter than the count
            vec![1, 0],                          // count=1, nothing else
            vec![1, 0, 0, 0],                    // offset present, no body, no index tail
            vec![0xff, 0xff, 0, 0, 0, 0],        // absurd instruction count
            vec![0, 0, 5, 0],                    // count=0 but current index 5
            vec![2, 0, 0xff, 0xff, 0, 0, 1, 0],  // offset points out of bounds
        ];
        for data in cases {
            assert_eq!(
                verify_with_sysvar(data.clone()),
                Err(BridgeError::MissingAttestation),
                "sysvar data {data:?} must be rejected, not panic or pass"
            );
        }
    }

    #[test]
    fn valid_attestation_through_sysvar_passes() {
        // The positive path for the sysvar parser itself: one ed25519
        // instruction at index 0 carrying 3 distinct relayers over
        // the message hash, current index 1 (the bridge instruction).
        let hash = test_message().hash();
        let data = sysvar_bytes(
            &[(
                solana_program::ed25519_program::ID,
                ed25519_data(&hash, &[0, 1, 2]),
            )],
            1,
        );
        assert_eq!(verify_with_sysvar(data), Ok(()));
    }

    #[test]
    fn extra_instructions_around_ed25519_are_ignored() {
        // Unrelated instructions before and after the ed25519
        // instruction must not disturb verification.
        let hash = test_message().hash();
        let ed = (
            solana_program::ed25519_program::ID,
            ed25519_data(&hash, &[0, 1, 2]),
        );
        let before = (solana_program::system_program::ID, vec![2, 0, 0, 0]);
        let after = (
            Pubkey::new_from_array([3u8; 32]), // memo-program-shaped
            b"hello".to_vec(),
        );
        let data = sysvar_bytes(&[before, ed.clone(), after], 3);
        assert_eq!(verify_with_sysvar(data), Ok(()));

        // But an ed25519 instruction AT or AFTER the current index is
        // not a preceding instruction and must not be scanned: here
        // the bridge instruction is at index 1 and the ed25519
        // instruction sits at index 1, so nothing precedes it.
        let data = sysvar_bytes(&[(solana_program::system_program::ID, vec![2, 0, 0, 0]), ed], 1);
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::MissingAttestation)
        );
    }

    #[test]
    fn duplicate_relayer_across_instructions_counts_once() {
        let hash = test_message().hash();
        let ed = solana_program::ed25519_program::ID;
        // Relayer 1 appears in both instructions; the union is
        // {0, 1, 2} — quorum reached exactly once per distinct key.
        let data = sysvar_bytes(
            &[
                (ed, ed25519_data(&hash, &[0, 1])),
                (ed, ed25519_data(&hash, &[1, 2])),
            ],
            2,
        );
        assert_eq!(verify_with_sysvar(data), Ok(()));
        // The same pair repeated in a second instruction is still
        // only 2 distinct relayers — no double counting.
        let data = sysvar_bytes(
            &[
                (ed, ed25519_data(&hash, &[0, 1])),
                (ed, ed25519_data(&hash, &[0, 1])),
            ],
            2,
        );
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::QuorumNotReached)
        );
    }

    #[test]
    fn entries_over_a_different_hash_do_not_count() {
        let ed = solana_program::ed25519_program::ID;
        // Three registered relayers, but over the wrong hash: the
        // entries are not attestations of this message at all.
        let data = sysvar_bytes(&[(ed, ed25519_data(&[1u8; 32], &[0, 1, 2]))], 1);
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::MissingAttestation)
        );
        // Signatures over THIS hash exist, but from a key that is not
        // a registered relayer.
        let sigs = vec![RelayerSignature {
            relayer: Pubkey::new_from_array([99u8; 32]),
            signature: [7u8; 64],
        }];
        let hash = test_message().hash();
        let data = sysvar_bytes(&[(ed, build_ed25519_instruction(&hash, &sigs).data)], 1);
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::UnknownRelayer)
        );
    }

    #[test]
    fn malformed_offsets_are_skipped_never_counted() {
        let hash = test_message().hash();
        let ed = solana_program::ed25519_program::ID;
        let valid = ed25519_data(&hash, &[0, 1, 2]);
        // Corrupt one field of entry 0's offsets record (base = 2,
        // field j at base + 2*j, u16 little-endian). The entry must
        // be skipped, leaving 2 of 3 — QuorumNotReached, never a
        // panic and never a pass.
        let corrupt = |field: usize, value: u16| -> Vec<u8> {
            let mut d = valid.clone();
            d[2 + 2 * field..2 + 2 * field + 2].copy_from_slice(&value.to_le_bytes());
            d
        };
        // Signature offset far beyond the instruction data.
        let data = sysvar_bytes(&[(ed, corrupt(0, 0xff00))], 1);
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::QuorumNotReached)
        );
        // Message offset beyond the data.
        let data = sysvar_bytes(&[(ed, corrupt(4, 0xff00))], 1);
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::QuorumNotReached)
        );
        // Message size (64) larger than the 32-byte hash slice
        // actually present at the message offset.
        let data = sysvar_bytes(&[(ed, corrupt(5, 64))], 1);
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::QuorumNotReached)
        );
        // A header count the data cannot back at all (30 records
        // claimed, room for 3): the whole instruction is skipped.
        let mut d = valid.clone();
        d[0] = 30;
        let data = sysvar_bytes(&[(ed, d)], 1);
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::MissingAttestation)
        );
        // A count that overruns the real records but stays inside
        // the data (9 claimed, 3 real) scans garbage "records" out
        // of the payload bytes — and still passes on the 3 real
        // entries alone. That is safe, and pinned here deliberately:
        // a garbage record can only re-point at pubkey/hash bytes
        // already in the payload, duplicates are deduplicated by
        // the seen-set, and on-chain the ed25519 precompile itself
        // rejects a count that does not match its layout before
        // this program ever runs.
        let mut d = valid.clone();
        d[0] = 9;
        let data = sysvar_bytes(&[(ed, d)], 1);
        assert_eq!(verify_with_sysvar(data), Ok(()));
    }

    #[test]
    fn hash_binding_every_message_field_is_covered() {
        // The attestation binds to the message hash, and the hash
        // binds to every field of the canonical message. Mutating
        // ANY single field must change the hash, and a mutated
        // message must not verify against attestations made over
        // the original message — this is what stops a relayer (or
        // anyone) from reusing a valid attestation for a different
        // amount, recipient, nonce, or chain.
        let original = test_message();
        let hash = original.hash();
        let sysvar = sysvar_bytes(
            &[(
                solana_program::ed25519_program::ID,
                ed25519_data(&hash, &[0, 1, 2]),
            )],
            1,
        );
        assert_eq!(verify_msg_with_sysvar(&original, sysvar.clone()), Ok(()));
        let mutations: Vec<(&str, BridgeMessage)> = {
            let mut v = Vec::new();
            let mut m = original.clone();
            m.version += 1;
            v.push(("version", m));
            let mut m = original.clone();
            m.source_chain = CHAIN_MACHINE;
            v.push(("source_chain", m));
            let mut m = original.clone();
            m.dest_chain = CHAIN_SOLANA;
            v.push(("dest_chain", m));
            let mut m = original.clone();
            m.nonce += 1;
            v.push(("nonce", m));
            let mut m = original.clone();
            m.source_mint = Pubkey::new_from_array([1u8; 32]);
            v.push(("source_mint", m));
            let mut m = original.clone();
            m.sender = Pubkey::new_from_array([2u8; 32]);
            v.push(("sender", m));
            let mut m = original.clone();
            m.recipient = Pubkey::new_from_array([3u8; 32]);
            v.push(("recipient", m));
            let mut m = original.clone();
            m.amount += 1;
            v.push(("amount", m));
            let mut m = original.clone();
            m.source_tx[0] ^= 1;
            v.push(("source_tx", m));
            let mut m = original.clone();
            m.source_slot += 1;
            v.push(("source_slot", m));
            let mut m = original.clone();
            m.expiry_timestamp += 1;
            v.push(("expiry_timestamp", m));
            let mut m = original.clone();
            m.source_timestamp += 1;
            v.push(("source_timestamp", m));
            v
        };
        // Every field of BridgeMessage is mutated exactly once.
        assert_eq!(mutations.len(), 12);
        for (field, mutated) in mutations {
            assert_ne!(
                mutated.hash(),
                hash,
                "field {field} must feed the message hash"
            );
            assert_eq!(
                verify_msg_with_sysvar(&mutated, sysvar.clone()),
                Err(BridgeError::MissingAttestation),
                "attestation over the original message must not verify a message with a changed {field}"
            );
        }
    }

    #[test]
    fn cross_instruction_references_are_rejected() {
        // Offsets records may only reference their own instruction
        // (index u16::MAX or the instruction's own index). Pointing
        // an entry's message or signature at another instruction
        // must not let it count.
        let hash = test_message().hash();
        let ed = solana_program::ed25519_program::ID;
        let valid = ed25519_data(&hash, &[0, 1, 2]);
        let mut by_msg_ix = valid.clone();
        by_msg_ix[2 + 2 * 6..2 + 2 * 6 + 2].copy_from_slice(&1u16.to_le_bytes());
        let data = sysvar_bytes(&[(ed, by_msg_ix)], 1);
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::QuorumNotReached)
        );
        let mut by_sig_ix = valid;
        by_sig_ix[2 + 2 * 1..2 + 2 * 1 + 2].copy_from_slice(&2u16.to_le_bytes());
        let data = sysvar_bytes(&[(ed, by_sig_ix)], 1);
        assert_eq!(
            verify_with_sysvar(data),
            Err(BridgeError::QuorumNotReached)
        );
    }
}

#[cfg(test)]
mod nonce_gate_tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    fn account<'a>(
        key: &'a Pubkey,
        owner: &'a Pubkey,
        lamports: &'a mut u64,
        data: &'a mut Vec<u8>,
    ) -> AccountInfo<'a> {
        AccountInfo {
            key,
            is_signer: false,
            is_writable: true,
            lamports: Rc::new(RefCell::new(lamports)),
            data: Rc::new(RefCell::new(data)),
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn fresh_nonce_account_passes_and_returns_bump() {
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let (pda, bump) = nonce_pda(&program_id, DIR_SOLANA_TO_MACHINE, 7);
        let system = solana_program::system_program::ID;
        let mut lamports = 0u64;
        let mut data: Vec<u8> = Vec::new();
        let acct = account(&pda, &system, &mut lamports, &mut data);
        assert_eq!(
            check_nonce_unprocessed(&acct, &program_id, DIR_SOLANA_TO_MACHINE, 7),
            Ok(bump)
        );
    }

    #[test]
    fn program_owned_nonce_account_is_already_processed() {
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let (pda, _) = nonce_pda(&program_id, DIR_MACHINE_TO_SOLANA, 9);
        let mut lamports = 1_000_000u64;
        let mut data: Vec<u8> = vec![0u8; NonceRecord::LEN];
        let acct = account(&pda, &program_id, &mut lamports, &mut data);
        assert_eq!(
            check_nonce_unprocessed(&acct, &program_id, DIR_MACHINE_TO_SOLANA, 9),
            Err(BridgeError::NonceAlreadyProcessed)
        );
    }

    #[test]
    fn wrong_account_is_invalid_account_data() {
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let wrong = Pubkey::new_from_array([7u8; 32]);
        let system = solana_program::system_program::ID;
        let mut lamports = 0u64;
        let mut data: Vec<u8> = Vec::new();
        let acct = account(&wrong, &system, &mut lamports, &mut data);
        assert_eq!(
            check_nonce_unprocessed(&acct, &program_id, DIR_SOLANA_TO_MACHINE, 7),
            Err(BridgeError::InvalidAccountData)
        );
    }

    #[test]
    fn directions_have_separate_nonce_registries() {
        // The PDA for (Solana->Machine, 7) must not satisfy the check
        // for (Machine->Solana, 7): the registries are independent.
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let (pda_s2m, _) = nonce_pda(&program_id, DIR_SOLANA_TO_MACHINE, 7);
        let system = solana_program::system_program::ID;
        let mut lamports = 0u64;
        let mut data: Vec<u8> = Vec::new();
        let acct = account(&pda_s2m, &system, &mut lamports, &mut data);
        assert_eq!(
            check_nonce_unprocessed(&acct, &program_id, DIR_MACHINE_TO_SOLANA, 7),
            Err(BridgeError::InvalidAccountData)
        );
    }
}

#[cfg(test)]
mod init_gate_tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    fn account<'a>(
        key: &'a Pubkey,
        owner: &'a Pubkey,
        lamports: &'a mut u64,
        data: &'a mut Vec<u8>,
    ) -> AccountInfo<'a> {
        AccountInfo {
            key,
            is_signer: false,
            is_writable: true,
            lamports: Rc::new(RefCell::new(lamports)),
            data: Rc::new(RefCell::new(data)),
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn member_set_validation_accepts_distinct_non_default_keys() {
        let set: [Pubkey; 5] = std::array::from_fn(|_| Pubkey::new_unique());
        assert_eq!(validate_member_set(&set), Ok(()));
        let admins: [Pubkey; 3] = std::array::from_fn(|_| Pubkey::new_unique());
        assert_eq!(validate_member_set(&admins), Ok(()));
    }

    #[test]
    fn member_set_validation_rejects_default_and_duplicate_keys() {
        // Self-audit F-3: Initialize must apply the same validation
        // SetRelayers always had.
        let mut with_default: [Pubkey; 5] = std::array::from_fn(|_| Pubkey::new_unique());
        with_default[3] = Pubkey::default();
        assert_eq!(
            validate_member_set(&with_default),
            Err(BridgeError::InvalidState)
        );
        let mut with_dup: [Pubkey; 5] = std::array::from_fn(|_| Pubkey::new_unique());
        with_dup[2] = with_dup[0];
        with_dup[4] = with_dup[0];
        assert_eq!(
            validate_member_set(&with_dup),
            Err(BridgeError::DuplicateRelayerSignature)
        );
    }

    /// Hand-built ProgramData metadata in the loader's bincode
    /// layout: variant tag 3, slot, option tag, authority.
    fn programdata_bytes(authority: Option<Pubkey>) -> Vec<u8> {
        let mut data = vec![0u8; 45];
        data[0..4].copy_from_slice(&3u32.to_le_bytes());
        data[4..12].copy_from_slice(&123u64.to_le_bytes());
        if let Some(a) = authority {
            data[12] = 1;
            data[13..45].copy_from_slice(a.as_ref());
        }
        data
    }

    #[test]
    #[allow(deprecated)]
    fn upgrade_authority_accepts_the_recorded_authority() {
        let loader = solana_program::bpf_loader_upgradeable::id();
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let pd = solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id);
        let authority = Pubkey::new_unique();
        let mut lamports = 1_000_000u64;
        let mut data = programdata_bytes(Some(authority));
        let acct = account(&pd, &loader, &mut lamports, &mut data);
        assert_eq!(
            verify_upgrade_authority(&program_id, &acct, &authority),
            Ok(())
        );
    }

    #[test]
    #[allow(deprecated)]
    fn upgrade_authority_rejects_strangers_immutable_and_wrong_accounts() {
        let loader = solana_program::bpf_loader_upgradeable::id();
        let program_id = Pubkey::new_from_array([42u8; 32]);
        let pd = solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id);
        let authority = Pubkey::new_unique();
        let stranger = Pubkey::new_unique();

        // A payer who is not the recorded upgrade authority.
        let mut lamports = 1_000_000u64;
        let mut data = programdata_bytes(Some(authority));
        let acct = account(&pd, &loader, &mut lamports, &mut data);
        assert_eq!(
            verify_upgrade_authority(&program_id, &acct, &stranger),
            Err(BridgeError::Unauthorized)
        );

        // No upgrade authority recorded (immutable program): nobody
        // may initialize against it.
        let mut lamports = 1_000_000u64;
        let mut data = programdata_bytes(None);
        let acct = account(&pd, &loader, &mut lamports, &mut data);
        assert_eq!(
            verify_upgrade_authority(&program_id, &acct, &authority),
            Err(BridgeError::InvalidAccountData)
        );

        // The right bytes at the wrong address prove nothing.
        let wrong = Pubkey::new_unique();
        let mut lamports = 1_000_000u64;
        let mut data = programdata_bytes(Some(authority));
        let acct = account(&wrong, &loader, &mut lamports, &mut data);
        assert_eq!(
            verify_upgrade_authority(&program_id, &acct, &authority),
            Err(BridgeError::InvalidAccountData)
        );

        // The right address not owned by the loader proves nothing.
        let mut lamports = 1_000_000u64;
        let mut data = programdata_bytes(Some(authority));
        let acct = account(&pd, &program_id, &mut lamports, &mut data);
        assert_eq!(
            verify_upgrade_authority(&program_id, &acct, &authority),
            Err(BridgeError::InvalidAccountData)
        );
    }
}
