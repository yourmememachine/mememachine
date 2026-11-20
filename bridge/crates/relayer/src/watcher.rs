//! Watching a source chain for the next bridge event.
//!
//! Discovery is by nonce cursor, not by scanning accounts: the programs
//! accept any unused caller-chosen nonce, this watcher probes them
//! sequentially per direction, and the event for nonce N lives
//! at a deterministic PDA. The watcher probes N, and only advances
//! past N once that event has been signed (or was already processed
//! on the destination). It never skips a nonce — a gap stalls the
//! bridge, which is the safe failure mode (BRIDGE.md 3.2).
//!
//! The chain access sits behind the `SourceChain` trait so the
//! polling/finality logic is testable with a mock, with no network.

use crate::message::{build_message, check_finalized, Direction, ObservedEvent};
use crate::{RelayerError, Result};
use bridge_common::{nonce_pda, BridgeMessage, EventRecord};
use serde::{Deserialize, Serialize};
use solana_program::pubkey::Pubkey;

/// Read-only view of one chain, as the watcher needs it.
pub trait SourceChain {
    /// Program that records events in this direction (lock program
    /// for Solana->Machine, bridge program for Machine->Solana).
    fn event_program_id(&self) -> Pubkey;
    /// Latest finalized/rooted slot.
    fn finalized_slot(&self) -> Result<u64>;
    /// The event record at its nonce PDA, if it exists yet.
    fn event_record(&self, record_pda: &Pubkey) -> Result<Option<EventRecord>>;
    /// Enrich a record with its sender and source transaction.
    fn enrich(&self, record: &EventRecord) -> Result<ObservedEvent>;
}

/// Persistent next-nonce cursors (one per direction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayerState {
    pub solana_to_machine_next_nonce: u64,
    pub machine_to_solana_next_nonce: u64,
}

impl RelayerState {
    pub fn new(start_nonce: u64) -> Self {
        Self {
            solana_to_machine_next_nonce: start_nonce,
            machine_to_solana_next_nonce: start_nonce,
        }
    }
    pub fn load(path: &str, start_nonce: u64) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let state: Self = serde_json::from_str(&text)?;
                // A cursor below the configured start nonce means the
                // file predates this deployment or was tampered with;
                // refuse rather than silently re-walk or skip.
                if state.solana_to_machine_next_nonce < start_nonce
                    || state.machine_to_solana_next_nonce < start_nonce
                {
                    return Err(RelayerError::InvalidConfig(format!(
                        "state file {path} has a cursor below start_nonce {start_nonce}"
                    )));
                }
                Ok(state)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new(start_nonce)),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, path: &str) -> Result<()> {
        let tmp = format!("{path}.tmp");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                crate::attestation::sync_dir(parent);
            }
        }
        Ok(())
    }
    pub fn next_nonce(&self, direction: Direction) -> u64 {
        match direction {
            Direction::SolanaToMachine => self.solana_to_machine_next_nonce,
            Direction::MachineToSolana => self.machine_to_solana_next_nonce,
        }
    }
    pub fn advance(&mut self, direction: Direction) {
        match direction {
            Direction::SolanaToMachine => self.solana_to_machine_next_nonce += 1,
            Direction::MachineToSolana => self.machine_to_solana_next_nonce += 1,
        }
    }
}

/// What one poll of one direction found.
#[derive(Debug, PartialEq, Eq)]
pub enum PollOutcome {
    /// No event recorded at the next nonce yet.
    NoEvent { nonce: u64 },
    /// An event exists but its slot is not finalized (+ margin) yet.
    NotFinalized { nonce: u64 },
    /// A finalized event, with the canonical message to sign.
    Ready { message: BridgeMessage },
}

/// Probe the next nonce in `direction` once.
pub fn poll_once(
    chain: &dyn SourceChain,
    direction: Direction,
    next_nonce: u64,
    source_mint: Pubkey,
    expiry_window_slots: u64,
    safety_margin_slots: u64,
) -> Result<PollOutcome> {
    let (record_pda, _) = nonce_pda(&chain.event_program_id(), direction.nonce_dir(), next_nonce);
    let record = match chain.event_record(&record_pda)? {
        None => return Ok(PollOutcome::NoEvent { nonce: next_nonce }),
        Some(r) => r,
    };
    if record.nonce != next_nonce {
        return Err(RelayerError::InvalidConfig(format!(
            "event record at {record_pda} has nonce {}, expected {next_nonce}",
            record.nonce
        )));
    }
    let finalized = chain.finalized_slot()?;
    if check_finalized(record.slot, finalized, safety_margin_slots).is_err() {
        return Ok(PollOutcome::NotFinalized { nonce: next_nonce });
    }
    let event = chain.enrich(&record)?;
    // The finality gate above checked THIS record; the message must
    // be built from exactly that record, never from whatever an
    // enrich implementation substitutes for it.
    if event.record != record {
        return Err(RelayerError::InvalidConfig(
            "enrich returned a record different from the one that passed finality".into(),
        ));
    }
    let message = build_message(direction, &event, source_mint, expiry_window_slots)?;
    Ok(PollOutcome::Ready { message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_common::DISCRIMINATOR_EVENT_RECORD;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct MockChain {
        program: Pubkey,
        finalized: u64,
        records: RefCell<HashMap<Pubkey, EventRecord>>,
        sender: Pubkey,
    }

    impl SourceChain for MockChain {
        fn event_program_id(&self) -> Pubkey {
            self.program
        }
        fn finalized_slot(&self) -> Result<u64> {
            Ok(self.finalized)
        }
        fn event_record(&self, pda: &Pubkey) -> Result<Option<EventRecord>> {
            Ok(self.records.borrow().get(pda).cloned())
        }
        fn enrich(&self, record: &EventRecord) -> Result<ObservedEvent> {
            Ok(ObservedEvent {
                record: record.clone(),
                sender: self.sender,
                source_tx: [8u8; 64],
            })
        }
    }

    fn record(program: Pubkey, nonce: u64, slot: u64) -> (Pubkey, EventRecord) {
        let (pda, _) = nonce_pda(&program, DIR_FOR(nonce), nonce);
        (
            pda,
            EventRecord {
                discriminator: DISCRIMINATOR_EVENT_RECORD,
                nonce,
                amount: 500,
                destination: Pubkey::new_unique(),
                slot,
            },
        )
    }
    // Deposits live under DIR_SOLANA_TO_MACHINE in these tests.
    fn DIR_FOR(_nonce: u64) -> u8 {
        bridge_common::DIR_SOLANA_TO_MACHINE
    }

    #[test]
    fn no_event_then_not_finalized_then_ready() {
        let program = Pubkey::new_unique();
        let mut chain = MockChain {
            program,
            finalized: 1_000,
            records: RefCell::new(HashMap::new()),
            sender: Pubkey::new_unique(),
        };
        let mint = Pubkey::new_unique();
        // Nothing at nonce 3 yet.
        assert_eq!(
            poll_once(&chain, Direction::SolanaToMachine, 3, mint, 100_000, 16).unwrap(),
            PollOutcome::NoEvent { nonce: 3 }
        );
        // Event at slot 990; finalized 1000 is only 10 past it (< 16 margin).
        let (pda, rec) = record(program, 3, 990);
        chain.records.borrow_mut().insert(pda, rec);
        assert_eq!(
            poll_once(&chain, Direction::SolanaToMachine, 3, mint, 100_000, 16).unwrap(),
            PollOutcome::NotFinalized { nonce: 3 }
        );
        // Finality advances past the margin: ready, with a built message.
        chain.finalized = 1_006;
        match poll_once(&chain, Direction::SolanaToMachine, 3, mint, 100_000, 16).unwrap() {
            PollOutcome::Ready { message } => {
                assert_eq!(message.nonce, 3);
                assert_eq!(message.amount, 500);
                assert_eq!(message.expiry_slot, 990 + 100_000);
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn poll_once_rejects_enrich_returning_a_different_record() {
        struct DivergingChain(MockChain);
        impl SourceChain for DivergingChain {
            fn event_program_id(&self) -> Pubkey { self.0.event_program_id() }
            fn finalized_slot(&self) -> Result<u64> { self.0.finalized_slot() }
            fn event_record(&self, pda: &Pubkey) -> Result<Option<EventRecord>> { self.0.event_record(pda) }
            fn enrich(&self, record: &EventRecord) -> Result<ObservedEvent> {
                let mut ev = self.0.enrich(record)?;
                ev.record.slot += 500; // a record that never passed the finality gate
                Ok(ev)
            }
        }
        let program = Pubkey::new_unique();
        let chain = MockChain { program, finalized: 2_000, records: RefCell::new(HashMap::new()), sender: Pubkey::new_unique() };
        let (pda, rec) = record(program, 3, 990);
        chain.records.borrow_mut().insert(pda, rec);
        let diverging = DivergingChain(chain);
        assert!(poll_once(&diverging, Direction::SolanaToMachine, 3, Pubkey::new_unique(), 100_000, 16).is_err());
    }

    #[test]
    fn state_load_rejects_cursor_below_start_nonce() {
        let path = std::env::temp_dir().join(format!("relayer-state-low-{}.json", std::process::id())).to_string_lossy().to_string();
        RelayerState::new(1).save(&path).unwrap();
        assert!(RelayerState::load(&path, 5).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn state_cursors_are_per_direction_and_persist() {
        let mut state = RelayerState::new(1);
        state.advance(Direction::SolanaToMachine);
        assert_eq!(state.next_nonce(Direction::SolanaToMachine), 2);
        assert_eq!(state.next_nonce(Direction::MachineToSolana), 1);
        let path = std::env::temp_dir()
            .join(format!("relayer-state-{}.json", std::process::id()))
            .to_string_lossy()
            .to_string();
        state.save(&path).unwrap();
        let loaded = RelayerState::load(&path, 1).unwrap();
        assert_eq!(loaded.solana_to_machine_next_nonce, 2);
        let _ = std::fs::remove_file(&path);
    }
}
