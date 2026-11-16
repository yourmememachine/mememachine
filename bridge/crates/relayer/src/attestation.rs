//! Attestation exchange and quorum collection.
//!
//! Phase-1 transport is deliberately boring: each relayer writes one
//! JSON file per (direction, nonce, relayer) into a shared directory
//! (a synced folder, an object-store bucket mounted locally, or — on a
//! single testnet host — one directory all five relayers run against).
//! Files are immutable evidence: a relayer that signs two conflicting
//! messages for the same nonce leaves two files with its name on both,
//! which is exactly the provable fault BRIDGE.md 3.3 removes a relayer
//! for. A network peer protocol can replace this transport later
//! without changing the collector: it consumes `AttestationFile`s,
//! wherever they came from.
//!
//! The collector is strict:
//! - the message must hash to the same value in every file, otherwise
//!   it reports `Disagreement` and collects nothing (BRIDGE.md 3.2);
//! - a relayer may contribute at most one signature;
//! - signers must be in the registered relayer set;
//! - the collected set is re-verified with bridge-common's own
//!   `verify_attestation` before it is handed to the submitter, so the
//!   off-chain check can never be weaker than the on-chain one.

use crate::message::Direction;
use crate::{RelayerError, Result};
use bridge_common::{verify_attestation, BridgeMessage, RelayerSignature, MAX_RELAYERS};
use serde::{Deserialize, Serialize};
use solana_program::pubkey::Pubkey;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

fn pubkey_to_string(p: &Pubkey) -> String {
    p.to_string()
}
fn bytes64_to_string(b: &[u8; 64]) -> String {
    bs58::encode(b).into_string()
}
fn string_to_pubkey(field: &str, s: &str) -> Result<Pubkey> {
    Pubkey::from_str(s)
        .map_err(|e| RelayerError::InvalidPubkey(format!("{field} '{s}': {e}")))
}
fn string_to_bytes64(field: &str, s: &str) -> Result<[u8; 64]> {
    let v = bs58::decode(s)
        .into_vec()
        .map_err(|e| RelayerError::InvalidSignature(format!("{field}: {e}")))?;
    v.try_into()
        .map_err(|_| RelayerError::InvalidSignature(format!("{field}: not 64 bytes")))
}

/// Wire form of a `BridgeMessage` (pubkeys / signatures as base58,
/// because `Pubkey`'s serde form is a byte array, which is painful to
/// inspect by eye in an incident).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFile {
    pub version: u8,
    pub source_chain: u8,
    pub dest_chain: u8,
    pub nonce: u64,
    pub source_mint: String,
    pub sender: String,
    pub recipient: String,
    pub amount: u64,
    pub source_tx: String,
    pub source_slot: u64,
    pub expiry_slot: u64,
}

impl From<&BridgeMessage> for MessageFile {
    fn from(m: &BridgeMessage) -> Self {
        Self {
            version: m.version,
            source_chain: m.source_chain,
            dest_chain: m.dest_chain,
            nonce: m.nonce,
            source_mint: pubkey_to_string(&m.source_mint),
            sender: pubkey_to_string(&m.sender),
            recipient: pubkey_to_string(&m.recipient),
            amount: m.amount,
            source_tx: bytes64_to_string(&m.source_tx),
            source_slot: m.source_slot,
            expiry_slot: m.expiry_slot,
        }
    }
}

impl MessageFile {
    pub fn to_message(&self) -> Result<BridgeMessage> {
        Ok(BridgeMessage {
            version: self.version,
            source_chain: self.source_chain,
            dest_chain: self.dest_chain,
            nonce: self.nonce,
            source_mint: string_to_pubkey("source_mint", &self.source_mint)?,
            sender: string_to_pubkey("sender", &self.sender)?,
            recipient: string_to_pubkey("recipient", &self.recipient)?,
            amount: self.amount,
            source_tx: string_to_bytes64("source_tx", &self.source_tx)?,
            source_slot: self.source_slot,
            expiry_slot: self.expiry_slot,
        })
    }
}

/// One relayer's signed attestation, as exchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationFile {
    pub direction: String,
    pub message: MessageFile,
    /// sha256 message hash, hex — redundant on purpose, so a human
    /// scanning the directory can spot disagreement at a glance.
    pub message_hash_hex: String,
    pub relayer: String,
    pub signature: String,
}

pub fn hash_hex(message: &BridgeMessage) -> String {
    message.hash().iter().map(|b| format!("{b:02x}")).collect()
}

impl AttestationFile {
    pub fn new(direction: Direction, message: &BridgeMessage, sig: &RelayerSignature) -> Self {
        Self {
            direction: direction.label().to_string(),
            message: MessageFile::from(message),
            message_hash_hex: hash_hex(message),
            relayer: pubkey_to_string(&sig.relayer),
            signature: bytes64_to_string(&sig.signature),
        }
    }

    pub fn to_parts(&self) -> Result<(BridgeMessage, RelayerSignature)> {
        let message = self.message.to_message()?;
        if self.message_hash_hex != hash_hex(&message) {
            return Err(RelayerError::InvalidSignature(format!(
                "attestation file for nonce {}: stored hash does not match message",
                message.nonce
            )));
        }
        Ok((
            message,
            RelayerSignature {
                relayer: string_to_pubkey("relayer", &self.relayer)?,
                signature: string_to_bytes64("signature", &self.signature)?,
            },
        ))
    }

    pub fn file_name(&self) -> String {
        format!(
            "{}-{}-{}.json",
            self.direction, self.message.nonce, self.relayer
        )
    }

    pub fn write_to(&self, dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(self.file_name());
        // Write-then-rename so a collector never reads a half file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    pub fn read_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }
}

/// Collect a quorum for one (direction, nonce) from a directory.
pub struct Collector {
    pub relayers: [Pubkey; MAX_RELAYERS],
    pub quorum: u8,
}

impl Collector {
    /// Read every attestation file for this direction+nonce, enforce
    /// agreement, and return the message plus at least `quorum`
    /// distinct valid signatures, re-verified with bridge-common.
    ///
    /// `current_dest_slot` is used for the expiry check inside
    /// `verify_attestation`, exactly as the destination program will.
    pub fn collect(
        &self,
        dir: &Path,
        direction: Direction,
        nonce: u64,
        current_dest_slot: u64,
    ) -> Result<(BridgeMessage, Vec<RelayerSignature>)> {
        let mut message: Option<BridgeMessage> = None;
        let mut hashes: Vec<String> = Vec::new();
        let mut by_relayer: BTreeMap<Pubkey, RelayerSignature> = BTreeMap::new();

        if dir.exists() {
            let prefix = format!("{}-{}-", direction.label(), nonce);
            let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with(&prefix) && n.ends_with(".json"))
                        .unwrap_or(false)
                })
                .collect();
            entries.sort();
            for path in entries {
                let file = AttestationFile::read_from(&path)?;
                let (msg, sig) = file.to_parts()?;
                if msg.nonce != nonce {
                    continue;
                }
                let h = hash_hex(&msg);
                if !hashes.contains(&h) {
                    hashes.push(h);
                }
                match &message {
                    None => message = Some(msg),
                    Some(m) if *m != msg => {
                        return Err(RelayerError::Disagreement { nonce, hashes });
                    }
                    _ => {}
                }
                if !self.relayers.contains(&sig.relayer) {
                    return Err(RelayerError::UnknownRelayer(sig.relayer.to_string()));
                }
                if by_relayer.insert(sig.relayer, sig).is_some() {
                    return Err(RelayerError::DuplicateRelayer(
                        // insert replaced an entry only if the key existed;
                        // we returned the pubkey string above is lost, so
                        // reconstruct it here.
                        by_relayer_key_string(&path),
                    ));
                }
            }
        }

        let message = message.ok_or(RelayerError::QuorumNotReached {
            have: 0,
            need: self.quorum,
        })?;
        let signatures: Vec<RelayerSignature> = by_relayer.into_values().collect();
        if (signatures.len() as u8) < self.quorum {
            return Err(RelayerError::QuorumNotReached {
                have: signatures.len(),
                need: self.quorum,
            });
        }
        // Re-verify off-chain with the program's own verifier.
        verify_attestation(
            &message,
            &signatures,
            &self.relayers,
            self.quorum,
            direction.source_chain(),
            direction.dest_chain(),
            current_dest_slot,
        )
        .map_err(|e| RelayerError::InvalidSignature(format!("attestation failed verification: {e:?}")))?;
        Ok((message, signatures))
    }
}

fn by_relayer_key_string(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::RelayerSigner;
    use bridge_common::{MESSAGE_VERSION, CHAIN_SOLANA, CHAIN_MACHINE};
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn setup() -> (Vec<RelayerSigner>, [Pubkey; 5], BridgeMessage) {
        let mut rng = OsRng;
        let mut signers = Vec::new();
        let mut pubs = Vec::new();
        for _ in 0..5 {
            let k = SigningKey::generate(&mut rng);
            signers.push(RelayerSigner::from_seed(&k.to_bytes()));
            pubs.push(Pubkey::new_from_array(k.verifying_key().to_bytes()));
        }
        let msg = BridgeMessage {
            version: MESSAGE_VERSION,
            source_chain: CHAIN_SOLANA,
            dest_chain: CHAIN_MACHINE,
            nonce: 7,
            source_mint: Pubkey::new_unique(),
            sender: Pubkey::new_unique(),
            recipient: Pubkey::new_unique(),
            amount: 1_000,
            source_tx: [5u8; 64],
            source_slot: 100,
            expiry_slot: 100_100,
        };
        (signers, pubs.try_into().unwrap(), msg)
    }

    fn write_n(dir: &Path, signers: &[RelayerSigner], msg: &BridgeMessage, n: usize) {
        for s in signers.iter().take(n) {
            AttestationFile::new(Direction::SolanaToMachine, msg, &s.sign(msg))
                .write_to(dir)
                .unwrap();
        }
    }

    #[test]
    fn three_of_five_collects_and_verifies() {
        let (signers, pubs, msg) = setup();
        let dir = std::env::temp_dir().join(format!("relayer-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_n(&dir, &signers, &msg, 3);
        let c = Collector { relayers: pubs, quorum: 3 };
        let (m, sigs) = c
            .collect(&dir, Direction::SolanaToMachine, 7, 500)
            .unwrap();
        assert_eq!(m, msg);
        assert_eq!(sigs.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_of_five_is_not_a_quorum() {
        let (signers, pubs, msg) = setup();
        let dir = std::env::temp_dir().join(format!("relayer-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_n(&dir, &signers, &msg, 2);
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(matches!(
            c.collect(&dir, Direction::SolanaToMachine, 7, 500),
            Err(RelayerError::QuorumNotReached { have: 2, need: 3 })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disagreement_blocks_collection() {
        let (signers, pubs, msg) = setup();
        let dir = std::env::temp_dir().join(format!("relayer-test3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_n(&dir, &signers, &msg, 2);
        // A third relayer signs a *different* amount for the same nonce.
        let mut other = msg.clone();
        other.amount = 999_999;
        AttestationFile::new(
            Direction::SolanaToMachine,
            &other,
            &signers[2].sign(&other),
        )
        .write_to(&dir)
        .unwrap();
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(matches!(
            c.collect(&dir, Direction::SolanaToMachine, 7, 500),
            Err(RelayerError::Disagreement { nonce: 7, .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn attestation_file_round_trips() {
        let (signers, _pubs, msg) = setup();
        let f = AttestationFile::new(Direction::MachineToSolana, &msg, &signers[0].sign(&msg));
        let text = serde_json::to_string(&f).unwrap();
        let back: AttestationFile = serde_json::from_str(&text).unwrap();
        assert_eq!(f, back);
        let (m, s) = back.to_parts().unwrap();
        assert_eq!(m, msg);
        assert_eq!(s.relayer, signers[0].pubkey());
    }

    // ---------------------------------------------------------------
    // Malformed exchange files must fail CLOSED (backlog #4).
    //
    // Exchange files are untrusted input: another relayer's host, a
    // synced folder, or an attacker with write access to the share
    // can put anything in the directory. For every malformed shape
    // below, `collect` must return an error — never panic, never
    // hand the submitter a partial attestation, and never let the
    // malformed file substitute for a valid signature toward the
    // quorum threshold.
    // ---------------------------------------------------------------

    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("relayer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_raw(dir: &Path, file_name: &str, bytes: &[u8]) {
        std::fs::write(dir.join(file_name), bytes).unwrap();
    }

    #[test]
    fn truncated_attestation_file_fails_closed() {
        let (signers, pubs, msg) = setup();
        let dir = fresh_dir("truncated");
        write_n(&dir, &signers, &msg, 2);
        // Third relayer's file, cut in half mid-JSON (interrupted
        // sync, disk-full peer, torn write on the share).
        let good = AttestationFile::new(
            Direction::SolanaToMachine,
            &msg,
            &signers[2].sign(&msg),
        );
        let text = serde_json::to_string_pretty(&good).unwrap();
        write_raw(&dir, &good.file_name(), &text.as_bytes()[..text.len() / 2]);
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(matches!(
            c.collect(&dir, Direction::SolanaToMachine, 7, 500),
            Err(RelayerError::Json(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_bytes_attestation_file_fails_closed() {
        let (signers, pubs, msg) = setup();
        let dir = fresh_dir("garbage");
        write_n(&dir, &signers, &msg, 2);
        // Not even valid UTF-8, under a perfectly plausible file name.
        let name = format!("solana-to-machine-7-{}.json", signers[2].pubkey());
        write_raw(&dir, &name, b"\xff\xfe\x00 not an attestation {{{");
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(c.collect(&dir, Direction::SolanaToMachine, 7, 500).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_attestation_file_fails_closed() {
        let (signers, pubs, msg) = setup();
        let dir = fresh_dir("empty");
        write_n(&dir, &signers, &msg, 2);
        let name = format!("solana-to-machine-7-{}.json", signers[2].pubkey());
        write_raw(&dir, &name, b"");
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(matches!(
            c.collect(&dir, Direction::SolanaToMachine, 7, 500),
            Err(RelayerError::Json(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_length_signature_field_fails_closed() {
        let (signers, pubs, msg) = setup();
        let dir = fresh_dir("badsiglen");
        write_n(&dir, &signers, &msg, 2);
        // Valid JSON, valid message, but the signature field decodes
        // to 32 bytes instead of 64.
        let mut bad = AttestationFile::new(
            Direction::SolanaToMachine,
            &msg,
            &signers[2].sign(&msg),
        );
        bad.signature = bs58::encode([1u8; 32]).into_string();
        let name = bad.file_name();
        write_raw(&dir, &name, serde_json::to_string_pretty(&bad).unwrap().as_bytes());
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(matches!(
            c.collect(&dir, Direction::SolanaToMachine, 7, 500),
            Err(RelayerError::InvalidSignature(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forged_signature_fails_closed_at_verification() {
        let (signers, pubs, msg) = setup();
        let dir = fresh_dir("forgedsig");
        write_n(&dir, &signers, &msg, 2);
        // A genuine signature by relayer 3 — but over a *different*
        // message (amount 42), transplanted onto this message's file.
        // The file parses, the stored hash matches the message, and
        // the signature is 64 well-formed bytes; only the final
        // bridge-common verification can catch it, and it must.
        let mut other = msg.clone();
        other.amount = 42;
        let forged = AttestationFile::new(
            Direction::SolanaToMachine,
            &msg,
            &signers[2].sign(&other),
        );
        let name = forged.file_name();
        write_raw(&dir, &name, serde_json::to_string_pretty(&forged).unwrap().as_bytes());
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(matches!(
            c.collect(&dir, Direction::SolanaToMachine, 7, 500),
            Err(RelayerError::InvalidSignature(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tampered_message_hash_fails_closed() {
        let (signers, pubs, msg) = setup();
        let dir = fresh_dir("badhash");
        write_n(&dir, &signers, &msg, 2);
        // The stored (redundant) hash no longer matches the message
        // in the same file: `to_parts` must reject before counting.
        let mut bad = AttestationFile::new(
            Direction::SolanaToMachine,
            &msg,
            &signers[2].sign(&msg),
        );
        bad.message_hash_hex = "00".repeat(32);
        let name = bad.file_name();
        write_raw(&dir, &name, serde_json::to_string_pretty(&bad).unwrap().as_bytes());
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(matches!(
            c.collect(&dir, Direction::SolanaToMachine, 7, 500),
            Err(RelayerError::InvalidSignature(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_for_another_nonce_never_counts_toward_quorum() {
        let (signers, pubs, msg) = setup();
        let dir = fresh_dir("wrongnonce");
        write_n(&dir, &signers, &msg, 2);
        // A fully valid attestation — for nonce 8 — wearing a
        // nonce-7 file name. It must be skipped, not counted: the
        // two genuine nonce-7 signatures are not a quorum.
        let mut msg8 = msg.clone();
        msg8.nonce = 8;
        let alien = AttestationFile::new(
            Direction::SolanaToMachine,
            &msg8,
            &signers[2].sign(&msg8),
        );
        let name = format!("solana-to-machine-7-{}.json", signers[2].pubkey());
        write_raw(&dir, &name, serde_json::to_string_pretty(&alien).unwrap().as_bytes());
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(matches!(
            c.collect(&dir, Direction::SolanaToMachine, 7, 500),
            Err(RelayerError::QuorumNotReached { have: 2, need: 3 })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_file_fails_closed_even_when_quorum_is_present() {
        let (signers, pubs, msg) = setup();
        let dir = fresh_dir("mixedbad");
        // A full valid quorum of 3, plus a fourth relayer's file
        // corrupted. Strict fail-closed: collection errors out
        // rather than silently skipping the bad file — a human
        // investigates the exchange directory; nothing is submitted.
        write_n(&dir, &signers, &msg, 3);
        let name = format!("solana-to-machine-7-{}.json", signers[3].pubkey());
        write_raw(&dir, &name, b"{\"direction\": torn write");
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(c.collect(&dir, Direction::SolanaToMachine, 7, 500).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_file_never_substitutes_for_a_missing_signature() {
        let (signers, pubs, msg) = setup();
        let dir = fresh_dir("mixedshort");
        // Two valid signatures plus one malformed file: the bad file
        // must not be counted as the third — no quorum, no partial
        // attestation, just an error.
        write_n(&dir, &signers, &msg, 2);
        let name = format!("solana-to-machine-7-{}.json", signers[2].pubkey());
        write_raw(&dir, &name, b"garbage");
        let c = Collector { relayers: pubs, quorum: 3 };
        assert!(c.collect(&dir, Direction::SolanaToMachine, 7, 500).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
