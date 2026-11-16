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

/// What the source transaction must prove before the relayer signs.
///
/// Coreum lesson (Aug 2026): relayers attested to a transaction that
/// moved the bridge's own wrapped token between two attacker wallets
/// and treated it as a deposit, because nothing checked that the
/// transaction actually delivered funds to the bridge. The check
/// below binds the transfer the relayer relies on to the event
/// record: the same amount, into the vault (deposit) or burning the
/// wrapped mint (burn). Anything else fails closed — including a
/// transaction that contains a transfer, but not the right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceTransferCheck {
    /// Deposit: the vault token account that must receive the funds.
    /// Burn: None — a burn has no destination.
    pub destination: Option<Pubkey>,
    /// Burn: the mint that must be burned. Deposit: None — the vault
    /// account is already mint-specific, so destination + amount bind
    /// the transfer; a plain SPL `transfer` carries no mint field.
    pub mint: Option<Pubkey>,
    /// Must equal the event record's amount.
    pub amount: u64,
}

/// Extract the sender from a parsed source transaction, verifying
/// that the transaction really contains the transfer behind the
/// event record.
///
/// The deposit/burn transfer is a CPI of the lock/bridge program, so
/// it lives in `meta.innerInstructions`, not in the top-level
/// instruction list — the old scan of top-level instructions only
/// could not find it in a real deposit transaction, and would have
/// attributed the sender of any unrelated top-level transfer instead.
/// This scans top-level instructions first, then inner instructions
/// in execution order, and returns the authority of the first
/// instruction of `kind` whose destination (deposit) or mint (burn)
/// and amount match the check. No match means no signature:
/// the relayer refuses to sign rather than signing a message whose
/// sender it cannot prove.
///
/// Pure function over the RPC JSON, so it is unit-testable with
/// fixtures and never guesses.
pub fn sender_from_verified_source_transfer(
    tx: &Value,
    kind: &str,
    check: &SourceTransferCheck,
) -> Result<Pubkey> {
    let mut candidates: Vec<&Value> = Vec::new();
    if let Some(ixs) = tx
        .pointer("/transaction/message/instructions")
        .and_then(|v| v.as_array())
    {
        candidates.extend(ixs.iter());
    }
    if let Some(groups) = tx
        .pointer("/meta/innerInstructions")
        .and_then(|v| v.as_array())
    {
        for group in groups {
            if let Some(ixs) = group.get("instructions").and_then(|v| v.as_array()) {
                candidates.extend(ixs.iter());
            }
        }
    }
    if candidates.is_empty() {
        return Err(RelayerError::SenderNotFound(
            "no instructions in transaction".into(),
        ));
    }
    let mut saw_kind = false;
    for ix in candidates {
        let parsed = &ix["parsed"];
        if parsed["type"].as_str().unwrap_or("") != kind {
            continue;
        }
        saw_kind = true;
        let info = &parsed["info"];
        let amount = info["amount"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .or_else(|| info["amount"].as_u64());
        if amount != Some(check.amount) {
            continue; // Wrong amount: not the transfer backing this record.
        }
        if let Some(dest) = check.destination {
            match info["destination"]
                .as_str()
                .and_then(|s| Pubkey::from_str(s).ok())
            {
                Some(d) if d == dest => {}
                _ => continue, // Not into the vault: not this deposit.
            }
        }
        if let Some(mint) = check.mint {
            match info["mint"].as_str().and_then(|s| Pubkey::from_str(s).ok()) {
                Some(m) if m == mint => {}
                _ => continue, // Wrong mint: not this burn.
            }
        }
        if let Some(authority) = info["authority"].as_str() {
            return Pubkey::from_str(authority).map_err(|e| {
                RelayerError::SenderNotFound(format!("authority '{authority}': {e}"))
            });
        }
    }
    if saw_kind {
        Err(RelayerError::SourceTransferNotVerified(format!(
            "no '{kind}' instruction matches the event record (amount {}, destination {:?}, mint {:?}); refusing to sign",
            check.amount, check.destination, check.mint
        )))
    } else {
        Err(RelayerError::SenderNotFound(format!(
            "no parsed '{kind}' instruction with an authority"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic deposit transaction: one top-level instruction
    /// (the lock program's Deposit, unparsed — it is not a known
    /// program) and the token transfer as a CPI in
    /// `meta.innerInstructions`.
    fn deposit_tx(vault: &Pubkey, authority: &Pubkey, amount: &str) -> Value {
        json!({
            "transaction": {"message": {"instructions": [
                {"programId": "Lock111111111111111111111111111111111111111",
                 "accounts": [], "data": "deadbeef"}
            ]}},
            "meta": {"innerInstructions": [
                {"index": 0, "instructions": [
                    {"program": "spl-token", "parsed": {"type": "transfer", "info": {
                        "source": Pubkey::new_unique().to_string(),
                        "destination": vault.to_string(),
                        "authority": authority.to_string(),
                        "amount": amount
                    }}}
                ]}
            ]}
        })
    }

    /// A realistic burn transaction: the burn is likewise a CPI of
    /// the bridge program's Burn instruction.
    fn burn_tx(mint: &Pubkey, authority: &Pubkey, amount: &str) -> Value {
        json!({
            "transaction": {"message": {"instructions": [
                {"programId": "Bridge1111111111111111111111111111111111111",
                 "accounts": [], "data": "deadbeef"}
            ]}},
            "meta": {"innerInstructions": [
                {"index": 0, "instructions": [
                    {"program": "spl-token", "parsed": {"type": "burn", "info": {
                        "account": Pubkey::new_unique().to_string(),
                        "mint": mint.to_string(),
                        "authority": authority.to_string(),
                        "amount": amount
                    }}}
                ]}
            ]}
        })
    }

    fn deposit_check(vault: Pubkey, amount: u64) -> SourceTransferCheck {
        SourceTransferCheck {
            destination: Some(vault),
            mint: None,
            amount,
        }
    }

    fn burn_check(mint: Pubkey, amount: u64) -> SourceTransferCheck {
        SourceTransferCheck {
            destination: None,
            mint: Some(mint),
            amount,
        }
    }

    #[test]
    fn deposit_sender_found_in_inner_instructions() {
        let vault = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let tx = deposit_tx(&vault, &user, "500");
        let sender =
            sender_from_verified_source_transfer(&tx, "transfer", &deposit_check(vault, 500))
                .unwrap();
        assert_eq!(sender, user);
    }

    #[test]
    fn burn_sender_verified_by_mint_and_amount() {
        let mint = Pubkey::new_unique();
        let burner = Pubkey::new_unique();
        let tx = burn_tx(&mint, &burner, "300");
        let sender =
            sender_from_verified_source_transfer(&tx, "burn", &burn_check(mint, 300)).unwrap();
        assert_eq!(sender, burner);
    }

    #[test]
    fn phantom_deposit_rejected_coreum_case() {
        // The Coreum failure: a transaction that moves tokens between
        // two attacker wallets, with no transfer into the vault, must
        // never yield a sender — it is not a deposit.
        let vault = Pubkey::new_unique();
        let attacker = Pubkey::new_unique();
        let tx = json!({
            "transaction": {"message": {"instructions": [
                {"program": "spl-token", "parsed": {"type": "transfer", "info": {
                    "source": Pubkey::new_unique().to_string(),
                    "destination": Pubkey::new_unique().to_string(),
                    "authority": attacker.to_string(),
                    "amount": "500"
                }}}
            ]}},
            "meta": {"innerInstructions": []}
        });
        let err = sender_from_verified_source_transfer(&tx, "transfer", &deposit_check(vault, 500))
            .unwrap_err();
        assert!(matches!(
            err,
            RelayerError::SourceTransferNotVerified(_)
        ));
    }

    #[test]
    fn inner_transfer_to_wrong_destination_rejected() {
        let vault = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let mut tx = deposit_tx(&Pubkey::new_unique(), &user, "500");
        // Same amount, but the CPI transfer went somewhere else.
        let err = sender_from_verified_source_transfer(&tx, "transfer", &deposit_check(vault, 500))
            .unwrap_err();
        assert!(matches!(
            err,
            RelayerError::SourceTransferNotVerified(_)
        ));
        // And the right vault with the same tx must succeed, proving
        // the rejection is about the destination, not the shape.
        tx = deposit_tx(&vault, &user, "500");
        assert_eq!(
            sender_from_verified_source_transfer(&tx, "transfer", &deposit_check(vault, 500))
                .unwrap(),
            user
        );
    }

    #[test]
    fn wrong_amount_rejected() {
        let vault = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let tx = deposit_tx(&vault, &user, "499");
        let err = sender_from_verified_source_transfer(&tx, "transfer", &deposit_check(vault, 500))
            .unwrap_err();
        assert!(matches!(
            err,
            RelayerError::SourceTransferNotVerified(_)
        ));
    }

    #[test]
    fn decoy_top_level_transfer_ignored() {
        // A batched transaction with an unrelated top-level transfer
        // before the Deposit must still attribute the sender of the
        // transfer that actually went into the vault.
        let vault = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let decoy = Pubkey::new_unique();
        let mut tx = deposit_tx(&vault, &user, "500");
        tx["transaction"]["message"]["instructions"]
            .as_array_mut()
            .unwrap()
            .insert(
                0,
                json!({"program": "spl-token", "parsed": {"type": "transfer", "info": {
                    "source": Pubkey::new_unique().to_string(),
                    "destination": Pubkey::new_unique().to_string(),
                    "authority": decoy.to_string(),
                    "amount": "500"
                }}}),
            );
        let sender =
            sender_from_verified_source_transfer(&tx, "transfer", &deposit_check(vault, 500))
                .unwrap();
        assert_eq!(sender, user);
        assert_ne!(sender, decoy);
    }

    #[test]
    fn burn_with_wrong_mint_rejected() {
        let mint = Pubkey::new_unique();
        let burner = Pubkey::new_unique();
        let tx = burn_tx(&Pubkey::new_unique(), &burner, "300");
        let err = sender_from_verified_source_transfer(&tx, "burn", &burn_check(mint, 300))
            .unwrap_err();
        assert!(matches!(
            err,
            RelayerError::SourceTransferNotVerified(_)
        ));
    }

    #[test]
    fn sender_missing_is_an_error_not_a_guess() {
        let tx = json!({"transaction": {"message": {"instructions": []}}});
        assert!(sender_from_verified_source_transfer(
            &tx,
            "transfer",
            &deposit_check(Pubkey::new_unique(), 500)
        )
        .is_err());
    }
}
