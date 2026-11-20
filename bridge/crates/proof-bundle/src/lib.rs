//! Crossing proof bundles (bridge/PROOF-BUNDLE.md).
//!
//! A bundle is one self-contained JSON file carrying everything an
//! independent checker needs to confirm that a bridge crossing
//! satisfied the bridge's own rules:
//!
//! - the **crossing receipt** as exported from the receipt the
//!   destination program wrote in the same transaction that moved
//!   the value (BRIDGE.md 4.5) — carried as stated fields, not as
//!   an authenticated artifact (see the limits below);
//! - the canonical **`BridgeMessage`** the relayers attested to, in
//!   the same wire form the relayer exchange uses
//!   (`relayer::attestation::MessageFile`);
//! - the **attestation set**: which relayers signed, and their
//!   signatures over the message hash;
//! - the **relayer set and quorum** the attestations are checked
//!   against — stated in the bundle, and pinned by the verifier to
//!   the protocol constants (exactly `MAX_RELAYERS` members,
//!   quorum exactly `QUORUM`), so a bundle cannot quietly weaken
//!   the rules it claims to prove compliance with. The set's
//!   *shape* is pinned, not its *identity*: the set is the bundle's
//!   own statement, and `verify_bundle_against_set` exists for a
//!   verifier who wants to pin it to the registered set.
//!
//! Verification is fully offline and re-runs the bridge's own
//! checks: the message hash is recomputed from the canonical
//! 203-byte encoding under the `MACHINE-BRIDGE-V2` domain separator,
//! the receipt is compared field-by-field against the message, and
//! the signatures are verified with bridge-common's
//! `verify_attestation` — the same code the relayer collector runs
//! before submitting, with the expiry check evaluated at the
//! receipt's timestamp (the destination chain's Clock time at which
//! the crossing completed, which is when the on-chain program
//! evaluated it too).
//!
//! What a valid bundle proves — and only this: a quorum of the
//! stated relayer set signed this exact message, and the bundle
//! contains a receipt whose fields match that message. The receipt
//! is unauthenticated data in the bundle (debate round 2026-11-18,
//! A-4): nothing in the file binds it to the destination chain, so
//! the bundle does *not* by itself prove the destination program
//! ever wrote that receipt — confirming a crossing on-chain (the
//! receipt PDA in the destination program's account space) is a
//! separate lookup, exactly as confirming the stated relayer set
//! against the on-chain config is. It does not prove the bridge is
//! safe, audited, or deployed; see bridge/PROOF-BUNDLE.md for the
//! honest limits.

use bridge_common::{
    validate_member_set, verify_attestation, BridgeError, BridgeMessage, CrossingReceipt,
    RelayerSignature, MAX_RELAYERS, QUORUM,
};
use relayer::attestation::{Collector, MessageFile};
use relayer::message::Direction;
use serde::{Deserialize, Serialize};
use solana_program::pubkey::Pubkey;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

/// Bundle format version. A verifier rejects any other version
/// rather than guessing at changed semantics.
pub const BUNDLE_VERSION: u8 = 1;

// ---------------------------------------------------------------------------
// Wire forms (base58 / hex, mirroring the relayer exchange format so
// the files are inspectable by eye in an incident)
// ---------------------------------------------------------------------------

/// One relayer's signature over the message hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureFile {
    pub relayer: String,
    /// base58, 64 bytes.
    pub signature: String,
}

/// The crossing receipt (bridge-common `CrossingReceipt`) in wire
/// form: pubkeys and the source transaction as base58, the message
/// hash as hex — the same encodings the relayer exchange files use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptFile {
    /// Direction byte (`DIR_SOLANA_TO_MACHINE` / `DIR_MACHINE_TO_SOLANA`).
    pub direction: u8,
    pub nonce: u64,
    /// hex, 32 bytes.
    pub message_hash: String,
    /// base58, 64 bytes.
    pub source_tx: String,
    pub amount: u64,
    pub recipient: String,
    /// Destination-chain slot at completion.
    pub slot: u64,
    /// Destination-chain unix timestamp at completion.
    pub timestamp: i64,
}

impl From<&CrossingReceipt> for ReceiptFile {
    fn from(r: &CrossingReceipt) -> Self {
        Self {
            direction: r.direction,
            nonce: r.nonce,
            message_hash: hex_encode(&r.message_hash),
            source_tx: bs58::encode(r.source_tx).into_string(),
            amount: r.amount,
            recipient: r.recipient.to_string(),
            slot: r.slot,
            timestamp: r.timestamp,
        }
    }
}

impl ReceiptFile {
    pub fn to_receipt(&self) -> Result<CrossingReceipt, BundleError> {
        Ok(CrossingReceipt {
            discriminator: bridge_common::DISCRIMINATOR_RECEIPT,
            direction: self.direction,
            nonce: self.nonce,
            message_hash: hex_decode_32("receipt.message_hash", &self.message_hash)?,
            source_tx: base58_decode_64("receipt.source_tx", &self.source_tx)?,
            amount: self.amount,
            recipient: parse_pubkey("receipt.recipient", &self.recipient)?,
            slot: self.slot,
            timestamp: self.timestamp,
        })
    }
}

/// One self-contained crossing proof bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossingBundle {
    pub bundle_version: u8,
    /// Direction label: `solana-to-machine` or `machine-to-solana`.
    pub direction: String,
    pub message: MessageFile,
    /// hex, 32 bytes: sha256("MACHINE-BRIDGE-V2" || canonical message).
    pub message_hash: String,
    pub attestations: Vec<SignatureFile>,
    /// The registered relayer set the attestations are checked
    /// against — exactly `MAX_RELAYERS` pubkeys, in config order.
    pub relayer_set: Vec<String>,
    /// The quorum the bundle claims; the verifier requires it to
    /// equal the protocol constant `QUORUM`.
    pub quorum: u8,
    pub receipt: ReceiptFile,
}

// ---------------------------------------------------------------------------
// Errors — every rejection names the specific check that failed
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleError {
    UnsupportedBundleVersion(u8),
    UnknownDirection(String),
    /// The direction label disagrees with the message's chain pair.
    DirectionMismatch,
    InvalidMessage(String),
    InvalidPubkey { field: String, value: String },
    InvalidEncoding { field: String, reason: String },
    RelayerSetWrongSize(usize),
    InvalidRelayerSet(BridgeError),
    QuorumMismatch { claimed: u8, required: u8 },
    /// A receipt field disagrees with the attested message.
    ReceiptMismatch { field: &'static str },
    /// The message amount is zero — a crossing both programs reject
    /// on-chain (debate round 2026-11-18, A-8).
    ZeroAmount,
    /// The bundle's stated relayer set differs from the expected set
    /// the caller pinned (debate round 2026-11-18, A-5).
    RelayerSetMismatch,
    /// The stated message hash is not the hash of the stated message.
    MessageHashMismatch,
    /// bridge-common's attestation verification rejected the set.
    Attestation(BridgeError),
    /// Export-side failure (attestation collection, IO, JSON).
    Export(String),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BundleError::UnsupportedBundleVersion(v) => {
                write!(f, "unsupported bundle version {v} (this verifier reads version {BUNDLE_VERSION})")
            }
            BundleError::UnknownDirection(d) => {
                write!(f, "unknown direction '{d}' (use solana-to-machine or machine-to-solana)")
            }
            BundleError::DirectionMismatch => {
                write!(f, "direction label disagrees with the message's source/dest chains")
            }
            BundleError::InvalidMessage(e) => write!(f, "message does not parse: {e}"),
            BundleError::InvalidPubkey { field, value } => {
                write!(f, "{field} is not a valid pubkey: '{value}'")
            }
            BundleError::InvalidEncoding { field, reason } => {
                write!(f, "{field} has an invalid encoding: {reason}")
            }
            BundleError::RelayerSetWrongSize(n) => {
                write!(f, "relayer set has {n} members, expected exactly {MAX_RELAYERS}")
            }
            BundleError::InvalidRelayerSet(e) => {
                write!(f, "relayer set is not a valid member set: {e}")
            }
            BundleError::QuorumMismatch { claimed, required } => {
                write!(f, "bundle claims quorum {claimed}, but the bridge's quorum is {required}; a weaker quorum is not a proof under the bridge's rules")
            }
            BundleError::ReceiptMismatch { field } => {
                write!(f, "receipt {field} does not match the attested message")
            }
            BundleError::ZeroAmount => {
                write!(f, "message amount is zero; the bridge programs reject zero-amount crossings")
            }
            BundleError::RelayerSetMismatch => {
                write!(f, "bundle relayer set does not match the expected relayer set")
            }
            BundleError::MessageHashMismatch => {
                write!(f, "stated message hash does not equal sha256(domain separator || canonical message)")
            }
            BundleError::Attestation(e) => write!(f, "attestation verification failed: {e}"),
            BundleError::Export(e) => write!(f, "export failed: {e}"),
        }
    }
}

impl std::error::Error for BundleError {}

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode_32(field: &str, s: &str) -> Result<[u8; 32], BundleError> {
    let invalid = |reason: &str| BundleError::InvalidEncoding {
        field: field.to_string(),
        reason: reason.to_string(),
    };
    if s.len() != 64 {
        return Err(invalid("expected 64 hex characters (32 bytes)"));
    }
    // Debate round 2026-11-18 (A-6/B-F5): decode from the byte view.
    // Slicing the &str at byte offsets panicked on a 64-byte string
    // whose multi-byte characters crossed an even boundary — a
    // verifier run on other people's files must reject, not crash.
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    for i in 0..32 {
        let pair = std::str::from_utf8(&bytes[2 * i..2 * i + 2])
            .map_err(|_| invalid("not hex"))?;
        out[i] = u8::from_str_radix(pair, 16).map_err(|_| invalid("not hex"))?;
    }
    Ok(out)
}

fn base58_decode_64(field: &str, s: &str) -> Result<[u8; 64], BundleError> {
    let v = bs58::decode(s).into_vec().map_err(|e| BundleError::InvalidEncoding {
        field: field.to_string(),
        reason: e.to_string(),
    })?;
    v.try_into().map_err(|_| BundleError::InvalidEncoding {
        field: field.to_string(),
        reason: "not 64 bytes".to_string(),
    })
}

fn parse_pubkey(field: &str, s: &str) -> Result<Pubkey, BundleError> {
    Pubkey::from_str(s).map_err(|_| BundleError::InvalidPubkey {
        field: field.to_string(),
        value: s.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// The facts a successful verification establishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub direction: String,
    pub nonce: u64,
    pub amount: u64,
    pub recipient: Pubkey,
    pub message_hash: [u8; 32],
    /// Distinct registered relayers whose signatures verified.
    pub distinct_signers: usize,
    pub quorum: u8,
    pub relayer_set_size: usize,
    /// The relayer set the signatures were verified against, in
    /// config order (debate round 2026-11-18, A-5: the report
    /// previously recorded only the set's size, so a consumer of the
    /// verdict could not tell *whose* quorum it was).
    pub relayer_set: Vec<Pubkey>,
    /// Destination slot at which the crossing completed (receipt).
    pub receipt_slot: u64,
    /// Destination timestamp at which the crossing completed (receipt).
    pub receipt_timestamp: i64,
}

/// Verify a bundle fully offline. Checks, in order:
///
/// 1. bundle version is one this verifier understands;
/// 2. the direction label parses and matches the message's chains;
/// 3. the relayer set is exactly `MAX_RELAYERS` valid, distinct,
///    non-default members, and the claimed quorum equals the
///    protocol constant — a bundle may not weaken the rules;
/// 4. the message amount is non-zero (a protocol invariant both
///    programs enforce on-chain);
/// 5. every receipt field that the message also carries agrees
///    with the message (direction, nonce, amount, recipient,
///    source transaction);
/// 6. the stated message hash (bundle and receipt) equals the
///    recomputed sha256 over the canonical encoding;
/// 7. bridge-common's `verify_attestation` accepts the signature
///    set against the stated relayer set — signatures valid over
///    the recomputed hash, signers registered and distinct, quorum
///    reached, message unexpired at the receipt's timestamp.
///
/// The relayer set is the bundle's own statement: this function
/// checks its shape, not its identity. To pin the set to a known
/// registered set, use `verify_bundle_against_set`.
pub fn verify_bundle(bundle: &CrossingBundle) -> Result<VerificationReport, BundleError> {
    verify_bundle_inner(bundle, None)
}

/// Verify a bundle and additionally require its stated relayer set
/// to equal `expected_set` (debate round 2026-11-18, A-5). A bundle
/// is self-asserting about *which* set signed; pinning is how a
/// careful verifier anchors the verdict to the set actually
/// registered in the on-chain config.
pub fn verify_bundle_against_set(
    bundle: &CrossingBundle,
    expected_set: &[Pubkey; MAX_RELAYERS],
) -> Result<VerificationReport, BundleError> {
    verify_bundle_inner(bundle, Some(expected_set))
}

fn verify_bundle_inner(
    bundle: &CrossingBundle,
    expected_set: Option<&[Pubkey; MAX_RELAYERS]>,
) -> Result<VerificationReport, BundleError> {
    if bundle.bundle_version != BUNDLE_VERSION {
        return Err(BundleError::UnsupportedBundleVersion(bundle.bundle_version));
    }
    let direction = Direction::parse(&bundle.direction)
        .map_err(|_| BundleError::UnknownDirection(bundle.direction.clone()))?;

    let message = bundle
        .message
        .to_message()
        .map_err(|e| BundleError::InvalidMessage(e.to_string()))?;
    if message.source_chain != direction.source_chain()
        || message.dest_chain != direction.dest_chain()
    {
        return Err(BundleError::DirectionMismatch);
    }

    if bundle.relayer_set.len() != MAX_RELAYERS {
        return Err(BundleError::RelayerSetWrongSize(bundle.relayer_set.len()));
    }
    let mut relayer_set: Vec<Pubkey> = Vec::with_capacity(MAX_RELAYERS);
    for (i, s) in bundle.relayer_set.iter().enumerate() {
        relayer_set.push(parse_pubkey(&format!("relayer_set[{i}]"), s)?);
    }
    validate_member_set(&relayer_set).map_err(BundleError::InvalidRelayerSet)?;
    if bundle.quorum != QUORUM {
        return Err(BundleError::QuorumMismatch {
            claimed: bundle.quorum,
            required: QUORUM,
        });
    }
    if let Some(expected) = expected_set {
        if relayer_set.as_slice() != expected.as_slice() {
            return Err(BundleError::RelayerSetMismatch);
        }
    }
    // Debate round 2026-11-18 (A-8): both programs reject a
    // zero-amount crossing on-chain, and the shipped relayer refuses
    // to build one; a bundle for one is not a proof under the
    // bridge's rules.
    if message.amount == 0 {
        return Err(BundleError::ZeroAmount);
    }

    let receipt = bundle.receipt.to_receipt()?;
    if receipt.direction != direction.nonce_dir() {
        return Err(BundleError::ReceiptMismatch { field: "direction" });
    }
    if receipt.nonce != message.nonce {
        return Err(BundleError::ReceiptMismatch { field: "nonce" });
    }
    if receipt.amount != message.amount {
        return Err(BundleError::ReceiptMismatch { field: "amount" });
    }
    if receipt.recipient != message.recipient {
        return Err(BundleError::ReceiptMismatch { field: "recipient" });
    }
    if receipt.source_tx != message.source_tx {
        return Err(BundleError::ReceiptMismatch { field: "source_tx" });
    }

    let hash = message.hash();
    if hex_decode_32("message_hash", &bundle.message_hash)? != hash
        || receipt.message_hash != hash
    {
        return Err(BundleError::MessageHashMismatch);
    }

    let mut signatures: Vec<RelayerSignature> = Vec::with_capacity(bundle.attestations.len());
    for (i, a) in bundle.attestations.iter().enumerate() {
        signatures.push(RelayerSignature {
            relayer: parse_pubkey(&format!("attestations[{i}].relayer"), &a.relayer)?,
            signature: base58_decode_64(&format!("attestations[{i}].signature"), &a.signature)?,
        });
    }
    let relayer_array: [Pubkey; MAX_RELAYERS] = relayer_set
        .clone()
        .try_into()
        .expect("length checked above");
    verify_attestation(
        &message,
        &signatures,
        &relayer_array,
        QUORUM,
        direction.source_chain(),
        direction.dest_chain(),
        receipt.timestamp,
    )
    .map_err(BundleError::Attestation)?;

    let mut distinct: Vec<Pubkey> = signatures.iter().map(|s| s.relayer).collect();
    distinct.sort();
    distinct.dedup();
    Ok(VerificationReport {
        direction: bundle.direction.clone(),
        nonce: message.nonce,
        amount: message.amount,
        recipient: message.recipient,
        message_hash: hash,
        distinct_signers: distinct.len(),
        quorum: QUORUM,
        relayer_set_size: MAX_RELAYERS,
        relayer_set,
        receipt_slot: receipt.slot,
        receipt_timestamp: receipt.timestamp,
    })
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Assemble a bundle from its parts and verify it before returning:
/// export never emits a bundle its own verifier would reject.
///
/// `collection_quorum` is the threshold the signatures were
/// gathered under; the bundle itself always stamps the protocol
/// constant `QUORUM` (debate round 2026-11-18, B-F7: stamping the
/// collection argument made a stronger 5-of-5 export fail its own
/// verification with a quorum mismatch).
pub fn build_bundle(
    direction: Direction,
    message: &BridgeMessage,
    signatures: &[RelayerSignature],
    relayer_set: &[Pubkey; MAX_RELAYERS],
    collection_quorum: u8,
    receipt: &CrossingReceipt,
) -> Result<CrossingBundle, BundleError> {
    let _ = collection_quorum; // collection-only; the stamp is QUORUM
    let bundle = CrossingBundle {
        bundle_version: BUNDLE_VERSION,
        direction: direction.label().to_string(),
        message: MessageFile::from(message),
        message_hash: hex_encode(&message.hash()),
        attestations: signatures
            .iter()
            .map(|s| SignatureFile {
                relayer: s.relayer.to_string(),
                signature: bs58::encode(s.signature).into_string(),
            })
            .collect(),
        relayer_set: relayer_set.iter().map(|p| p.to_string()).collect(),
        quorum: QUORUM,
        receipt: ReceiptFile::from(receipt),
    };
    verify_bundle(&bundle)?;
    Ok(bundle)
}

/// Export a bundle for one completed crossing: collect the quorum
/// from the relayer attestation-exchange directory (the collector
/// re-verifies every signature with bridge-common, exactly as it
/// does before submitting), pair it with the crossing receipt the
/// destination program wrote, and verify the assembled bundle.
///
/// The collector's expiry check is evaluated at `receipt.slot` —
/// the destination slot at which the crossing completed.
pub fn export_bundle(
    attestations_dir: &Path,
    direction: Direction,
    nonce: u64,
    relayer_set: &[Pubkey; MAX_RELAYERS],
    quorum: u8,
    receipt: &CrossingReceipt,
) -> Result<CrossingBundle, BundleError> {
    let collector = Collector {
        relayers: *relayer_set,
        quorum,
    };
    let (message, signatures) = collector
        .collect(attestations_dir, direction, nonce, receipt.timestamp)
        .map_err(|e| BundleError::Export(format!("attestation collection: {e}")))?;
    build_bundle(direction, &message, &signatures, relayer_set, quorum, receipt)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_common::{
        CHAIN_MACHINE, CHAIN_SOLANA, DIR_SOLANA_TO_MACHINE, DISCRIMINATOR_RECEIPT, MESSAGE_VERSION,
    };
    use relayer::signing::RelayerSigner;

    /// A complete, valid crossing in data form: message, five
    /// relayer signers (seeds 21..=25), three signatures, and the
    /// receipt the destination program would have written.
    struct Fixture {
        bundle: CrossingBundle,
        signers: Vec<RelayerSigner>,
        message: BridgeMessage,
    }

    fn fixture() -> Fixture {
        let signers: Vec<RelayerSigner> = (0..5u8)
            .map(|i| RelayerSigner::from_seed(&[21 + i; 32]))
            .collect();
        let relayer_set: [Pubkey; MAX_RELAYERS] =
            signers.iter().map(|s| s.pubkey()).collect::<Vec<_>>().try_into().unwrap();
        let message = BridgeMessage {
            version: MESSAGE_VERSION,
            source_chain: CHAIN_SOLANA,
            dest_chain: CHAIN_MACHINE,
            nonce: 7,
            source_mint: Pubkey::new_from_array([3u8; 32]),
            sender: Pubkey::new_from_array([4u8; 32]),
            recipient: Pubkey::new_from_array([5u8; 32]),
            amount: 250_000,
            source_tx: [9u8; 64],
            source_slot: 12_345,
            expiry_timestamp: 1_763_060_000,
            source_timestamp: 1_762_960_000,
        };
        let signatures: Vec<RelayerSignature> =
            signers.iter().take(3).map(|s| s.sign(&message)).collect();
        let receipt = CrossingReceipt {
            discriminator: DISCRIMINATOR_RECEIPT,
            direction: DIR_SOLANA_TO_MACHINE,
            nonce: message.nonce,
            message_hash: message.hash(),
            source_tx: message.source_tx,
            amount: message.amount,
            recipient: message.recipient,
            slot: 50_000,
            timestamp: 1_763_000_000,
        };
        let bundle = build_bundle(
            Direction::SolanaToMachine,
            &message,
            &signatures,
            &relayer_set,
            QUORUM,
            &receipt,
        )
        .unwrap();
        Fixture { bundle, signers, message }
    }

    #[test]
    fn valid_bundle_verifies() {
        let fx = fixture();
        let report = verify_bundle(&fx.bundle).unwrap();
        assert_eq!(report.nonce, 7);
        assert_eq!(report.amount, 250_000);
        assert_eq!(report.distinct_signers, 3);
        assert_eq!(report.quorum, QUORUM);
        assert_eq!(report.relayer_set_size, MAX_RELAYERS);
        assert_eq!(report.message_hash, fx.message.hash());
        assert_eq!(report.receipt_slot, 50_000);
    }

    #[test]
    fn debate_non_ascii_message_hash_rejected_not_panic() {
        // Debate round 2026-11-18 (A-6/B-F5): a 64-byte hash string
        // with a multi-byte character crossing an even byte boundary
        // panicked hex_decode_32 (string slicing), crashing the
        // verifier instead of returning INVALID.
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.message_hash = format!("a\u{20ac}{}", "x".repeat(60));
        assert_eq!(bundle.message_hash.len(), 64);
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::InvalidEncoding {
                field: "message_hash".to_string(),
                reason: "not hex".to_string(),
            })
        );
    }

    #[test]
    fn debate_zero_amount_bundle_rejected() {
        // Debate round 2026-11-18 (A-8): a zero-amount message with a
        // matching receipt and valid signatures verified, although
        // both programs reject zero-amount crossings on-chain.
        let fx = fixture();
        let mut message = fx.message.clone();
        message.amount = 0;
        let signatures: Vec<RelayerSignature> =
            fx.signers.iter().take(3).map(|s| s.sign(&message)).collect();
        let receipt = CrossingReceipt {
            discriminator: DISCRIMINATOR_RECEIPT,
            direction: DIR_SOLANA_TO_MACHINE,
            nonce: message.nonce,
            message_hash: message.hash(),
            source_tx: message.source_tx,
            amount: 0,
            recipient: message.recipient,
            slot: 50_000,
            timestamp: 1_763_000_000,
        };
        let relayer_set: [Pubkey; MAX_RELAYERS] = fx
            .signers
            .iter()
            .map(|s| s.pubkey())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        assert_eq!(
            build_bundle(
                Direction::SolanaToMachine,
                &message,
                &signatures,
                &relayer_set,
                QUORUM,
                &receipt,
            ),
            Err(BundleError::ZeroAmount)
        );
    }

    #[test]
    fn debate_stronger_collection_quorum_stamps_protocol_quorum() {
        // Debate round 2026-11-18 (B-F7): build_bundle stamped the
        // collection quorum into the bundle, so a 5-of-5 export
        // failed its own verification (claimed 5, required 3).
        let fx = fixture();
        let signatures: Vec<RelayerSignature> =
            fx.signers.iter().map(|s| s.sign(&fx.message)).collect();
        let relayer_set: [Pubkey; MAX_RELAYERS] = fx
            .signers
            .iter()
            .map(|s| s.pubkey())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let receipt = CrossingReceipt {
            discriminator: DISCRIMINATOR_RECEIPT,
            direction: DIR_SOLANA_TO_MACHINE,
            nonce: fx.message.nonce,
            message_hash: fx.message.hash(),
            source_tx: fx.message.source_tx,
            amount: fx.message.amount,
            recipient: fx.message.recipient,
            slot: 50_000,
            timestamp: 1_763_000_000,
        };
        let bundle = build_bundle(
            Direction::SolanaToMachine,
            &fx.message,
            &signatures,
            &relayer_set,
            5,
            &receipt,
        )
        .unwrap();
        assert_eq!(bundle.quorum, QUORUM);
        assert_eq!(verify_bundle(&bundle).unwrap().distinct_signers, 5);
    }

    #[test]
    fn debate_verify_against_expected_set() {
        // Debate round 2026-11-18 (A-5): the report now carries the
        // set that was verified against, and a caller can pin it.
        let fx = fixture();
        let relayer_set: [Pubkey; MAX_RELAYERS] = fx
            .signers
            .iter()
            .map(|s| s.pubkey())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let report = verify_bundle(&fx.bundle).unwrap();
        assert_eq!(report.relayer_set, relayer_set.to_vec());
        verify_bundle_against_set(&fx.bundle, &relayer_set).unwrap();
        let other: [Pubkey; MAX_RELAYERS] =
            (0..5u8).map(|i| Pubkey::new_from_array([100 + i; 32])).collect::<Vec<_>>().try_into().unwrap();
        assert_eq!(
            verify_bundle_against_set(&fx.bundle, &other),
            Err(BundleError::RelayerSetMismatch)
        );
    }

    #[test]
    fn valid_5_of_5_bundle_verifies() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        for signer in fx.signers.iter().skip(3) {
            bundle.attestations.push(SignatureFile {
                relayer: signer.pubkey().to_string(),
                signature: bs58::encode(signer.sign(&fx.message).signature).into_string(),
            });
        }
        let report = verify_bundle(&bundle).unwrap();
        assert_eq!(report.distinct_signers, 5);
    }

    #[test]
    fn bundle_survives_json_round_trip() {
        // The bundle is a file format: what matters is the bytes on
        // disk, not the struct in memory.
        let fx = fixture();
        let json = serde_json::to_string_pretty(&fx.bundle).unwrap();
        let parsed: CrossingBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, fx.bundle);
        verify_bundle(&parsed).unwrap();
    }

    #[test]
    fn amount_flipped_in_message_fails() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.message.amount += 1;
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::ReceiptMismatch { field: "amount" })
        );
    }

    #[test]
    fn amount_flipped_in_receipt_fails() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.receipt.amount += 1;
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::ReceiptMismatch { field: "amount" })
        );
    }

    #[test]
    fn amount_flipped_in_both_fails_on_hash() {
        // Message and receipt agree with each other but not with
        // the hash the relayers signed: the stated hash no longer
        // matches the stated message.
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.message.amount += 1;
        bundle.receipt.amount += 1;
        assert_eq!(verify_bundle(&bundle), Err(BundleError::MessageHashMismatch));
    }

    #[test]
    fn amount_flipped_everywhere_fails_on_signature() {
        // Even with the hash fields rewritten to match the tampered
        // message, the relayers' signatures cover the original hash.
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.message.amount += 1;
        bundle.receipt.amount += 1;
        let tampered = bundle.message.to_message().unwrap();
        let h = hex_encode(&tampered.hash());
        bundle.message_hash = h.clone();
        bundle.receipt.message_hash = h;
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::Attestation(BridgeError::InvalidSignature))
        );
    }

    #[test]
    fn nonce_changed_in_message_fails() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.message.nonce = 8;
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::ReceiptMismatch { field: "nonce" })
        );
    }

    #[test]
    fn recipient_changed_in_receipt_fails() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.receipt.recipient = Pubkey::new_unique().to_string();
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::ReceiptMismatch { field: "recipient" })
        );
    }

    #[test]
    fn attestation_dropped_below_quorum_fails() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.attestations.truncate(2);
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::Attestation(BridgeError::QuorumNotReached))
        );
    }

    #[test]
    fn signature_from_non_relayer_fails() {
        let fx = fixture();
        let outsider = RelayerSigner::from_seed(&[99u8; 32]);
        let mut bundle = fx.bundle.clone();
        bundle.attestations[2] = SignatureFile {
            relayer: outsider.pubkey().to_string(),
            signature: bs58::encode(outsider.sign(&fx.message).signature).into_string(),
        };
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::Attestation(BridgeError::UnknownRelayer))
        );
    }

    #[test]
    fn duplicated_attestation_counts_once() {
        // Three entries, two distinct relayers: not a quorum.
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.attestations[2] = bundle.attestations[0].clone();
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::Attestation(BridgeError::DuplicateRelayerSignature))
        );
    }

    #[test]
    fn corrupted_signature_fails() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        let mut sig = fx.signers[0].sign(&fx.message).signature;
        sig[0] ^= 0xff;
        bundle.attestations[0].signature = bs58::encode(sig).into_string();
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::Attestation(BridgeError::InvalidSignature))
        );
    }

    #[test]
    fn relayer_set_member_swapped_fails() {
        // The stated set is what signatures are checked against:
        // swapping one member makes a real signature "unknown".
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.relayer_set[2] = Pubkey::new_unique().to_string();
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::Attestation(BridgeError::UnknownRelayer))
        );
    }

    #[test]
    fn weakened_quorum_claim_fails() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.quorum = 1;
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::QuorumMismatch { claimed: 1, required: QUORUM })
        );
    }

    #[test]
    fn completion_after_expiry_fails() {
        // The receipt claims the crossing completed after the
        // attestation expired: the on-chain program would have
        // rejected it, so the bundle must not verify.
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.receipt.timestamp = fx.message.expiry_timestamp + 1;
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::Attestation(BridgeError::ExpiredAttestation))
        );
    }

    #[test]
    fn wrong_direction_label_fails() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.direction = "machine-to-solana".to_string();
        assert_eq!(verify_bundle(&bundle), Err(BundleError::DirectionMismatch));
    }

    #[test]
    fn unsupported_version_fails() {
        let fx = fixture();
        let mut bundle = fx.bundle.clone();
        bundle.bundle_version = 2;
        assert_eq!(
            verify_bundle(&bundle),
            Err(BundleError::UnsupportedBundleVersion(2))
        );
    }
}
