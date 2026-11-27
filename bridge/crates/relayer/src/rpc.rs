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

    /// Estimated unix timestamp of the latest finalized slot
    /// (`getBlockTime` of the finalized slot). Used as the
    /// destination "now" for attestation expiry checks, mirroring
    /// the destination program's Clock. Fails closed: a chain that
    /// cannot report a block time for its finalized slot yields no
    /// timestamp, never a guessed one.
    pub fn finalized_timestamp(&self) -> Result<i64> {
        let slot = self.finalized_slot()?;
        let r = self.call("getBlockTime", json!([slot]))?;
        timestamp_from_block_time_result(&r)
    }

    /// Raw account data, or None if the account does not exist.
    /// Read at finalized commitment.
    pub fn account_data(&self, pubkey: &Pubkey) -> Result<Option<Vec<u8>>> {
        Ok(self.account_info(pubkey)?.map(|(_, data)| data))
    }

    /// Account owner and raw data, or None if the account does not
    /// exist. Read at finalized commitment. Callers that treat an
    /// account's EXISTENCE as meaningful (the receipt check in
    /// main.rs) must use this form and validate the owner and
    /// contents — existence alone proves nothing (debate
    /// scope-2r2, A-F1).
    pub fn account_info(&self, pubkey: &Pubkey) -> Result<Option<(Pubkey, Vec<u8>)>> {
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
        let data = B64
            .decode(b64)
            .map_err(|e| RelayerError::Rpc(format!("getAccountInfo: base64: {e}")))?;
        let owner_str = value["owner"]
            .as_str()
            .ok_or_else(|| RelayerError::Rpc("getAccountInfo: no owner".into()))?;
        let owner = Pubkey::from_str(owner_str)
            .map_err(|e| RelayerError::Rpc(format!("getAccountInfo: owner: {e}")))?;
        Ok(Some((owner, data)))
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

    /// All successful transaction signatures for an address that
    /// sit in `target_slot`, oldest first, at finalized commitment.
    /// For an event-record PDA these are the candidate creating
    /// transactions — the caller verifies the transfer inside each
    /// candidate and signs only for the one that created the
    /// record (see main.rs `enrich`).
    ///
    /// Pages BACKWARD through `getSignaturesForAddress` with the
    /// `before` cursor (debate scope-2r2, A-F2): the old
    /// oldest-in-a-single-1000-window heuristic permanently
    /// stalled a nonce when cheap pre-spam referencing the
    /// predictable record PDA (or >1,000 later signatures) pushed
    /// the creating transaction out of the window, and the hard
    /// slot-equality check then failed identically on every poll.
    /// Paging stops once a page's oldest entry is older than the
    /// target slot, the RPC returns a short page, or a sanity cap
    /// of 100 pages (100,000 signatures) is hit.
    pub fn signatures_at_slot(&self, pubkey: &Pubkey, target_slot: u64) -> Result<Vec<[u8; 64]>> {
        const PAGE: usize = 1000;
        const MAX_PAGES: usize = 100;
        let mut pages: Vec<Vec<[u8; 64]>> = Vec::new();
        let mut before: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut opts = json!({"limit": PAGE, "commitment": "finalized"});
            if let Some(b) = &before {
                opts["before"] = json!(b);
            }
            let r = self.call(
                "getSignaturesForAddress",
                json!([pubkey.to_string(), opts]),
            )?;
            let arr = r.as_array().ok_or_else(|| {
                RelayerError::Rpc("getSignaturesForAddress: not an array".into())
            })?;
            let (candidates, cursor, done) = signature_page(arr, target_slot);
            pages.push(candidates);
            if done || arr.len() < PAGE {
                break;
            }
            before = Some(cursor.ok_or_else(|| {
                RelayerError::Rpc("getSignaturesForAddress: page without a cursor".into())
            })?);
        }
        // Pages arrive newest-first and each page's candidates are
        // oldest-first within the page. Oldest-first overall means
        // reversing the PAGE order only — a whole-vector reverse
        // also inverts within each page (debate scope-2r3,
        // A-F5/B-F5: the old code returned newest-first on a
        // single page and a non-monotonic order across pages).
        let found = assemble_oldest_first(pages);
        if found.is_empty() {
            return Err(RelayerError::EventNotFound(format!(
                "no successful signature for {pubkey} at slot {target_slot}"
            )));
        }
        Ok(found)
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

/// Process one `getSignaturesForAddress` page (newest first) for
/// the paging search in `signatures_at_slot`. Returns:
/// - the successful signatures sitting exactly in `target_slot`,
///   oldest first within this page;
/// - the `before` cursor for the next (older) page: the oldest
///   entry's signature string;
/// - `done`: true when no older page can contain the target slot
///   (the page is empty, or its oldest entry is already older).
/// Pure over the RPC JSON so the selection is unit-testable.
pub fn signature_page(
    entries: &[Value],
    target_slot: u64,
) -> (Vec<[u8; 64]>, Option<String>, bool) {
    let mut candidates: Vec<[u8; 64]> = Vec::new();
    for e in entries.iter().rev() {
        if e["slot"].as_u64() != Some(target_slot) || !e["err"].is_null() {
            continue;
        }
        if let Some(bytes) = e["signature"]
            .as_str()
            .and_then(|s| bs58::decode(s).into_vec().ok())
            .and_then(|v| <[u8; 64]>::try_from(v).ok())
        {
            candidates.push(bytes);
        }
    }
    let cursor = entries
        .last()
        .and_then(|e| e["signature"].as_str().map(|s| s.to_string()));
    let done = match entries.last() {
        None => true,
        Some(oldest) => oldest["slot"].as_u64().map(|s| s < target_slot).unwrap_or(true),
    };
    (candidates, cursor, done)
}

/// Assemble per-page candidate lists (pages newest-first, each
/// page oldest-first internally) into one oldest-first list by
/// reversing the page order and keeping each page's internal
/// order. Pure so the ordering contract is unit-testable.
pub fn assemble_oldest_first(pages: Vec<Vec<[u8; 64]>>) -> Vec<[u8; 64]> {
    let mut out = Vec::new();
    for page in pages.into_iter().rev() {
        out.extend(page);
    }
    out
}

/// Extract the `blockTime` field from a `getTransaction` reply,
/// failing closed when the chain does not report one (war room
/// 2026-11-21, F-TEST-2): the A-F1 expiry derivation depends on
/// this value, so a missing or null block time is an error —
/// never a substituted local clock or zero.
pub fn block_time_from_transaction(tx: &Value) -> Result<i64> {
    tx.get("blockTime")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| {
            RelayerError::Rpc(
                "getTransaction: no blockTime for the source transaction; refusing to sign".into(),
            )
        })
}

/// Parse a `getBlockTime` result into a unix timestamp, failing
/// closed on a null or non-numeric result (war room 2026-11-21,
/// F-TEST-2).
pub fn timestamp_from_block_time_result(result: &Value) -> Result<i64> {
    result
        .as_i64()
        .ok_or_else(|| RelayerError::Rpc("getBlockTime: no timestamp".into()))
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
///
/// The scan is BOUND to the record being verified (self-audit L-1,
/// 2026-11-16): the transfer must sit in the inner-instruction
/// group of the one top-level instruction that created this record
/// — identified as the call to `source_program` whose account list
/// contains `record_pda`. The earlier whole-transaction scan let a
/// single genuine transfer satisfy the check for ANY number of
/// same-amount records created in the same batched transaction: one
/// transfer could verify two records. That is not reachable today —
/// a record cannot be created without its own transfer executing in
/// the same instruction (the programs' token CPIs are built by
/// spl-token's checked builders) — but the check now stands on its
/// own instead of borrowing that on-chain invariant: a record whose
/// creating instruction has no matching transfer in its own group
/// fails closed, and a batched transaction's records each resolve
/// to their own transfer's authority.
///
/// Pure function over the RPC JSON, so it is unit-testable with
/// fixtures and never guesses.
pub fn sender_from_verified_source_transfer(
    tx: &Value,
    kind: &str,
    check: &SourceTransferCheck,
    source_program: &Pubkey,
    record_pda: &Pubkey,
) -> Result<Pubkey> {
    // A failed transaction created nothing: refuse it here rather
    // than relying on the signature-list filter upstream.
    if tx.pointer("/meta/err").map(|e| !e.is_null()).unwrap_or(false) {
        return Err(RelayerError::SourceTransferNotVerified(
            "source transaction has a non-null meta.err; refusing to sign".into(),
        ));
    }
    // Locate the instruction that created this record.
    let top_level = tx
        .pointer("/transaction/message/instructions")
        .and_then(|v| v.as_array());
    let parent_index = top_level.and_then(|ixs| {
        ixs.iter().position(|ix| {
            ix.get("programId")
                .and_then(|v| v.as_str())
                .and_then(|s| Pubkey::from_str(s).ok())
                == Some(*source_program)
                && ix
                    .get("accounts")
                    .and_then(|v| v.as_array())
                    .map(|accounts| {
                        accounts
                            .iter()
                            .any(|a| a.as_str() == Some(record_pda.to_string().as_str()))
                    })
                    .unwrap_or(false)
        })
    });
    let Some(parent_index) = parent_index else {
        // This transaction did not create this record through the
        // source program — whatever transfers it holds are not this
        // record's backing. Fail closed.
        return Err(RelayerError::SourceTransferNotVerified(format!(
            "no instruction of {source_program} creating record {record_pda} in this transaction; refusing to sign"
        )));
    };
    // Only the creating instruction's own inner group can hold the
    // transfer that backs this record.
    let mut candidates: Vec<&Value> = Vec::new();
    if let Some(groups) = tx
        .pointer("/meta/innerInstructions")
        .and_then(|v| v.as_array())
    {
        for group in groups {
            if group.get("index").and_then(|v| v.as_u64()) == Some(parent_index as u64) {
                if let Some(ixs) = group.get("instructions").and_then(|v| v.as_array()) {
                    candidates.extend(ixs.iter());
                }
            }
        }
    }
    for ix in candidates {
        // The transfer must come from the SPL Token program itself,
        // not merely carry a token-shaped parsed payload.
        let is_token_program = ix["program"].as_str() == Some("spl-token")
            || ix["programId"].as_str()
                == Some("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
        if !is_token_program {
            continue;
        }
        let parsed = &ix["parsed"];
        if parsed["type"].as_str().unwrap_or("") != kind {
            continue;
        }
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
    // The creating instruction was found, but its own inner group
    // holds no transfer backing this record (none at all, a wrong
    // amount, a wrong destination/mint, or one without an
    // authority): the record is unverified. Fail closed.
    Err(RelayerError::SourceTransferNotVerified(format!(
        "no '{kind}' instruction in the creating instruction's group matches the event record (amount {}, destination {:?}, mint {:?}); refusing to sign",
        check.amount, check.destination, check.mint
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_page_selects_only_successful_at_target_slot() {
        // Debate scope-2r2 (A-F2): the page processor must pick
        // exactly the successful signatures in the record's slot,
        // oldest first, ignore spam in other slots and failed
        // transactions, and report when older pages cannot help.
        let sig = |b: u8| bs58::encode([b; 64]).into_string();
        let page = vec![
            json!({"signature": sig(9), "slot": 150, "err": null}),
            json!({"signature": sig(8), "slot": 100, "err": null}),
            json!({"signature": sig(7), "slot": 100, "err": {"InstructionError": [0, "Custom"]}}),
            json!({"signature": sig(6), "slot": 100, "err": null}),
            json!({"signature": sig(5), "slot": 90, "err": null}),
        ];
        let (cands, cursor, done) = signature_page(&page, 100);
        assert_eq!(cands, vec![[6u8; 64], [8u8; 64]]);
        assert_eq!(cursor, Some(sig(5)));
        assert!(done, "oldest entry is below the target slot");
        // A page entirely at/above the target slot is not done.
        let page2 = vec![json!({"signature": sig(3), "slot": 100, "err": null})];
        let (_, _, done2) = signature_page(&page2, 100);
        assert!(!done2);
        // Empty page: done, no cursor.
        let (c3, cur3, done3) = signature_page(&[], 100);
        assert!(c3.is_empty() && cur3.is_none() && done3);
    }

    #[test]
    fn debate_r3_assemble_oldest_first_single_and_multi_page() {
        // Debate scope-2r3 (A-F5/B-F5): single page stays
        // oldest-first (the old whole-vector reverse flipped it);
        // multi-page reverses page order only.
        let single = assemble_oldest_first(vec![vec![[6u8; 64], [8u8; 64]]]);
        assert_eq!(single, vec![[6u8; 64], [8u8; 64]]);
        // Pages arrive newest-first: page1 = newer [a_old, a_new],
        // page2 = older [b_old, b_new]; oldest-first overall is
        // [b_old, b_new, a_old, a_new].
        let multi = assemble_oldest_first(vec![
            vec![[3u8; 64], [4u8; 64]],
            vec![[1u8; 64], [2u8; 64]],
        ]);
        assert_eq!(multi, vec![[1u8; 64], [2u8; 64], [3u8; 64], [4u8; 64]]);
    }

    #[test]
    fn block_time_extraction_fails_closed() {
        // War room 2026-11-21 (F-TEST-2): both fail-closed block
        // time conversions are pinned — a present value parses;
        // null, missing, and wrong-typed values are errors, never
        // a substituted zero or local time.
        assert_eq!(
            block_time_from_transaction(&json!({"blockTime": 1_700_000_000})).unwrap(),
            1_700_000_000
        );
        assert!(block_time_from_transaction(&json!({"blockTime": null})).is_err());
        assert!(block_time_from_transaction(&json!({})).is_err());
        assert!(block_time_from_transaction(&json!({"blockTime": "1700000000"})).is_err());
        assert_eq!(
            timestamp_from_block_time_result(&json!(1_700_000_000)).unwrap(),
            1_700_000_000
        );
        assert!(timestamp_from_block_time_result(&json!(null)).is_err());
        assert!(timestamp_from_block_time_result(&json!("1700000000")).is_err());
    }

    /// A realistic deposit transaction: one top-level instruction
    /// (the lock program's Deposit, unparsed — it is not a known
    /// program; its account list carries the record PDA) and the
    /// token transfer as a CPI in `meta.innerInstructions`, in the
    /// group belonging to that instruction (index 0).
    fn deposit_tx(
        program: &Pubkey,
        record: &Pubkey,
        vault: &Pubkey,
        authority: &Pubkey,
        amount: &str,
    ) -> Value {
        json!({
            "transaction": {"message": {"instructions": [
                {"programId": program.to_string(),
                 "accounts": [Pubkey::new_unique().to_string(), record.to_string()],
                 "data": "deadbeef"}
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
    fn burn_tx(
        program: &Pubkey,
        record: &Pubkey,
        mint: &Pubkey,
        authority: &Pubkey,
        amount: &str,
    ) -> Value {
        json!({
            "transaction": {"message": {"instructions": [
                {"programId": program.to_string(),
                 "accounts": [Pubkey::new_unique().to_string(), record.to_string()],
                 "data": "deadbeef"}
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
        let program = Pubkey::new_unique();
        let record = Pubkey::new_unique();
        let vault = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let tx = deposit_tx(&program, &record, &vault, &user, "500");
        let sender = sender_from_verified_source_transfer(
            &tx,
            "transfer",
            &deposit_check(vault, 500),
            &program,
            &record,
        )
        .unwrap();
        assert_eq!(sender, user);
    }

    #[test]
    fn burn_sender_verified_by_mint_and_amount() {
        let program = Pubkey::new_unique();
        let record = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let burner = Pubkey::new_unique();
        let tx = burn_tx(&program, &record, &mint, &burner, "300");
        let sender = sender_from_verified_source_transfer(
            &tx,
            "burn",
            &burn_check(mint, 300),
            &program,
            &record,
        )
        .unwrap();
        assert_eq!(sender, burner);
    }

    #[test]
    fn phantom_deposit_rejected_coreum_case() {
        // The Coreum failure: a transaction that moves tokens between
        // two attacker wallets, with no transfer into the vault, must
        // never yield a sender — it is not a deposit.
        let program = Pubkey::new_unique();
        let record = Pubkey::new_unique();
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
        let err = sender_from_verified_source_transfer(
            &tx,
            "transfer",
            &deposit_check(vault, 500),
            &program,
            &record,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RelayerError::SourceTransferNotVerified(_)
        ));
    }

    #[test]
    fn inner_transfer_to_wrong_destination_rejected() {
        let program = Pubkey::new_unique();
        let record = Pubkey::new_unique();
        let vault = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let mut tx = deposit_tx(&program, &record, &Pubkey::new_unique(), &user, "500");
        // Same amount, but the CPI transfer went somewhere else.
        let err = sender_from_verified_source_transfer(
            &tx,
            "transfer",
            &deposit_check(vault, 500),
            &program,
            &record,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RelayerError::SourceTransferNotVerified(_)
        ));
        // And the right vault with the same tx must succeed, proving
        // the rejection is about the destination, not the shape.
        tx = deposit_tx(&program, &record, &vault, &user, "500");
        assert_eq!(
            sender_from_verified_source_transfer(
                &tx,
                "transfer",
                &deposit_check(vault, 500),
                &program,
                &record
            )
            .unwrap(),
            user
        );
    }

    #[test]
    fn wrong_amount_rejected() {
        let program = Pubkey::new_unique();
        let record = Pubkey::new_unique();
        let vault = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let tx = deposit_tx(&program, &record, &vault, &user, "499");
        let err = sender_from_verified_source_transfer(
            &tx,
            "transfer",
            &deposit_check(vault, 500),
            &program,
            &record,
        )
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
        // transfer that actually went into the vault. Inserting the
        // decoy shifts the Deposit to index 1, so its inner group
        // moves to index 1 with it (as real RPC output would show).
        let program = Pubkey::new_unique();
        let record = Pubkey::new_unique();
        let vault = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let decoy = Pubkey::new_unique();
        let mut tx = deposit_tx(&program, &record, &vault, &user, "500");
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
        tx["meta"]["innerInstructions"][0]["index"] = json!(1);
        let sender = sender_from_verified_source_transfer(
            &tx,
            "transfer",
            &deposit_check(vault, 500),
            &program,
            &record,
        )
        .unwrap();
        assert_eq!(sender, user);
        assert_ne!(sender, decoy);
    }

    #[test]
    fn burn_with_wrong_mint_rejected() {
        let program = Pubkey::new_unique();
        let record = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let burner = Pubkey::new_unique();
        let tx = burn_tx(&program, &record, &Pubkey::new_unique(), &burner, "300");
        let err = sender_from_verified_source_transfer(
            &tx,
            "burn",
            &burn_check(mint, 300),
            &program,
            &record,
        )
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
            &deposit_check(Pubkey::new_unique(), 500),
            &Pubkey::new_unique(),
            &Pubkey::new_unique()
        )
        .is_err());
    }
}
