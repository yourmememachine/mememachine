//! Relayer configuration.
//!
//! One JSON file per relayer operator. Every operator must run with
//! the same chain/program/mint values and the same
//! `expiry_window_slots` — those values are inside the signed message
//! hash, so a mismatch does not cause a bad mint, it causes (safely)
//! no quorum. Each operator has a *different* `relayer_keypair_path`.
//!
//! Example: see `relayer.example.json` in this crate.

use crate::{RelayerError, Result};
use serde::{Deserialize, Serialize};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;

pub fn parse_pubkey(field: &str, value: &str) -> Result<Pubkey> {
    Pubkey::from_str(value)
        .map_err(|e| RelayerError::InvalidConfig(format!("{field}: '{value}': {e}")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayerConfig {
    /// JSON-RPC endpoint for Solana (the lock program side).
    pub solana_rpc_url: String,
    /// JSON-RPC endpoint for Machine (the bridge program side).
    pub machine_rpc_url: String,
    /// Solana-side lock program id.
    pub lock_program_id: String,
    /// Machine-side bridge program id.
    pub machine_program_id: String,
    /// Original SPL mint on Solana.
    pub source_mint: String,
    /// Wrapped SPL mint on Machine (created at bridge deployment).
    pub wrapped_mint: String,
    /// Vault token account on Solana (owner = lock program authority PDA).
    pub vault: String,
    /// This relayer's Solana-format keypair file (JSON array of 64 bytes).
    /// Generate fresh for production; never use the dev keys in this repo.
    pub relayer_keypair_path: String,
    /// Keypair that pays fees for destination submissions. May be the
    /// same file as the relayer keypair on a testnet; in production the
    /// roles are separate keys (BRIDGE.md 4.2).
    pub payer_keypair_path: String,
    /// Directory where signed attestation files are exchanged with the
    /// other relayers (phase 1 transport; see attestation.rs).
    pub attestation_dir: String,
    /// File holding the next-nonce cursors, so a restart does not
    /// re-process or skip events.
    pub state_path: String,
    /// Deterministic expiry: expiry_slot = source_slot + this value.
    /// MUST be identical for every relayer. Default 100_000 slots
    /// (~11 hours on Solana's ~400ms slots; Machine slots are faster,
    /// so the window is longer there — expiry is checked against the
    /// *destination* chain's slot in the programs).
    #[serde(default = "default_expiry_window_slots")]
    pub expiry_window_slots: u64,
    /// Extra finalized slots required beyond the finalized slot itself
    /// for Solana -> Machine (BRIDGE.md 2.5: finalized + 16 margin).
    #[serde(default = "default_solana_safety_margin_slots")]
    pub solana_safety_margin_slots: u64,
    /// Extra rooted slots required for Machine -> Solana. Rooted is
    /// already final on Machine; default margin 0.
    #[serde(default)]
    pub machine_safety_margin_slots: u64,
    /// First nonce to watch in each direction. Nonces are strictly
    /// increasing per direction; the watcher probes them in order and
    /// never skips one.
    #[serde(default)]
    pub start_nonce: u64,
    /// Poll interval for the `run` loop.
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Safety switch: submissions are dry-run unless this is true.
    /// Even with a quorum, a relayer with `submit_enabled = false`
    /// only reports the transaction it *would* send.
    #[serde(default)]
    pub submit_enabled: bool,
}

fn default_expiry_window_slots() -> u64 {
    100_000
}
fn default_solana_safety_margin_slots() -> u64 {
    16
}
fn default_poll_interval_secs() -> u64 {
    10
}

impl RelayerConfig {
    pub fn load(path: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cfg: RelayerConfig = serde_json::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.expiry_window_slots == 0 {
            return Err(RelayerError::InvalidConfig(
                "expiry_window_slots must be > 0".into(),
            ));
        }
        // Parse every pubkey field now so a typo fails at startup,
        // not after an event has been observed.
        parse_pubkey("lock_program_id", &self.lock_program_id)?;
        parse_pubkey("machine_program_id", &self.machine_program_id)?;
        parse_pubkey("source_mint", &self.source_mint)?;
        parse_pubkey("wrapped_mint", &self.wrapped_mint)?;
        parse_pubkey("vault", &self.vault)?;
        if self.solana_rpc_url.is_empty() || self.machine_rpc_url.is_empty() {
            return Err(RelayerError::InvalidConfig("rpc urls must not be empty".into()));
        }
        Ok(())
    }

    pub fn lock_program_id(&self) -> Pubkey {
        parse_pubkey("lock_program_id", &self.lock_program_id).expect("validated")
    }
    pub fn machine_program_id(&self) -> Pubkey {
        parse_pubkey("machine_program_id", &self.machine_program_id).expect("validated")
    }
    pub fn source_mint(&self) -> Pubkey {
        parse_pubkey("source_mint", &self.source_mint).expect("validated")
    }
    pub fn wrapped_mint(&self) -> Pubkey {
        parse_pubkey("wrapped_mint", &self.wrapped_mint).expect("validated")
    }
    pub fn vault(&self) -> Pubkey {
        parse_pubkey("vault", &self.vault).expect("validated")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config_json() -> String {
        let a = Pubkey::new_unique().to_string();
        format!(
            r#"{{
              "solana_rpc_url": "http://127.0.0.1:8899",
              "machine_rpc_url": "http://127.0.0.1:8899",
              "lock_program_id": "{a}",
              "machine_program_id": "{a}",
              "source_mint": "{a}",
              "wrapped_mint": "{a}",
              "vault": "{a}",
              "relayer_keypair_path": "/tmp/relayer.json",
              "payer_keypair_path": "/tmp/payer.json",
              "attestation_dir": "/tmp/attestations",
              "state_path": "/tmp/relayer-state.json"
            }}"#
        )
    }

    #[test]
    fn defaults_are_applied() {
        let cfg: RelayerConfig = serde_json::from_str(&valid_config_json()).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.expiry_window_slots, 100_000);
        assert_eq!(cfg.solana_safety_margin_slots, 16);
        assert_eq!(cfg.machine_safety_margin_slots, 0);
        assert_eq!(cfg.poll_interval_secs, 10);
        assert!(!cfg.submit_enabled);
    }

    #[test]
    fn bad_pubkey_rejected_at_validation() {
        let mut cfg: RelayerConfig = serde_json::from_str(&valid_config_json()).unwrap();
        cfg.vault = "not-a-pubkey".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn zero_expiry_window_rejected() {
        let mut cfg: RelayerConfig = serde_json::from_str(&valid_config_json()).unwrap();
        cfg.expiry_window_slots = 0;
        assert!(cfg.validate().is_err());
    }
}
