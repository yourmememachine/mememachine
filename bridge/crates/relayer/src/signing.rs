//! Relayer keypair loading and attestation signing.
//!
//! A relayer keypair is a normal Solana keypair: an ed25519 key whose
//! public key is the relayer's registered `Pubkey` in the on-chain
//! config. The programs verify signatures in-program with
//! ed25519-dalek over the message hash — signing here is the exact
//! counterpart: ed25519 over `message.hash()`, nothing else.

use crate::{RelayerError, Result};
use bridge_common::{BridgeMessage, RelayerSignature};
use ed25519_dalek::{Signer, SigningKey};
use solana_program::pubkey::Pubkey;

#[derive(Clone)]
pub struct RelayerSigner {
    key: SigningKey,
}

impl RelayerSigner {
    /// Load a Solana-format keypair file: a JSON array of 64 bytes
    /// (32-byte seed || 32-byte public key), as produced by
    /// `solana-keygen new -o relayer.json`.
    pub fn load(path: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let bytes: Vec<u8> = serde_json::from_str(&text).map_err(|e| {
            RelayerError::InvalidKeypair(format!("{path}: not a JSON byte array: {e}"))
        })?;
        Self::from_keypair_bytes(&bytes)
    }

    pub fn from_keypair_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 64 {
            return Err(RelayerError::InvalidKeypair(format!(
                "expected 64 keypair bytes, got {}",
                bytes.len()
            )));
        }
        let key = SigningKey::from_keypair_bytes(bytes.try_into().unwrap())
            .map_err(|e| RelayerError::InvalidKeypair(e.to_string()))?;
        // The file's stored public half must match the secret half,
        // otherwise this keypair would never verify on-chain.
        if key.verifying_key().to_bytes() != bytes[32..] {
            return Err(RelayerError::InvalidKeypair(
                "public half does not match secret half".into(),
            ));
        }
        Ok(Self { key })
    }

    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            key: SigningKey::from_bytes(seed),
        }
    }

    pub fn pubkey(&self) -> Pubkey {
        Pubkey::new_from_array(self.key.verifying_key().to_bytes())
    }

    /// Sign a bridge message: ed25519 over the message hash.
    /// Relayers sign the hash, never the raw fields (BRIDGE.md 2.3).
    pub fn sign(&self, message: &BridgeMessage) -> RelayerSignature {
        let hash = message.hash();
        RelayerSignature {
            relayer: self.pubkey(),
            signature: self.key.sign(&hash).to_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_common::{verify_attestation, CHAIN_MACHINE, CHAIN_SOLANA, MESSAGE_VERSION, QUORUM};
    use ed25519_dalek::SigningKey as DalekKey;
    use rand::rngs::OsRng;

    fn message() -> BridgeMessage {
        BridgeMessage {
            version: MESSAGE_VERSION,
            source_chain: CHAIN_SOLANA,
            dest_chain: CHAIN_MACHINE,
            nonce: 1,
            source_mint: Pubkey::new_unique(),
            sender: Pubkey::new_unique(),
            recipient: Pubkey::new_unique(),
            amount: 42,
            source_tx: [3u8; 64],
            source_slot: 10,
            expiry_slot: 100_010,
        }
    }

    #[test]
    fn signature_verifies_with_bridge_common() {
        // Build 5 relayers the way solana-keygen would: dalek keys,
        // pubkey = verifying key bytes.
        let mut rng = OsRng;
        let mut signers = Vec::new();
        let mut pubs = Vec::new();
        for _ in 0..5 {
            let dalek = DalekKey::generate(&mut rng);
            let seed = dalek.to_bytes();
            signers.push(RelayerSigner::from_seed(&seed));
            pubs.push(Pubkey::new_from_array(dalek.verifying_key().to_bytes()));
        }
        let msg = message();
        let sigs: Vec<_> = signers.iter().take(3).map(|s| s.sign(&msg)).collect();
        let relayers: [Pubkey; 5] = pubs.try_into().unwrap();
        assert!(verify_attestation(
            &msg, &sigs, &relayers, QUORUM, CHAIN_SOLANA, CHAIN_MACHINE, 500
        )
        .is_ok());
    }

    #[test]
    fn keypair_file_round_trip() {
        let mut rng = OsRng;
        let dalek = DalekKey::generate(&mut rng);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&dalek.to_bytes());
        bytes.extend_from_slice(&dalek.verifying_key().to_bytes());
        let signer = RelayerSigner::from_keypair_bytes(&bytes).unwrap();
        assert_eq!(
            signer.pubkey(),
            Pubkey::new_from_array(dalek.verifying_key().to_bytes())
        );
        // Truncated / mismatched files are refused.
        assert!(RelayerSigner::from_keypair_bytes(&bytes[..32]).is_err());
        let mut bad = bytes.clone();
        bad[63] ^= 0xff;
        assert!(RelayerSigner::from_keypair_bytes(&bad).is_err());
    }
}
