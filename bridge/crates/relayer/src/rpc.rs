//! Minimal JSON-RPC client for Solana-style chains (Solana mainnet
//! and Machine, which is an Agave fork and speaks the same RPC).
//!
//! Only the calls the relayer needs, each pinned to `finalized`
//! commitment where it matters — a relayer must never act on a
//! processed/confirmed view of the source chain (BRIDGE.md 2.5).
//! Transaction submission builds a real signed transaction with
//! solana-sdk and posts it as base64, the `sendTransaction` wire form.

use crate::{RelayerError, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use borsh::BorshDeserialize;
use bridge_common::EventRecord;
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;

pub struct RpcClient {
    url: String,
    agent: ureq::Agent,
}

impl RpcClient {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
        }
    }

    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let resp: Value = self
            .agent
            .post(&self.url)
            .send_json(body)
            .map_err(|e| RelayerError::Rpc(format!("{method}: {e}")))?
            .into_json()
            .map_err(|e| RelayerError::Rpc(format!("{method}: response json: {e}")))?;
        if let Some(err) = resp.get("error") {
            return Err(RelayerError::Rpc(format!("{method}: {err}")));
        }
        resp.get("result")
            .cloned()
            .ok_or_else(|| RelayerError::Rpc(format!("{method}: no result field")))
    }

    /// Latest finalized (Solana) / rooted (Machine) slot.
    pub fn finalized_slot(&self) -> Result<u64> {
        let r = self.call("getSlot", json!([{"commitment": "finalized"}]))?;
        r.as_u64()
            .ok_or_else(|| RelayerError::Rpc("getSlot: not a number".into()))
    }

    /// Raw account data, or None if the account does not exist.
    /// Read at finalized commitment.
    pub fn account_data(&self, pubkey: &Pubkey) -> Result<Option<Vec<u8>>> {
        let r = self.call(
            "getAccountInfo",
            json!([pubkey.to_string(), {"encoding": "base64", "commitment": "finalized"}]),
        )?;
        let value = &r["value"];
        if value.is_null() {
            return Ok(None);
        }
        let b64 = value["data"][0]
            .as_str()
            .ok_or_else(|| RelayerError::Rpc("getAccountInfo: no base64 data".into()))?;
        Ok(Some(B64.decode(b64).map_err(|e| {
            RelayerError::Rpc(format!("getAccountInfo: base64: {e}"))
        })?))
    }

    /// Decode an `EventRecord` account (a recorded deposit or burn).
    pub fn event_record(&self, record_pda: &Pubkey) -> Result<Option<EventRecord>> {
        match self.account_data(record_pda)? {
            None => Ok(None),
            Some(data) => {
                let rec = EventRecord::try_from_slice(&data)
                    .map_err(RelayerError::Borsh)?;
                if rec.discriminator != bridge_common::DISCRIMINATOR_EVENT_RECORD {
                    return Err(RelayerError::Rpc(format!(
                        "account {record_pda} is not an event record (discriminator {})",
                        rec.discriminator
                    )));
                }
                Ok(Some(rec))
            }
        }
    }

    /// The signature of the transaction that created / first touched
    /// an account, at finalized commitment. For an event-record PDA
    /// that is the deposit/burn transaction — this becomes the
    /// message's `source_tx`.
    pub fn first_signature_for_address(&self, pubkey: &Pubkey) -> Result<([u8; 64], u64)> {
        let r = self.call(
            "getSignaturesForAddress",
            json!([pubkey.to_string(), {"limit": 1000, "commitment": "finalized"}]),
        )?;
        let arr = r
            .as_array()
            .ok_or_else(|| RelayerError::Rpc("getSignaturesForAddress: not an array".into()))?;
        // Results are newest first; the record was created by the oldest
        // successful signature for its address.
        let entry = arr
            .iter()
            .rev()
            .find(|e| e["err"].is_null())
            .ok_or_else(|| {
                RelayerError::EventNotFound(format!("no successful signature for {pubkey}"))
            })?;
        let sig_str = entry["signature"]
            .as_str()
            .ok_or_else(|| RelayerError::Rpc("getSignaturesForAddress: no signature".into()))?;
        let bytes = bs58::decode(sig_str)
            .into_vec()
            .map_err(|e| RelayerError::Rpc(format!("signature bs58: {e}")))?;
        let sig: [u8; 64] = bytes
            .try_into()
            .map_err(|_| RelayerError::Rpc("signature is not 64 bytes".into()))?;
        let slot = entry["slot"].as_u64().unwrap_or(0);
        Ok((sig, slot))
    }

    /// Full parsed transaction (jsonParsed), at finalized commitment.
    pub fn transaction_parsed(&self, signature: &[u8; 64]) -> Result<Value> {
        let sig_str = bs58::encode(signature).into_string();
        self.call(
            "getTransaction",
            json!([sig_str, {"encoding": "jsonParsed", "commitment": "finalized", "maxSupportedTransactionVersion": 0}]),
        )
    }

    pub fn latest_blockhash(&self) -> Result<String> {
        let r = self.call(
            "getLatestBlockhash",
            json!([{"commitment": "finalized"}]),
        )?;
        r["value"]["blockhash"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| RelayerError::Rpc("getLatestBlockhash: no blockhash".into()))
    }

    /// Submit an already-signed transaction (base64 wire form).
    /// Returns the transaction signature.
    pub fn send_transaction(&self, base64_tx: &str) -> Result<String> {
        let r = self.call(
            "sendTransaction",
            json!([base64_tx, {"encoding": "base64", "skipPreflight": false}]),
        )?;
        r.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| RelayerError::Rpc("sendTransaction: no signature".into()))
    }
}

/// Extract the sender (the token owner's pubkey) from a parsed
/// deposit or burn transaction.
///
/// Deposit: the parsed SPL-token `transfer` instruction into the
/// vault — its `authority` is the depositor.
/// Burn: the parsed SPL-token `burn` instruction — its `authority`
/// is the burner.
/// Pure function over the RPC JSON, so it is unit-testable with a
/// fixture and never guesses: no matching instruction, no sender,
/// and the relayer refuses to sign.
pub fn sender_from_parsed_transaction(tx: &Value, kind: &str) -> Result<Pubkey> {
    let instructions = tx
        .pointer("/transaction/message/instructions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RelayerError::SenderNotFound("no instructions in transaction".into()))?;
    for ix in instructions {
        let parsed = &ix["parsed"];
        let ty = parsed["type"].as_str().unwrap_or("");
        if ty == kind {
            if let Some(authority) = parsed["info"]["authority"].as_str() {
                return Pubkey::from_str(authority).map_err(|e| {
                    RelayerError::SenderNotFound(format!("authority '{authority}': {e}"))
                });
            }
        }
    }
    Err(RelayerError::SenderNotFound(format!(
        "no parsed '{kind}' instruction with an authority"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(kind: &str) -> Value {
        let authority = Pubkey::new_unique().to_string();
        let tx = json!({
            "transaction": {"message": {"instructions": [
                {"program": "system", "parsed": {"type": "createAccount", "info": {}}},
                {"program": "spl-token", "parsed": {"type": kind, "info": {"authority": authority, "amount": "1000"}}}
            ]}}
        });
        // Stash the expected value inside the fixture's meta for the test.
        json!({"tx": tx, "authority": authority})
    }

    #[test]
    fn sender_extracted_for_deposit_and_burn() {
        for kind in ["transfer", "burn"] {
            let f = fixture(kind);
            let sender = sender_from_parsed_transaction(&f["tx"], kind).unwrap();
            assert_eq!(sender.to_string(), f["authority"].as_str().unwrap());
        }
    }

    #[test]
    fn sender_missing_is_an_error_not_a_guess() {
        let tx = json!({"transaction": {"message": {"instructions": []}}});
        assert!(sender_from_parsed_transaction(&tx, "transfer").is_err());
    }
}
