//! Receipt indexer (bridge/RECEIPT-INDEXER.md).
//!
//! Every completed bridge crossing writes a **crossing receipt**: a
//! PDA derived from (direction, nonce) holding the attested
//! message's hash, amount, recipient, and the destination slot and
//! timestamp at completion (BRIDGE.md 4.5). One thing the receipt
//! deliberately does *not* hold is the destination transaction's
//! own signature — a program cannot observe it — so the design
//! leaves that binding to indexers: resolve the receipt PDA's
//! creating transaction off-chain.
//!
//! This crate is that indexer. Given a destination program, a
//! direction, and a nonce, it:
//!
//! 1. derives the receipt PDA itself (`bridge_common::receipt_pda`)
//!    — it never takes an address on trust;
//! 2. parses the receipt account **strictly**: the account must be
//!    owned by the destination program, be exactly
//!    `CrossingReceipt::LEN` bytes, carry the receipt
//!    discriminator, and its own (direction, nonce) fields must
//!    re-derive the very address it was read from;
//! 3. resolves the creating transaction from the PDA's signature
//!    list (`getSignaturesForAddress` shape): the **oldest
//!    successful** signature — failed transactions created nothing,
//!    and a later failed replay attempt against the same nonce
//!    must never shadow the real crossing (the Ankr failure,
//!    Nov 2026, was five forged receipts recycled from one
//!    already-redeemed deposit; a receipt that can be traced to
//!    exactly one creating transaction is how that shape is
//!    audited after the fact);
//! 4. verifies the creating transaction (`getTransaction`,
//!    jsonParsed shape): it succeeded, it executed in exactly the
//!    slot the receipt records, its account list contains the
//!    receipt PDA, and it carries a top-level instruction of the
//!    destination program whose account list contains the receipt
//!    PDA — the MintWrapped / Unlock that wrote the receipt;
//! 5. emits one JSON record binding all of it: receipt fields,
//!    receipt PDA, destination transaction signature and slot.
//!
//! A receipt PDA is written exactly once (replay fails on-chain),
//! so more than one *successful* signature for the address is
//! itself an anomaly: the record flags it
//! (`extra_successful_signatures`) instead of hiding it, while the
//! binding still resolves to the oldest — the creation.
//!
//! Everything except the small RPC client in the binary is a pure
//! function over the JSON shapes an RPC node returns, so the whole
//! binding is testable offline against saved RPC output — and the
//! binary's `offline` mode runs exactly that path on files.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use borsh::BorshDeserialize;
use bridge_common::{receipt_pda, CrossingReceipt, DISCRIMINATOR_RECEIPT};
use relayer::message::Direction;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use solana_program::pubkey::Pubkey;
use std::fmt;
use std::str::FromStr;

/// Record format version. A reader rejects any other version
/// rather than guessing at changed semantics.
pub const RECORD_VERSION: u8 = 1;

// ---------------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------------

/// The receipt's own fields, in the wire encodings the relayer
/// exchange and the proof bundle use (hash hex, source tx base58).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptFields {
    pub direction: u8,
    pub nonce: u64,
    /// hex, 32 bytes.
    pub message_hash: String,
    /// base58, 64 bytes.
    pub source_tx: String,
    pub amount: u64,
    pub recipient: String,
    /// Destination-chain slot at completion (as the program wrote).
    pub slot: u64,
    /// Destination-chain unix timestamp at completion.
    pub timestamp: i64,
}

impl From<&CrossingReceipt> for ReceiptFields {
    fn from(r: &CrossingReceipt) -> Self {
        Self {
            direction: r.direction,
            nonce: r.nonce,
            message_hash: r.message_hash.iter().map(|b| format!("{b:02x}")).collect(),
            source_tx: bs58::encode(r.source_tx).into_string(),
            amount: r.amount,
            recipient: r.recipient.to_string(),
            slot: r.slot,
            timestamp: r.timestamp,
        }
    }
}

/// One fully bound crossing record: the on-chain receipt plus the
/// destination transaction that created it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptRecord {
    pub record_version: u8,
    /// The destination program that wrote the receipt.
    pub program_id: String,
    /// The receipt PDA (derived, never supplied).
    pub receipt_pda: String,
    /// Direction label: `solana-to-machine` or `machine-to-solana`.
    pub direction: String,
    pub nonce: u64,
    pub receipt: ReceiptFields,
    /// base58 signature of the destination transaction that
    /// created the receipt — the binding BRIDGE.md 4.5 leaves to
    /// indexers.
    pub destination_tx: String,
    /// Slot of that transaction; verified equal to the receipt's
    /// own recorded slot.
    pub destination_slot: u64,
    /// Block time of that transaction, when the RPC reports it.
    pub destination_block_time: Option<i64>,
    /// How many signature-list entries the RPC returned for the
    /// receipt PDA in total (successful and failed).
    pub signatures_for_receipt: usize,
    /// How many of those entries are successful transactions.
    pub successful_signatures: usize,
    /// True when more than one successful transaction touched the
    /// receipt PDA. The receipt is written exactly once, so this
    /// is an anomaly worth a human's attention — reported, never
    /// hidden; the binding above is still the oldest (creation).
    pub extra_successful_signatures: bool,
}

// ---------------------------------------------------------------------------
// Errors — every rejection names the specific check that failed
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// No account exists at the derived receipt PDA: this crossing
    /// has not completed (or the program/direction/nonce is wrong).
    ReceiptNotFound,
    WrongOwner { expected: String, actual: String },
    BadAccountData(String),
    /// The address queried is not the PDA the receipt's own
    /// (direction, nonce) fields derive.
    PdaMismatch { expected: String, actual: String },
    /// The receipt's fields disagree with the requested crossing.
    CrossingMismatch { field: &'static str },
    NoSignatures,
    NoSuccessfulSignature,
    TransactionNotFound,
    TransactionFailed,
    SlotMismatch { what: &'static str, receipt_slot: u64, other_slot: u64 },
    ReceiptNotInTransaction,
    ProgramInstructionMissing,
    RpcShape(String),
    InvalidPubkey { field: String, value: String },
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexError::ReceiptNotFound => {
                write!(f, "no receipt account at the derived PDA — this crossing has not completed on the destination chain")
            }
            IndexError::WrongOwner { expected, actual } => {
                write!(f, "receipt account is owned by {actual}, not the destination program {expected}")
            }
            IndexError::BadAccountData(m) => write!(f, "receipt account data is invalid: {m}"),
            IndexError::PdaMismatch { expected, actual } => {
                write!(f, "receipt was read from {actual} but its own fields derive {expected}")
            }
            IndexError::CrossingMismatch { field } => {
                write!(f, "receipt {field} disagrees with the requested crossing")
            }
            IndexError::NoSignatures => {
                write!(f, "the RPC returned no signatures for the receipt PDA")
            }
            IndexError::NoSuccessfulSignature => {
                write!(f, "no successful signature for the receipt PDA — failed transactions create nothing")
            }
            IndexError::TransactionNotFound => {
                write!(f, "the creating transaction could not be fetched")
            }
            IndexError::TransactionFailed => {
                write!(f, "the resolved creating transaction has a non-null meta.err — it created nothing")
            }
            IndexError::SlotMismatch { what, receipt_slot, other_slot } => {
                write!(f, "{what} is in slot {other_slot}, but the receipt records slot {receipt_slot}")
            }
            IndexError::ReceiptNotInTransaction => {
                write!(f, "the resolved transaction's account list does not contain the receipt PDA")
            }
            IndexError::ProgramInstructionMissing => {
                write!(f, "the resolved transaction has no instruction of the destination program writing the receipt PDA")
            }
            IndexError::RpcShape(m) => write!(f, "unexpected RPC response shape: {m}"),
            IndexError::InvalidPubkey { field, value } => {
                write!(f, "{field} is not a valid pubkey: '{value}'")
            }
        }
    }
}

impl std::error::Error for IndexError {}

fn parse_pubkey(field: &str, s: &str) -> Result<Pubkey, IndexError> {
    Pubkey::from_str(s).map_err(|_| IndexError::InvalidPubkey {
        field: field.to_string(),
        value: s.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Step 2 — strict receipt parsing
// ---------------------------------------------------------------------------

/// Parse a receipt account strictly: owner must be the destination
/// program, data must be exactly `CrossingReceipt::LEN` bytes with
/// the receipt discriminator, and the address the account was read
/// from must be the PDA its own (direction, nonce) fields derive.
pub fn parse_receipt_account(
    program_id: &Pubkey,
    address: &Pubkey,
    owner: &Pubkey,
    data: &[u8],
) -> Result<CrossingReceipt, IndexError> {
    if owner != program_id {
        return Err(IndexError::WrongOwner {
            expected: program_id.to_string(),
            actual: owner.to_string(),
        });
    }
    if data.len() != CrossingReceipt::LEN {
        return Err(IndexError::BadAccountData(format!(
            "expected exactly {} bytes, got {}",
            CrossingReceipt::LEN,
            data.len()
        )));
    }
    let receipt = CrossingReceipt::try_from_slice(data)
        .map_err(|e| IndexError::BadAccountData(format!("borsh decode: {e}")))?;
    if receipt.discriminator != DISCRIMINATOR_RECEIPT {
        return Err(IndexError::BadAccountData(format!(
            "discriminator {} is not the receipt discriminator {DISCRIMINATOR_RECEIPT}",
            receipt.discriminator
        )));
    }
    let (derived, _) = receipt_pda(program_id, receipt.direction, receipt.nonce);
    if derived != *address {
        return Err(IndexError::PdaMismatch {
            expected: derived.to_string(),
            actual: address.to_string(),
        });
    }
    Ok(receipt)
}

/// Extract (owner, data) from a `getAccountInfo` result value.
/// `Ok(None)` is the account-does-not-exist case.
pub fn parse_account_info(result: &Value) -> Result<Option<(Pubkey, Vec<u8>)>, IndexError> {
    let value = result
        .get("value")
        .ok_or_else(|| IndexError::RpcShape("getAccountInfo: no value field".into()))?;
    if value.is_null() {
        return Ok(None);
    }
    let owner = parse_pubkey(
        "account.owner",
        value["owner"]
            .as_str()
            .ok_or_else(|| IndexError::RpcShape("getAccountInfo: no owner".into()))?,
    )?;
    let b64 = value["data"][0]
        .as_str()
        .ok_or_else(|| IndexError::RpcShape("getAccountInfo: no base64 data".into()))?;
    let data = B64
        .decode(b64)
        .map_err(|e| IndexError::RpcShape(format!("getAccountInfo: base64: {e}")))?;
    Ok(Some((owner, data)))
}

// ---------------------------------------------------------------------------
// Step 3 — resolve the creating transaction from the signature list
// ---------------------------------------------------------------------------

/// The creation binding extracted from a signature list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Creation {
    pub signature: [u8; 64],
    pub slot: u64,
    pub block_time: Option<i64>,
    /// Total entries the RPC returned for the address.
    pub total: usize,
    /// Entries whose transaction succeeded (`err` null).
    pub successful: usize,
}

/// Resolve the creating transaction from a `getSignaturesForAddress`
/// result array (newest first): the **oldest successful** entry.
/// Failed transactions created nothing, so they are skipped — a
/// failed replay attempt against the same nonce, however recent,
/// never shadows the real crossing.
pub fn resolve_creation(signatures: &Value) -> Result<Creation, IndexError> {
    let arr = signatures
        .as_array()
        .ok_or_else(|| IndexError::RpcShape("getSignaturesForAddress: not an array".into()))?;
    if arr.is_empty() {
        return Err(IndexError::NoSignatures);
    }
    let successful: Vec<&Value> = arr.iter().filter(|e| e["err"].is_null()).collect();
    let entry = successful.last().ok_or(IndexError::NoSuccessfulSignature)?;
    let sig_str = entry["signature"]
        .as_str()
        .ok_or_else(|| IndexError::RpcShape("getSignaturesForAddress: no signature".into()))?;
    let bytes = bs58::decode(sig_str)
        .into_vec()
        .map_err(|e| IndexError::RpcShape(format!("signature bs58: {e}")))?;
    let signature: [u8; 64] = bytes
        .try_into()
        .map_err(|_| IndexError::RpcShape("signature is not 64 bytes".into()))?;
    Ok(Creation {
        signature,
        slot: entry["slot"].as_u64().unwrap_or(0),
        block_time: entry["blockTime"].as_i64(),
        total: arr.len(),
        successful: successful.len(),
    })
}

// ---------------------------------------------------------------------------
// Step 4 — verify the creating transaction
// ---------------------------------------------------------------------------

/// Verify a `getTransaction` result (jsonParsed) really is the
/// transaction that created this receipt:
///
/// - it succeeded (`meta.err` null);
/// - it executed in exactly the slot the receipt records — the
///   program wrote that slot from the Clock sysvar as it created
///   the receipt, so any other transaction is the wrong one;
/// - its account list contains the receipt PDA;
/// - it carries a top-level instruction of the destination
///   program whose account list contains the receipt PDA (the
///   MintWrapped / Unlock that wrote it — an unrelated successful
///   transaction that merely references the PDA, e.g. a failed
///   replay's sibling or a balance check, is not a creation).
pub fn verify_creation_transaction(
    tx: &Value,
    program_id: &Pubkey,
    receipt_address: &Pubkey,
    receipt: &CrossingReceipt,
) -> Result<(), IndexError> {
    if tx.is_null() {
        return Err(IndexError::TransactionNotFound);
    }
    if tx.pointer("/meta/err").map(|e| !e.is_null()).unwrap_or(true) {
        return Err(IndexError::TransactionFailed);
    }
    let tx_slot = tx["slot"]
        .as_u64()
        .ok_or_else(|| IndexError::RpcShape("getTransaction: no slot".into()))?;
    if tx_slot != receipt.slot {
        return Err(IndexError::SlotMismatch {
            what: "the resolved transaction",
            receipt_slot: receipt.slot,
            other_slot: tx_slot,
        });
    }
    let pda_str = receipt_address.to_string();
    let account_keys = tx
        .pointer("/transaction/message/accountKeys")
        .and_then(|v| v.as_array())
        .ok_or_else(|| IndexError::RpcShape("getTransaction: no accountKeys".into()))?;
    // jsonParsed account keys are objects ({pubkey, signer,
    // writable}); plain json encoding uses bare strings. Accept both.
    let key_matches = |k: &Value| -> bool {
        k.as_str() == Some(pda_str.as_str())
            || k.get("pubkey").and_then(|p| p.as_str()) == Some(pda_str.as_str())
    };
    if !account_keys.iter().any(key_matches) {
        return Err(IndexError::ReceiptNotInTransaction);
    }
    let instructions = tx
        .pointer("/transaction/message/instructions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| IndexError::RpcShape("getTransaction: no instructions".into()))?;
    let program_str = program_id.to_string();
    let writes_receipt = instructions.iter().any(|ix| {
        ix.get("programId").and_then(|p| p.as_str()) == Some(program_str.as_str())
            && ix
                .get("accounts")
                .and_then(|a| a.as_array())
                .map(|accounts| accounts.iter().any(|a| a.as_str() == Some(pda_str.as_str())))
                .unwrap_or(false)
    });
    if !writes_receipt {
        return Err(IndexError::ProgramInstructionMissing);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The whole binding
// ---------------------------------------------------------------------------

/// Run the full indexing binding over RPC-shaped values:
///
/// - `account_result`: a `getAccountInfo` result for the derived
///   receipt PDA;
/// - `signatures_result`: a `getSignaturesForAddress` result for
///   the same PDA;
/// - `tx_result`: a `getTransaction` result for the resolved
///   creating signature.
///
/// The signature list's own slot for the creation entry must also
/// agree with the receipt's slot — three independent RPC views
/// (account, signature list, transaction) pinned to one slot.
pub fn index_crossing(
    program_id: &Pubkey,
    direction: Direction,
    nonce: u64,
    account_result: &Value,
    signatures_result: &Value,
    tx_result: &Value,
) -> Result<ReceiptRecord, IndexError> {
    let (pda, _) = receipt_pda(program_id, direction.nonce_dir(), nonce);
    let (owner, data) = parse_account_info(account_result)?.ok_or(IndexError::ReceiptNotFound)?;
    let receipt = parse_receipt_account(program_id, &pda, &owner, &data)?;
    if receipt.direction != direction.nonce_dir() {
        return Err(IndexError::CrossingMismatch { field: "direction" });
    }
    if receipt.nonce != nonce {
        return Err(IndexError::CrossingMismatch { field: "nonce" });
    }
    let creation = resolve_creation(signatures_result)?;
    if creation.slot != receipt.slot {
        return Err(IndexError::SlotMismatch {
            what: "the signature list's creation entry",
            receipt_slot: receipt.slot,
            other_slot: creation.slot,
        });
    }
    verify_creation_transaction(tx_result, program_id, &pda, &receipt)?;
    Ok(ReceiptRecord {
        record_version: RECORD_VERSION,
        program_id: program_id.to_string(),
        receipt_pda: pda.to_string(),
        direction: direction.label().to_string(),
        nonce,
        receipt: ReceiptFields::from(&receipt),
        destination_tx: bs58::encode(creation.signature).into_string(),
        destination_slot: receipt.slot,
        destination_block_time: creation.block_time,
        signatures_for_receipt: creation.total,
        successful_signatures: creation.successful,
        extra_successful_signatures: creation.successful > 1,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_common::{DIR_SOLANA_TO_MACHINE, DISCRIMINATOR_RECEIPT};
    use serde_json::json;

    fn program() -> Pubkey {
        Pubkey::new_from_array([42u8; 32])
    }

    fn receipt() -> CrossingReceipt {
        CrossingReceipt {
            discriminator: DISCRIMINATOR_RECEIPT,
            direction: DIR_SOLANA_TO_MACHINE,
            nonce: 7,
            message_hash: [3u8; 32],
            source_tx: [9u8; 64],
            amount: 250_000,
            recipient: Pubkey::new_from_array([5u8; 32]),
            slot: 50_000,
            timestamp: 1_763_000_000,
        }
    }

    fn pda() -> Pubkey {
        receipt_pda(&program(), DIR_SOLANA_TO_MACHINE, 7).0
    }

    fn account_result(r: &CrossingReceipt) -> Value {
        let data = borsh::to_vec(&r).unwrap();
        json!({
            "value": {
                "owner": program().to_string(),
                "data": [B64.encode(&data), "base64"],
                "lamports": 1_000_000,
                "executable": false,
            }
        })
    }

    fn sig_entry(sig: &[u8; 64], slot: u64, err: bool) -> Value {
        json!({
            "signature": bs58::encode(sig).into_string(),
            "slot": slot,
            "err": if err { json!({"InstructionError": [1, {"Custom": 1}]}) } else { Value::Null },
            "blockTime": 1_763_000_000,
            "confirmationStatus": "finalized",
        })
    }

    fn creation_tx(r: &CrossingReceipt) -> Value {
        json!({
            "slot": r.slot,
            "blockTime": 1_763_000_000,
            "meta": {"err": null},
            "transaction": {"message": {
                "accountKeys": [
                    {"pubkey": Pubkey::new_unique().to_string(), "signer": true, "writable": true},
                    {"pubkey": pda().to_string(), "signer": false, "writable": true},
                ],
                "instructions": [
                    {"programId": program().to_string(),
                     "accounts": [pda().to_string()],
                     "data": "deadbeef"}
                ],
            }},
        })
    }

    fn index(
        account: &Value,
        sigs: &Value,
        tx: &Value,
    ) -> Result<ReceiptRecord, IndexError> {
        index_crossing(
            &program(),
            Direction::SolanaToMachine,
            7,
            account,
            sigs,
            tx,
        )
    }

    #[test]
    fn valid_crossing_indexes() {
        let r = receipt();
        let sig = [11u8; 64];
        let record = index(
            &account_result(&r),
            &json!([sig_entry(&sig, r.slot, false)]),
            &creation_tx(&r),
        )
        .unwrap();
        assert_eq!(record.receipt_pda, pda().to_string());
        assert_eq!(record.destination_tx, bs58::encode(sig).into_string());
        assert_eq!(record.destination_slot, 50_000);
        assert_eq!(record.receipt.amount, 250_000);
        assert_eq!(record.successful_signatures, 1);
        assert!(!record.extra_successful_signatures);
        // The record is a file format: it must round-trip.
        let parsed: ReceiptRecord =
            serde_json::from_str(&serde_json::to_string_pretty(&record).unwrap()).unwrap();
        assert_eq!(parsed, record);
    }

    #[test]
    fn missing_receipt_is_not_found_not_an_error_shape() {
        let r = receipt();
        let sig = [11u8; 64];
        let err = index(
            &json!({"value": null}),
            &json!([sig_entry(&sig, r.slot, false)]),
            &creation_tx(&r),
        )
        .unwrap_err();
        assert_eq!(err, IndexError::ReceiptNotFound);
    }

    #[test]
    fn wrong_owner_rejected() {
        let r = receipt();
        let account = json!({
            "value": {
                "owner": Pubkey::new_unique().to_string(),
                "data": [B64.encode(borsh::to_vec(&r).unwrap()), "base64"],
            }
        });
        let sig = [11u8; 64];
        let err = index(&account, &json!([sig_entry(&sig, r.slot, false)]), &creation_tx(&r))
            .unwrap_err();
        assert!(matches!(err, IndexError::WrongOwner { .. }));
    }

    #[test]
    fn truncated_account_rejected() {
        let r = receipt();
        let mut data = borsh::to_vec(&r).unwrap();
        data.truncate(100);
        let account = json!({
            "value": {"owner": program().to_string(), "data": [B64.encode(&data), "base64"]}
        });
        let sig = [11u8; 64];
        let err = index(&account, &json!([sig_entry(&sig, r.slot, false)]), &creation_tx(&r))
            .unwrap_err();
        assert!(matches!(err, IndexError::BadAccountData(_)));
    }

    #[test]
    fn receipt_for_another_nonce_at_this_address_rejected() {
        // A receipt whose own fields say nonce 8, read from the
        // nonce-7 PDA address: the fields re-derive a different
        // address than the one queried.
        let mut r = receipt();
        r.nonce = 8;
        let sig = [11u8; 64];
        let err = index(
            &account_result(&r),
            &json!([sig_entry(&sig, r.slot, false)]),
            &creation_tx(&receipt()),
        )
        .unwrap_err();
        assert!(matches!(err, IndexError::PdaMismatch { .. }));
    }

    #[test]
    fn failed_replay_does_not_shadow_creation() {
        // Newest first: a later FAILED replay attempt against the
        // same nonce, then the real creation. The binding must
        // resolve to the creation, not the replay.
        let r = receipt();
        let creation_sig = [11u8; 64];
        let replay_sig = [12u8; 64];
        let sigs = json!([
            sig_entry(&replay_sig, r.slot + 500, true),
            sig_entry(&creation_sig, r.slot, false),
        ]);
        let record = index(&account_result(&r), &sigs, &creation_tx(&r)).unwrap();
        assert_eq!(record.destination_tx, bs58::encode(creation_sig).into_string());
        assert_eq!(record.signatures_for_receipt, 2);
        assert_eq!(record.successful_signatures, 1);
    }

    #[test]
    fn extra_successful_signatures_are_flagged_not_hidden() {
        let r = receipt();
        let creation_sig = [11u8; 64];
        let other_sig = [13u8; 64];
        let sigs = json!([
            sig_entry(&other_sig, r.slot + 500, false),
            sig_entry(&creation_sig, r.slot, false),
        ]);
        let record = index(&account_result(&r), &sigs, &creation_tx(&r)).unwrap();
        assert_eq!(record.destination_tx, bs58::encode(creation_sig).into_string());
        assert!(record.extra_successful_signatures);
        assert_eq!(record.successful_signatures, 2);
    }

    #[test]
    fn all_failed_signatures_rejected() {
        let r = receipt();
        let sig = [11u8; 64];
        let err = index(
            &account_result(&r),
            &json!([sig_entry(&sig, r.slot, true)]),
            &creation_tx(&r),
        )
        .unwrap_err();
        assert_eq!(err, IndexError::NoSuccessfulSignature);
    }

    #[test]
    fn empty_signature_list_rejected() {
        let r = receipt();
        let err = index(&account_result(&r), &json!([]), &creation_tx(&r)).unwrap_err();
        assert_eq!(err, IndexError::NoSignatures);
    }

    #[test]
    fn failed_creating_transaction_rejected() {
        let r = receipt();
        let sig = [11u8; 64];
        let mut tx = creation_tx(&r);
        tx["meta"]["err"] = json!({"InstructionError": [1, {"Custom": 0}]});
        let err = index(
            &account_result(&r),
            &json!([sig_entry(&sig, r.slot, false)]),
            &tx,
        )
        .unwrap_err();
        assert_eq!(err, IndexError::TransactionFailed);
    }

    #[test]
    fn transaction_in_wrong_slot_rejected() {
        // A transaction from another slot that references the PDA
        // is not the transaction that wrote this receipt — the
        // program recorded the creation slot in the receipt itself.
        let r = receipt();
        let sig = [11u8; 64];
        let mut tx = creation_tx(&r);
        tx["slot"] = json!(r.slot + 1);
        let err = index(
            &account_result(&r),
            &json!([sig_entry(&sig, r.slot, false)]),
            &tx,
        )
        .unwrap_err();
        assert!(matches!(err, IndexError::SlotMismatch { .. }));
    }

    #[test]
    fn signature_list_slot_disagreeing_with_receipt_rejected() {
        let r = receipt();
        let sig = [11u8; 64];
        let err = index(
            &account_result(&r),
            &json!([sig_entry(&sig, r.slot + 9, false)]),
            &creation_tx(&r),
        )
        .unwrap_err();
        assert!(matches!(err, IndexError::SlotMismatch { .. }));
    }

    #[test]
    fn transaction_without_the_program_instruction_rejected() {
        // The transaction references the PDA but no instruction of
        // the destination program writes it: not a creation.
        let r = receipt();
        let sig = [11u8; 64];
        let mut tx = creation_tx(&r);
        tx["transaction"]["message"]["instructions"][0]["programId"] =
            json!(Pubkey::new_unique().to_string());
        let err = index(
            &account_result(&r),
            &json!([sig_entry(&sig, r.slot, false)]),
            &tx,
        )
        .unwrap_err();
        assert_eq!(err, IndexError::ProgramInstructionMissing);
    }

    #[test]
    fn transaction_without_receipt_account_rejected() {
        let r = receipt();
        let sig = [11u8; 64];
        let mut tx = creation_tx(&r);
        tx["transaction"]["message"]["accountKeys"] = json!([
            {"pubkey": Pubkey::new_unique().to_string(), "signer": true, "writable": true}
        ]);
        let err = index(
            &account_result(&r),
            &json!([sig_entry(&sig, r.slot, false)]),
            &tx,
        )
        .unwrap_err();
        assert_eq!(err, IndexError::ReceiptNotInTransaction);
    }
}
