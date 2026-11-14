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
//! - Caps are absolute amounts set at initialization. BRIDGE.md section 4.4
//!   gives launch values as percentages of locked supply (1% per-tx, 5%
//!   daily); those translate to absolute numbers at deployment time.
//! - Signatures are verified in-program with ed25519-dalek over the
//!   message hash, rather than via the ed25519 precompile + instructions
//!   sysvar. The verification is equivalent (same curve, same hash) and
//!   is directly unit-testable; the precompile path is an optimization
//!   for a later review, not a security difference.

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use solana_program::{
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

/// One day in seconds, for the daily-cap window.
pub const DAY_SECONDS: i64 = 86_400;

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

/// Verify a quorum attestation over `message`.
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
// Caps (BRIDGE.md section 4.4)
// ---------------------------------------------------------------------------

/// Daily usage tracker. The window is the UTC day number; the first
/// operation in a new window resets the counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize, Default)]
pub struct DailyUsage {
    pub window_start: i64,
    pub used: u64,
}

impl DailyUsage {
    /// Check `amount` against the per-transaction and daily caps and return
    /// the updated usage. Pure function — fully unit-testable.
    pub fn check_and_record(
        &self,
        amount: u64,
        per_tx_cap: u64,
        daily_cap: u64,
        now: i64,
    ) -> Result<DailyUsage, BridgeError> {
        if amount == 0 {
            return Err(BridgeError::InvalidAmount);
        }
        if amount > per_tx_cap {
            return Err(BridgeError::AmountExceedsPerTxCap);
        }
        let window = now / DAY_SECONDS;
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
    pub bump: u8,
}

impl MachineConfig {
    pub const LEN: usize = 1 + 1 + 1 + (32 * 5 + 1) + (32 * 3 + 1) + 32 + 8 + 8 + (8 + 8) + 1;

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
    MintWrapped {
        message: BridgeMessage,
        signatures: Vec<RelayerSignature>,
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
    Unlock {
        message: BridgeMessage,
        signatures: Vec<RelayerSignature>,
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
        // Day 0: use 800 of 1000 daily cap.
        let usage = usage.check_and_record(800, 10_000, 1_000, 100).unwrap();
        assert_eq!(usage.used, 800);
        // Same day: another 300 exceeds the daily cap.
        assert_eq!(
            usage.check_and_record(300, 10_000, 1_000, 200),
            Err(BridgeError::AmountExceedsDailyCap)
        );
        // Next day: window resets, 300 is fine.
        let usage = usage.check_and_record(300, 10_000, 1_000, DAY_SECONDS + 100).unwrap();
        assert_eq!(usage.used, 300);
        // Per-transaction cap.
        assert_eq!(
            usage.check_and_record(10_001, 10_000, 1_000_000, DAY_SECONDS + 200),
            Err(BridgeError::AmountExceedsPerTxCap)
        );
        // Zero amount.
        assert_eq!(
            usage.check_and_record(0, 10_000, 1_000_000, DAY_SECONDS + 200),
            Err(BridgeError::InvalidAmount)
        );
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
