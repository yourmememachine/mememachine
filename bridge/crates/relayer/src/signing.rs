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

/// Refuse a keypair file that group/other users can read. Shared
/// by the relayer signer and the submission payer (debate
/// scope-2r2, A-F5/B-F4): the payer key holds the fee float and
/// was previously loaded with no permission check at all.
pub fn check_keypair_permissions(path: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(RelayerError::InvalidKeypair(format!(
                "{path}: keypair file permissions {:o} are too open; require 0600",
                mode & 0o777
            )));
        }
    }
    Ok(())
}

impl RelayerSigner {
    /// Load a Solana-format keypair file: a JSON array of 64 bytes
    /// (32-byte seed || 32-byte public key), as produced by
    /// `solana-keygen new -o relayer.json`.
    pub fn load(path: &str) -> Result<Self> {
        // A signing key readable by group/other users on a shared
        // host is a relayer identity waiting to be taken; refuse it.
        check_keypair_permissions(path)?;
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
            expiry_timestamp: 100_010,
            source_timestamp: 10,
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

    #[cfg(unix)]
    #[test]
    fn keypair_permission_check_refuses_open_files() {
        // Debate scope-2r2 (A-F5/B-F4): the shared permission
        // check (relayer signer AND submission payer) refuses a
        // group/other-readable keypair file and accepts 0600.
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!(
            "relayer-perm-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"[0]").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check_keypair_permissions(path.to_str().unwrap()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check_keypair_permissions(path.to_str().unwrap()).is_ok());
        let _ = std::fs::remove_file(&path);
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
