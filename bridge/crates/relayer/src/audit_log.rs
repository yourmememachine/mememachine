//! Tamper-evident relayer audit log.
//!
//! The attestation exchange (see `attestation`) proves *what was
//! signed*: one immutable file per relayer per nonce. It does not
//! prove *the order in which this relayer acted* — that it signed
//! nonce 7 before it collected nonce 8, that a collection failed
//! with a disagreement at a particular point, or that a submission
//! happened at all. After an incident (a disputed crossing, a
//! Kelp-style manufactured attestation, an Alephium-style forced
//! fallback) that sequence is the evidence, and a plain log file
//! can be edited after the fact without a trace.
//!
//! This log is append-only and hash-chained. Each entry stores the
//! previous entry's hash and its own hash, computed as
//!
//! ```text
//! entry_hash = sha256("MACHINE-RELAYER-LOG-V1"
//!                     || prev_hash (32 bytes)
//!                     || seq (u64, little-endian)
//!                     || canonical JSON of the event)
//! ```
//!
//! Editing, deleting, or reordering any entry breaks every hash
//! after it, and `verify_chain` names the first broken entry.
//!
//! The honest limit, stated plainly (and pinned by a test): a hash
//! chain proves *internal consistency only*. Anyone with write
//! access to the file can recompute a fully forged chain that
//! `verify_chain` accepts. The chain becomes evidence only once
//! its head leaves the writer's control — so the `relayer-log`
//! binary prints a **checkpoint** (`seq` + head hash) meant to be
//! published somewhere the operator cannot rewrite (a release
//! note, another relayer's log, an auditor's inbox), and
//! `verify_checkpoint` checks a log against a previously published
//! checkpoint: a truncated, rolled-back, or wholly rewritten log
//! whose head differs is rejected even though it is internally
//! consistent. Verification against the *latest* head needs no
//! checkpoint — that is the truncation case the checkpoint exists
//! for.
//!
//! Fail-closed discipline, matching the collector: opening a log
//! whose existing contents do not verify is an error, never a
//! silent reset, and the relayer binary refuses to sign, collect,
//! or submit with a broken log — a human investigates first.

use crate::{RelayerError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Domain separator for entry hashes, so a relayer-log hash can
/// never be confused with a bridge message hash or any other
/// sha256 in the system.
pub const LOG_DOMAIN: &[u8] = b"MACHINE-RELAYER-LOG-V1";

/// The `prev_hash` of entry 0: 32 zero bytes.
pub const GENESIS_PREV: [u8; 32] = [0u8; 32];

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode_32(field: &str, s: &str) -> Result<[u8; 32]> {
    let bad = |reason: &str| {
        RelayerError::AuditLog(format!("{field}: {reason} (got '{s}')"))
    };
    if s.len() != 64 {
        return Err(bad("expected 64 hex characters (32 bytes)"));
    }
    // Decode from BYTES, never by slicing the string: a 64-byte
    // string containing multi-byte UTF-8 passes the length check
    // but a &s[2*i..2*i+2] slice can land inside a character and
    // panic — on untrusted on-disk data that turns the promised
    // fail-closed error into a crash (debate scope-2r2, A-F7;
    // same fix as bridge-common in scope-4).
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    for i in 0..32 {
        let hi = hex_val(bytes[2 * i]).ok_or_else(|| bad("not hex"))?;
        let lo = hex_val(bytes[2 * i + 1]).ok_or_else(|| bad("not hex"))?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

/// One thing a relayer did, in the order it did it. The event is
/// the payload the entry hash commits to; field order in the
/// serialized form is part of that commitment, so events are
/// structs/enums with a fixed declaration order, never maps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEvent {
    /// This relayer signed a message and wrote its attestation file.
    Signed {
        direction: String,
        nonce: u64,
        /// hex sha256 of the canonical bridge message.
        message_hash: String,
        /// This relayer's pubkey (base58).
        relayer: String,
    },
    /// A quorum was collected for a crossing (re-verified with
    /// bridge-common before this entry is written).
    Collected {
        direction: String,
        nonce: u64,
        message_hash: String,
        /// Distinct relayers in the collected quorum.
        signers: usize,
    },
    /// Collection failed for a non-routine reason (disagreement,
    /// malformed or forged exchange file, unknown relayer).
    /// Quorum-not-reached is routine in the poll loop and is
    /// deliberately not logged, so the log cannot be spammed by
    /// normal operation.
    CollectionFailed {
        direction: String,
        nonce: u64,
        reason: String,
    },
    /// A quorum attestation was submitted to the destination chain.
    Submitted {
        direction: String,
        nonce: u64,
        message_hash: String,
        /// Destination transaction signature (base58).
        tx_signature: String,
    },
    /// A submission was prepared but not sent (`submit_enabled`
    /// is false): the log records that the relayer *would* have
    /// submitted, so a dry-run leaves the same trail shape as a
    /// live run, minus the transaction.
    DryRun {
        direction: String,
        nonce: u64,
        message_hash: String,
    },
}

/// One line of the log file: an event plus its chain links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Position in the chain, starting at 0, strictly sequential.
    pub seq: u64,
    /// hex, 32 bytes: the previous entry's `entry_hash`
    /// (`GENESIS_PREV` for entry 0).
    pub prev_hash: String,
    pub event: AuditEvent,
    /// hex, 32 bytes: this entry's hash (see module docs).
    pub entry_hash: String,
}

impl LogEntry {
    fn compute_hash(seq: u64, prev_hash: &[u8; 32], event: &AuditEvent) -> Result<[u8; 32]> {
        let canonical = serde_json::to_vec(event)?;
        let mut h = Sha256::new();
        h.update(LOG_DOMAIN);
        h.update(prev_hash);
        h.update(seq.to_le_bytes());
        h.update(&canonical);
        Ok(h.finalize().into())
    }
}

/// Verify a chain of entries end to end: sequence numbers are
/// 0,1,2,…, every `prev_hash` links to the previous entry's hash,
/// and every `entry_hash` recomputes from its event. Returns the
/// head `(seq, hash)` of the verified prefix. An empty chain is
/// valid and has no head.
pub fn verify_chain(entries: &[LogEntry]) -> Result<Option<(u64, String)>> {
    let mut prev = GENESIS_PREV;
    for (i, entry) in entries.iter().enumerate() {
        if entry.seq != i as u64 {
            return Err(RelayerError::AuditLog(format!(
                "entry {i}: seq is {}, expected {i} (gap, deletion, or reorder)",
                entry.seq
            )));
        }
        let claimed_prev = hex_decode_32(&format!("entry {i} prev_hash"), &entry.prev_hash)?;
        if claimed_prev != prev {
            return Err(RelayerError::AuditLog(format!(
                "entry {i}: prev_hash does not match entry {}'s hash (chain broken)",
                i.wrapping_sub(1)
            )));
        }
        let recomputed = LogEntry::compute_hash(entry.seq, &prev, &entry.event)?;
        let claimed = hex_decode_32(&format!("entry {i} entry_hash"), &entry.entry_hash)?;
        if claimed != recomputed {
            return Err(RelayerError::AuditLog(format!(
                "entry {i}: entry_hash does not recompute from its event (contents edited)"
            )));
        }
        prev = recomputed;
    }
    Ok(entries
        .last()
        .map(|e| (e.seq, e.entry_hash.clone())))
}

/// Verify a chain *and* that it still contains a previously
/// published checkpoint: the entry at `checkpoint_seq` must carry
/// exactly `checkpoint_hash`. This is the check that catches what
/// `verify_chain` cannot — truncation or rollback past the
/// checkpoint, and a wholesale rewrite (a forged chain recomputes
/// cleanly but cannot reproduce a head it never had, short of
/// reproducing every original event byte-for-byte, at which point
/// it is the original log).
pub fn verify_checkpoint(
    entries: &[LogEntry],
    checkpoint_seq: u64,
    checkpoint_hash: &str,
) -> Result<Option<(u64, String)>> {
    let head = verify_chain(entries)?;
    let at = entries.get(checkpoint_seq as usize).ok_or_else(|| {
        RelayerError::AuditLog(format!(
            "log has {} entries but the checkpoint is at seq {checkpoint_seq} (truncated or rolled back)",
            entries.len()
        ))
    })?;
    if at.entry_hash != checkpoint_hash {
        return Err(RelayerError::AuditLog(format!(
            "entry at checkpoint seq {checkpoint_seq} has hash {}, checkpoint says {checkpoint_hash} (rewritten history)",
            at.entry_hash
        )));
    }
    Ok(head)
}

/// Read and parse a log file (JSON Lines, one `LogEntry` per line).
/// Parsing only — call `verify_chain` on the result; `AuditLog::open`
/// does both.
pub fn read_entries(path: &Path) -> Result<Vec<LogEntry>> {
    // Stream line by line instead of reading the whole log into a
    // String first (debate scope-2r2, B-F6): the log grows with
    // every event and is re-read on every open, so peak memory
    // must not hold the file text AND the parsed entries at once.
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut entries = Vec::new();
    for (n, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: LogEntry = serde_json::from_str(&line).map_err(|e| {
            RelayerError::AuditLog(format!("line {}: unparseable entry: {e}", n + 1))
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

/// An opened audit log: its existing contents were verified on
/// open (fail closed), and `append` extends the chain and the file
/// together — the entry is written and flushed before the in-memory
/// chain advances, so a crash cannot leave memory ahead of disk.
pub struct AuditLog {
    path: PathBuf,
    entries: Vec<LogEntry>,
}

impl AuditLog {
    /// Open (or create) the log at `path`. A missing file is an
    /// empty log; an existing file that fails to parse or verify
    /// is an error — the log is never silently reset.
    pub fn open(path: &Path) -> Result<Self> {
        let entries = if path.exists() {
            let entries = read_entries(path)?;
            verify_chain(&entries)?;
            entries
        } else {
            Vec::new()
        };
        Ok(Self {
            path: path.to_path_buf(),
            entries,
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }

    /// The current checkpoint: `(seq, entry_hash)` of the last
    /// entry, or `None` for an empty log. Publish this value
    /// outside the operator's control to make the chain evidence
    /// (see module docs).
    pub fn head(&self) -> Option<(u64, String)> {
        self.entries
            .last()
            .map(|e| (e.seq, e.entry_hash.clone()))
    }

    /// Append one event: compute its chain links, write the line,
    /// then advance. Returns the stored entry.
    pub fn append(&mut self, event: AuditEvent) -> Result<LogEntry> {
        let seq = self.entries.len() as u64;
        let prev = match self.entries.last() {
            Some(last) => hex_decode_32("last entry_hash", &last.entry_hash)?,
            None => GENESIS_PREV,
        };
        let hash = LogEntry::compute_hash(seq, &prev, &event)?;
        let entry = LogEntry {
            seq,
            prev_hash: hex_encode(&prev),
            event,
            entry_hash: hex_encode(&hash),
        };
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&entry)?)?;
        file.flush()?;
        // flush() only reaches the OS page cache; sync so the entry
        // survives a host crash / power loss, as the docs claim.
        file.sync_all()?;
        self.entries.push(entry.clone());
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(nonce: u64) -> AuditEvent {
        AuditEvent::Signed {
            direction: "solana-to-machine".to_string(),
            nonce,
            message_hash: "ab".repeat(32),
            relayer: "Relayer1111111111111111111111111111111111".to_string(),
        }
    }

    fn fresh_path(name: &str) -> PathBuf {
        // Unique per call: tests run in parallel threads in one
        // process, so name + pid alone would collide.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "audit-log-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("relayer-audit.jsonl")
    }

    fn log_with(n: u64) -> (PathBuf, AuditLog) {
        let path = fresh_path("build");
        let mut log = AuditLog::open(&path).unwrap();
        for nonce in 0..n {
            log.append(signed(nonce)).unwrap();
        }
        (path, log)
    }

    #[test]
    fn appended_entries_form_a_verifiable_chain() {
        let (_path, log) = log_with(4);
        assert_eq!(log.len(), 4);
        let head = verify_chain(log.entries()).unwrap().unwrap();
        assert_eq!(head, log.head().unwrap());
        assert_eq!(head.0, 3);
        // Entry 0 hangs off the genesis prev.
        assert_eq!(log.entries()[0].prev_hash, hex_encode(&GENESIS_PREV));
    }

    #[test]
    fn reopening_continues_the_same_chain() {
        let (path, log) = log_with(2);
        let head_before = log.head().unwrap();
        drop(log);
        let mut reopened = AuditLog::open(&path).unwrap();
        assert_eq!(reopened.head().unwrap(), head_before);
        let third = reopened.append(signed(2)).unwrap();
        assert_eq!(third.seq, 2);
        assert_eq!(third.prev_hash, head_before.1);
        verify_chain(reopened.entries()).unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn edited_event_is_detected() {
        let (_path, log) = log_with(3);
        let mut entries = log.entries().to_vec();
        // Rewrite history: nonce 1 was "really" for ten times the amount —
        // here, a different nonce in the signed event.
        entries[1].event = signed(99);
        let err = verify_chain(&entries).unwrap_err().to_string();
        assert!(err.contains("entry 1"), "unexpected error: {err}");
    }

    #[test]
    fn deleted_entry_is_detected() {
        let (_path, log) = log_with(3);
        let mut entries = log.entries().to_vec();
        entries.remove(1);
        assert!(verify_chain(&entries).is_err());
    }

    #[test]
    fn reordered_entries_are_detected() {
        let (_path, log) = log_with(3);
        let mut entries = log.entries().to_vec();
        entries.swap(0, 1);
        assert!(verify_chain(&entries).is_err());
    }

    #[test]
    fn forged_entry_hash_is_detected() {
        let (_path, log) = log_with(2);
        let mut entries = log.entries().to_vec();
        entries[1].entry_hash = "ff".repeat(32);
        assert!(verify_chain(&entries).is_err());
    }

    #[test]
    fn truncation_passes_internal_verify_but_fails_the_checkpoint() {
        // The documented limit, pinned: a log cut back to a prefix
        // is internally consistent — only the published checkpoint
        // (taken at the full length) exposes the rollback.
        let (_path, log) = log_with(4);
        let (cp_seq, cp_hash) = log.head().unwrap();
        let truncated = log.entries()[..2].to_vec();
        verify_chain(&truncated).unwrap();
        let err = verify_checkpoint(&truncated, cp_seq, &cp_hash)
            .unwrap_err()
            .to_string();
        assert!(err.contains("truncated"), "unexpected error: {err}");
    }

    #[test]
    fn wholesale_rewrite_passes_internal_verify_but_fails_the_checkpoint() {
        // The attacker recomputes a brand-new chain over different
        // events. It verifies cleanly on its own — and still fails
        // against the checkpoint published from the real log.
        let (_path, log) = log_with(3);
        let (cp_seq, cp_hash) = log.head().unwrap();

        let mut forged: Vec<LogEntry> = Vec::new();
        let mut prev = GENESIS_PREV;
        for nonce in 0..3u64 {
            let event = signed(1_000 + nonce);
            let hash = LogEntry::compute_hash(nonce, &prev, &event).unwrap();
            forged.push(LogEntry {
                seq: nonce,
                prev_hash: hex_encode(&prev),
                event,
                entry_hash: hex_encode(&hash),
            });
            prev = hash;
        }
        verify_chain(&forged).unwrap();
        let err = verify_checkpoint(&forged, cp_seq, &cp_hash)
            .unwrap_err()
            .to_string();
        assert!(err.contains("rewritten"), "unexpected error: {err}");
    }

    #[test]
    fn checkpoint_at_a_prefix_still_verifies_after_appends() {
        // Checkpoints pin a prefix, not the whole future: a log
        // that grew past its published checkpoint still verifies
        // against it (the pinned prefix is unchanged).
        let (_path, mut log) = log_with(2);
        let (cp_seq, cp_hash) = log.head().unwrap();
        log.append(signed(2)).unwrap();
        log.append(signed(3)).unwrap();
        let head = verify_checkpoint(log.entries(), cp_seq, &cp_hash)
            .unwrap()
            .unwrap();
        assert_eq!(head.0, 3);
    }

    #[test]
    fn open_fails_closed_on_a_tampered_file() {
        let (path, log) = log_with(2);
        drop(log);
        // Edit the file behind the log's back, then reopen.
        let mut entries = read_entries(&path).unwrap();
        entries[0].event = signed(42);
        let text: String = entries
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n")
            .collect();
        std::fs::write(&path, text).unwrap();
        assert!(AuditLog::open(&path).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn open_fails_closed_on_a_corrupt_line() {
        let path = fresh_path("corrupt");
        std::fs::write(&path, "{\"seq\": torn write\n").unwrap();
        assert!(AuditLog::open(&path).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn non_ascii_hash_is_an_error_not_a_panic() {
        // Debate scope-2r2 (A-F7): a crafted 64-BYTE hash string
        // whose bytes are not ASCII ("a" + "€" + 60*"a" = 64 bytes,
        // 62 chars) must fail closed with an AuditLog error.
        // Pre-fix, hex_decode_32 sliced &s[2..4] inside the €
        // character and panicked.
        let bad_hash = format!("a€{}", "a".repeat(60));
        assert_eq!(bad_hash.len(), 64);
        let entry = LogEntry {
            seq: 0,
            prev_hash: bad_hash,
            event: signed(0),
            entry_hash: "ab".repeat(32),
        };
        assert!(verify_chain(&[entry]).is_err());
    }

    #[test]
    fn empty_log_has_no_head_and_verifies() {
        let path = fresh_path("empty");
        let log = AuditLog::open(&path).unwrap();
        assert!(log.is_empty());
        assert_eq!(log.head(), None);
        assert_eq!(verify_chain(log.entries()).unwrap(), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn distinct_event_kinds_chain_together() {
        let path = fresh_path("kinds");
        let mut log = AuditLog::open(&path).unwrap();
        log.append(signed(7)).unwrap();
        log.append(AuditEvent::Collected {
            direction: "solana-to-machine".to_string(),
            nonce: 7,
            message_hash: "ab".repeat(32),
            signers: 3,
        })
        .unwrap();
        log.append(AuditEvent::Submitted {
            direction: "solana-to-machine".to_string(),
            nonce: 7,
            message_hash: "ab".repeat(32),
            tx_signature: "TxSig111111111111111111111111111111111111".to_string(),
        })
        .unwrap();
        log.append(AuditEvent::CollectionFailed {
            direction: "machine-to-solana".to_string(),
            nonce: 3,
            reason: "relayer disagreement".to_string(),
        })
        .unwrap();
        verify_chain(log.entries()).unwrap();
        // And the file on disk parses back to the same entries.
        let from_disk = read_entries(&path).unwrap();
        assert_eq!(from_disk, log.entries());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
