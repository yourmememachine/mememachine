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
//!   The daily window is slot-based (216,000 slots), not wall-clock,
//!   so it is as deterministic as the message expiry.
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

/// Current message format version.
pub const MESSAGE_VERSION: u8 = 1;

/// Domain separator mixed into every message hash.
pub const DOMAIN_SEPARATOR: &[u8] = b"MACHINE-BRIDGE-V1";

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

/// Basis-point denominator for the dynamic percentage caps.
pub const BPS_DENOMINATOR: u64 = 10_000;
/// Dynamic per-transaction cap: 1% of the reference supply (BRIDGE.md
/// section 4.4).
pub const DYNAMIC_PER_TX_BPS: u64 = 100;
/// Dynamic daily cap: 5% of the reference supply (BRIDGE.md 4.4).
pub const DYNAMIC_DAILY_BPS: u64 = 500;

/// Cross-chain direction, used in nonce PDA seeds so the two directions
/// can never share a nonce registry.
pub const DIR_SOLANA_TO_MACHINE: u8 = 0;
pub const DIR_MACHINE_TO_SOLANA: u8 = 1;

/// PDA seeds.
pub const SEED_CONFIG: &[u8] = b"config";
pub const SEED_NONCE: &[u8] = b"nonce";
pub const SEED_AUTHORITY: &[u8] = b"bridge-authority";

/// Account discriminators (first byte of every account's data).
pub const DISCRIMINATOR_CONFIG: u8 = 1;
pub const DISCRIMINATOR_NONCE_RECORD: u8 = 2;
pub const DISCRIMINATOR_EVENT_RECORD: u8 = 3;

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
    pub expiry_slot: u64,
}

/// Fixed-size canonical encoding: 1 + 1 + 1 + 8 + 32 + 32 + 32 + 8 + 64 + 8
/// + 8 = 195 bytes. Fixed layout means no parsing ambiguity.
pub const MESSAGE_CANONICAL_LEN: usize = 195;

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
        out[o..o + 8].copy_from_slice(&self.expiry_slot.to_le_bytes());
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
/// chain binding, and expiry against the current slot.
pub fn validate_message(
    message: &BridgeMessage,
    expected_source: u8,
    expected_dest: u8,
    current_slot: u64,
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
    if current_slot > message.expiry_slot {
        return Err(BridgeError::ExpiredAttestation);
    }
    Ok(())
}

/// Verify a quorum attestation over `message` (off-chain path, used by
/// the relayer to re-verify peer attestations before signing).
///
/// Checks, in order: version, chain binding, expiry against the current
/// slot, then each signature (relayer must be registered, signature must
/// verify over the message hash, each relayer counted at most once), and
/// finally that distinct valid signatures reach `quorum`.
pub fn verify_attestation(
    message: &BridgeMessage,
    signatures: &[RelayerSignature],
    relayers: &[Pubkey; MAX_RELAYERS],
    quorum: u8,
    expected_source: u8,
    expected_dest: u8,
    current_slot: u64,
) -> Result<(), BridgeError> {
    validate_message(message, expected_source, expected_dest, current_slot)?;

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
    if (amount as u128) * (BPS_DENOMINATOR as u128)
        > (reference_supply as u128) * (DYNAMIC_PER_TX_BPS as u128)
    {
        return Err(BridgeError::AmountExceedsDynamicPerTxCap);
    }
    if (window_total_after as u128) * (BPS_DENOMINATOR as u128)
        > (reference_supply as u128) * (DYNAMIC_DAILY_BPS as u128)
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
    if count_signers_in_set(signer_pubkeys, admins) < threshold {
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
    pub const LEN: usize = 1 + 1 + 1 + (32 * 5 + 1) + (32 * 3 + 1) + 32 + 8 + 8 + (8 + 8) + 8 + 1;

    pub fn new(
        relayers: [Pubkey; MAX_RELAYERS],
        admins: [Pubkey; MAX_ADMINS],
        wrapped_mint: Pubkey,
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
    pub bump: u8,
}

impl LockConfig {
    pub const LEN: usize =
        1 + 1 + (32 * 5 + 1) + (32 * 3 + 1) + 32 + 32 + 1 + 8 + 8 + 8 + 8 + (8 + 8) + 1;

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
            bump,
        }
    }

    pub fn check_active(&self) -> Result<(), BridgeError> {
        if self.paused {
            return Err(BridgeError::BridgePaused);
        }
        Ok(())
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

// ---------------------------------------------------------------------------
// PDA helpers
// ---------------------------------------------------------------------------

pub fn config_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_CONFIG], program_id)
}

pub fn nonce_pda(program_id: &Pubkey, direction: u8, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_NONCE, &[direction], &nonce.to_le_bytes()], program_id)
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
            expiry_slot: 10_000,
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
    fn canonical_encoding_is_stable_and_195_bytes() {
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
        assert!(verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 5000).is_ok());
    }

    #[test]
    fn valid_5_of_5_attestation_passes() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let sigs = sign_all(&msg, &keys, 5);
        assert!(verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 5000).is_ok());
    }

    #[test]
    fn two_of_5_fails_quorum() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let sigs = sign_all(&msg, &keys, 2);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 5000),
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
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 5000),
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
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 5000),
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
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 5000),
            Err(BridgeError::InvalidSignature)
        );
    }

    #[test]
    fn wrong_direction_rejected() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let sigs = sign_all(&msg, &keys, 3);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_MACHINE, CHAIN_SOLANA, 5000),
            Err(BridgeError::WrongSourceChain)
        );
    }

    #[test]
    fn expired_attestation_rejected() {
        let (keys, pubs) = make_relayers();
        let msg = test_message();
        let sigs = sign_all(&msg, &keys, 3);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 10_001),
            Err(BridgeError::ExpiredAttestation)
        );
    }

    #[test]
    fn wrong_version_rejected() {
        let (keys, pubs) = make_relayers();
        let mut msg = test_message();
        msg.version = 2;
        let sigs = sign_all(&msg, &keys, 3);
        assert_eq!(
            verify_attestation(&msg, &sigs, &pubs, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 5000),
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
        let mut cfg = MachineConfig::new(relayers, admins, Pubkey::new_unique(), 1000, 10_000, 255);
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
        let cfg = MachineConfig::new(relayers, admins, Pubkey::new_unique(), 1000, 10_000, 255);
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

    fn verify_with_sysvar(data: Vec<u8>) -> Result<(), BridgeError> {
        let pubs: [Pubkey; MAX_RELAYERS] = [
            Pubkey::new_from_array([11u8; 32]),
            Pubkey::new_from_array([22u8; 32]),
            Pubkey::new_from_array([33u8; 32]),
            Pubkey::new_from_array([44u8; 32]),
            Pubkey::new_from_array([55u8; 32]),
        ];
        let message = BridgeMessage {
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
            expiry_slot: 1_000_000,
        };
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
        verify_precompile_attestation(&message, &pubs, 3, &account)
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
