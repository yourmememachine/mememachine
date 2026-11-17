//! Off-chain relayer for the Machine bridge (BRIDGE.md sections 2.5, 3).
//!
//! What a relayer does, and only what it does:
//! 1. Watch the source chain for a recorded deposit (Solana lock program)
//!    or burn (Machine bridge program) — an `EventRecord` PDA.
//! 2. Refuse to act until that event is in a *finalized* slot, plus the
//!    safety margin from BRIDGE.md 2.5. Never attest to a merely
//!    confirmed or processed transaction.
//! 3. Build the canonical `BridgeMessage` for the event and sign its
//!    hash with this relayer's own keypair.
//! 4. Exchange that signature with the other relayers (phase 1: signed
//!    attestation files in a shared directory — see `attestation`).
//!    A quorum is 3 distinct relayers over the *same* message hash.
//!    If relayers disagree on the message for a nonce, no quorum can
//!    form and nothing executes (BRIDGE.md 3.2).
//! 5. Submit the quorum attestation to the destination program:
//!    `MintWrapped` on Machine, or `Unlock` on Solana.
//!
//! Relayers hold no funds and cannot move assets alone: one signature
//! is useless, the mint authority is a program PDA, and caps are
//! enforced on-chain, not here.
//!
//! Determinism rule (load-bearing): every field of `BridgeMessage` is
//! part of the signed hash, so all relayers must build the *identical*
//! message for the same event. That is why `expiry_slot` is derived
//! deterministically from `source_slot` (`source_slot + expiry_window_slots`,
//! a shared config value), never from the local clock or the current
//! destination slot. Relayers running different expiry windows will
//! (safely) never reach quorum — the collector reports the mismatch.

pub mod attestation;
pub mod audit_log;
pub mod config;
pub mod message;
pub mod rpc;
pub mod signing;
pub mod submit;
pub mod watcher;

use std::fmt;

#[derive(Debug)]
pub enum RelayerError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Borsh(std::io::Error),
    Rpc(String),
    InvalidConfig(String),
    InvalidKeypair(String),
    InvalidPubkey(String),
    InvalidSignature(String),
    /// The source event exists but is not finalized yet (plus margin).
    NotFinalized { source_slot: u64, finalized_slot: u64, safety_margin_slots: u64 },
    /// Relayers produced different message hashes for the same nonce.
    Disagreement { nonce: u64, hashes: Vec<String> },
    QuorumNotReached { have: usize, need: u8 },
    DuplicateRelayer(PubkeyString),
    UnknownRelayer(PubkeyString),
    EventNotFound(String),
    SenderNotFound(String),
    /// The source transaction contains a transfer/burn of the right
    /// kind but none that matches the event record (wrong vault
    /// destination, wrong mint, or wrong amount) — fail closed
    /// rather than sign a message whose sender cannot be proven.
    SourceTransferNotVerified(String),
    /// The tamper-evident audit log (audit_log.rs) failed to parse,
    /// verify, or append — the relayer fails closed rather than act
    /// without its evidence trail.
    AuditLog(String),
}

pub type PubkeyString = String;

impl fmt::Display for RelayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RelayerError::Io(e) => write!(f, "io error: {e}"),
            RelayerError::Json(e) => write!(f, "json error: {e}"),
            RelayerError::Borsh(e) => write!(f, "borsh error: {e}"),
            RelayerError::Rpc(m) => write!(f, "rpc error: {m}"),
            RelayerError::InvalidConfig(m) => write!(f, "invalid config: {m}"),
            RelayerError::InvalidKeypair(m) => write!(f, "invalid keypair: {m}"),
            RelayerError::InvalidPubkey(m) => write!(f, "invalid pubkey: {m}"),
            RelayerError::InvalidSignature(m) => write!(f, "invalid signature: {m}"),
            RelayerError::NotFinalized { source_slot, finalized_slot, safety_margin_slots } => write!(
                f,
                "not finalized: source slot {source_slot}, finalized slot {finalized_slot}, safety margin {safety_margin_slots}"
            ),
            RelayerError::Disagreement { nonce, hashes } => {
                write!(f, "relayer disagreement for nonce {nonce}: message hashes {hashes:?}")
            }
            RelayerError::QuorumNotReached { have, need } => {
                write!(f, "quorum not reached: have {have} distinct signatures, need {need}")
            }
            RelayerError::DuplicateRelayer(p) => write!(f, "duplicate signature from relayer {p}"),
            RelayerError::UnknownRelayer(p) => write!(f, "signature from unknown relayer {p}"),
            RelayerError::EventNotFound(m) => write!(f, "source event not found: {m}"),
            RelayerError::SenderNotFound(m) => write!(f, "sender not found in source transaction: {m}"),
            RelayerError::SourceTransferNotVerified(m) => {
                write!(f, "source transfer not verified: {m}")
            }
            RelayerError::AuditLog(m) => write!(f, "audit log error: {m}"),
        }
    }
}

impl std::error::Error for RelayerError {}

impl From<std::io::Error> for RelayerError {
    fn from(e: std::io::Error) -> Self {
        RelayerError::Io(e)
    }
}

impl From<serde_json::Error> for RelayerError {
    fn from(e: serde_json::Error) -> Self {
        RelayerError::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, RelayerError>;
