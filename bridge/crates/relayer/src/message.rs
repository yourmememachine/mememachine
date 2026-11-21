//! Building the canonical `BridgeMessage` for a source-chain event,
//! and the finality rule that gates signing (BRIDGE.md 2.3, 2.5).

use crate::{RelayerError, Result};
use bridge_common::{
    BridgeMessage, EventRecord, CHAIN_MACHINE, CHAIN_SOLANA, DIR_MACHINE_TO_SOLANA,
    DIR_SOLANA_TO_MACHINE, MAX_ATTESTATION_LIFETIME_SECS, MAX_SOURCE_TIMESTAMP_SKEW_SECS,
    MESSAGE_VERSION,
};
use solana_program::pubkey::Pubkey;

/// A source-chain event, enriched with the fields the on-chain
/// `EventRecord` does not store: who sent it, in which transaction,
/// and that transaction's block time. All are read from the source
/// transaction by the watcher (see rpc.rs) — they are chain data,
/// not relayer opinion, so every honest relayer derives the same
/// values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedEvent {
    pub record: EventRecord,
    pub sender: Pubkey,
    /// Raw 64-byte source transaction signature.
    pub source_tx: [u8; 64],
    /// The source transaction's block time (unix seconds), from the
    /// `getTransaction` response. The message's expiry is derived
    /// from it, so `enrich` must fail closed when the chain does not
    /// report one — never substitute a local clock or zero.
    pub source_block_time: i64,
}

/// Direction of a crossing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    SolanaToMachine,
    MachineToSolana,
}

impl Direction {
    pub fn source_chain(&self) -> u8 {
        match self {
            Direction::SolanaToMachine => CHAIN_SOLANA,
            Direction::MachineToSolana => CHAIN_MACHINE,
        }
    }
    pub fn dest_chain(&self) -> u8 {
        match self {
            Direction::SolanaToMachine => CHAIN_MACHINE,
            Direction::MachineToSolana => CHAIN_SOLANA,
        }
    }
    /// Direction byte used in nonce PDA seeds (bridge-common).
    pub fn nonce_dir(&self) -> u8 {
        match self {
            Direction::SolanaToMachine => DIR_SOLANA_TO_MACHINE,
            Direction::MachineToSolana => DIR_MACHINE_TO_SOLANA,
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "solana-to-machine" | "s2m" => Ok(Direction::SolanaToMachine),
            "machine-to-solana" | "m2s" => Ok(Direction::MachineToSolana),
            other => Err(RelayerError::InvalidConfig(format!(
                "unknown direction '{other}' (use solana-to-machine or machine-to-solana)"
            ))),
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Direction::SolanaToMachine => "solana-to-machine",
            Direction::MachineToSolana => "machine-to-solana",
        }
    }
}

/// BRIDGE.md 2.5 finality gate. `finalized_slot` must come from an RPC
/// query at finalized commitment (Solana) / the rooted slot (Machine).
/// Signing is allowed only when the event slot is at or before
/// `finalized_slot - safety_margin_slots`.
pub fn check_finalized(
    source_slot: u64,
    finalized_slot: u64,
    safety_margin_slots: u64,
) -> Result<()> {
    let required = source_slot.saturating_add(safety_margin_slots);
    if finalized_slot < required {
        return Err(RelayerError::NotFinalized {
            source_slot,
            finalized_slot,
            safety_margin_slots,
        });
    }
    Ok(())
}

/// Build the message every relayer must sign for `event`.
///
/// `source_mint` is the original SPL mint on Solana in *both*
/// directions: for a burn, it is the mint of the original token whose
/// wrapped representation was burned — the lock program checks the
/// destination token account against its configured `source_mint`,
/// and BRIDGE.md 2.3 defines the field that way.
///
/// `expiry_window_secs` must be the same for all relayers (see lib.rs).
pub fn build_message(
    direction: Direction,
    event: &ObservedEvent,
    source_mint: Pubkey,
    expiry_window_secs: i64,
) -> Result<BridgeMessage> {
    if event.record.amount == 0 {
        return Err(RelayerError::InvalidConfig(
            "event amount is zero; refusing to build a message".into(),
        ));
    }
    if expiry_window_secs <= 0 {
        return Err(RelayerError::InvalidConfig(
            "expiry_window_secs must be > 0".into(),
        ));
    }
    // The cap also lives here, not only in config validation (war
    // room 2026-11-21, F-OPS-6): a caller that bypasses
    // RelayerConfig::validate must not be able to build a message
    // validate_message will reject forever.
    if expiry_window_secs > MAX_ATTESTATION_LIFETIME_SECS {
        return Err(RelayerError::InvalidConfig(
            "expiry_window_secs exceeds MAX_ATTESTATION_LIFETIME_SECS".into(),
        ));
    }
    if event.source_block_time <= 0 {
        return Err(RelayerError::InvalidConfig(
            "source block time must be > 0; refusing to build a message".into(),
        ));
    }
    // Expiry is timestamp-based (A-F1 fix, 2026-11-20): both fields
    // come from the source transaction's own block time, which
    // every relayer reads from the same finalized transaction, so
    // all relayers derive the identical expiry and sign the
    // identical message.
    let expiry_timestamp = event
        .source_block_time
        .checked_add(expiry_window_secs)
        .ok_or_else(|| RelayerError::InvalidConfig("expiry timestamp overflow".into()))?;
    Ok(BridgeMessage {
        version: MESSAGE_VERSION,
        source_chain: direction.source_chain(),
        dest_chain: direction.dest_chain(),
        nonce: event.record.nonce,
        source_mint,
        sender: event.sender,
        recipient: event.record.destination,
        amount: event.record.amount,
        source_tx: event.source_tx,
        source_slot: event.record.slot,
        expiry_timestamp,
        source_timestamp: event.source_block_time,
    })
}

/// Plausibility gate on a source block time read from an RPC
/// response, checked against the relayer's own wall clock (war
/// room 2026-11-21, F-ADV-1/F-OPS-6). The signed timestamps are
/// otherwise only the RPC's assertion: nothing on either chain
/// binds them to the source block, so a shared or compromised RPC
/// serving a far-future block time would otherwise produce a
/// unanimous quorum over a post-dated message that becomes
/// executable when its window arrives. A block time in the past
/// is always plausible (the relayer may be catching up); a block
/// time ahead of the relayer's own clock by more than the
/// cross-chain skew allowance is not. This never substitutes the
/// local clock for the value — it only refuses to sign an
/// implausible one, fail-closed and attributable.
pub fn check_source_block_time(source_block_time: i64, now: i64) -> Result<()> {
    if source_block_time <= 0 {
        return Err(RelayerError::Rpc(format!(
            "source block time {source_block_time} is not a plausible unix timestamp; refusing to sign"
        )));
    }
    match source_block_time.checked_sub(now) {
        Some(ahead) if ahead <= MAX_SOURCE_TIMESTAMP_SKEW_SECS => Ok(()),
        _ => Err(RelayerError::Rpc(format!(
            "source block time {source_block_time} is more than {MAX_SOURCE_TIMESTAMP_SKEW_SECS}s ahead of the local clock ({now}); refusing to sign"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_common::DISCRIMINATOR_EVENT_RECORD;

    fn event(slot: u64, amount: u64) -> ObservedEvent {
        ObservedEvent {
            record: EventRecord {
                discriminator: DISCRIMINATOR_EVENT_RECORD,
                nonce: 7,
                amount,
                destination: Pubkey::new_unique(),
                slot,
            },
            sender: Pubkey::new_unique(),
            source_tx: [9u8; 64],
            source_block_time: 1_700_000_000,
        }
    }

    #[test]
    fn build_message_rejects_over_cap_window_and_bad_block_time() {
        // War room 2026-11-21 (F-OPS-6/F-TEST-5): the lifetime cap
        // is enforced in build_message itself, and a non-positive
        // source block time can never produce a signable message.
        let mint = Pubkey::new_unique();
        let e = event(500, 1_000);
        assert!(build_message(Direction::SolanaToMachine, &e, mint, 400_000).is_ok());
        assert!(build_message(Direction::SolanaToMachine, &e, mint, 400_001).is_err());
        let mut e = event(500, 1_000);
        e.source_block_time = 0;
        assert!(build_message(Direction::SolanaToMachine, &e, mint, 40_000).is_err());
        e.source_block_time = -1;
        assert!(build_message(Direction::SolanaToMachine, &e, mint, 40_000).is_err());
    }

    #[test]
    fn source_block_time_plausibility_gate() {
        // War room 2026-11-21 (F-ADV-1): past and within-skew block
        // times pass; non-positive and far-future ones are refused.
        let now = 1_700_000_000;
        assert!(check_source_block_time(now - 86_400, now).is_ok());
        assert!(check_source_block_time(now + MAX_SOURCE_TIMESTAMP_SKEW_SECS, now).is_ok());
        assert!(check_source_block_time(now + MAX_SOURCE_TIMESTAMP_SKEW_SECS + 1, now).is_err());
        assert!(check_source_block_time(now + 30 * 86_400, now).is_err());
        assert!(check_source_block_time(0, now).is_err());
        assert!(check_source_block_time(-1, now).is_err());
    }

    #[test]
    fn finality_margin_enforced() {
        // Solana rule: finalized + 16.
        assert!(check_finalized(100, 116, 16).is_ok());
        assert!(matches!(
            check_finalized(100, 115, 16),
            Err(RelayerError::NotFinalized { .. })
        ));
        // Machine rule: rooted, no extra margin.
        assert!(check_finalized(100, 100, 0).is_ok());
        assert!(check_finalized(100, 99, 0).is_err());
    }

    #[test]
    fn expiry_is_deterministic_from_source_block_time() {
        let mint = Pubkey::new_unique();
        let e = event(500, 1_000);
        let m1 = build_message(Direction::SolanaToMachine, &e, mint, 40_000).unwrap();
        let m2 = build_message(Direction::SolanaToMachine, &e, mint, 40_000).unwrap();
        assert_eq!(m1, m2);
        assert_eq!(m1.hash(), m2.hash());
        assert_eq!(m1.source_timestamp, 1_700_000_000);
        assert_eq!(m1.expiry_timestamp, 1_700_040_000);
        // A different window produces a different hash — which is why
        // all relayers must share this config value.
        let m3 = build_message(Direction::SolanaToMachine, &e, mint, 80_000).unwrap();
        assert_ne!(m1.hash(), m3.hash());
    }

    #[test]
    fn directions_are_bound() {
        let mint = Pubkey::new_unique();
        let e = event(500, 1_000);
        let s2m = build_message(Direction::SolanaToMachine, &e, mint, 40_000).unwrap();
        assert_eq!((s2m.source_chain, s2m.dest_chain), (CHAIN_SOLANA, CHAIN_MACHINE));
        let m2s = build_message(Direction::MachineToSolana, &e, mint, 40_000).unwrap();
        assert_eq!((m2s.source_chain, m2s.dest_chain), (CHAIN_MACHINE, CHAIN_SOLANA));
        assert_ne!(s2m.hash(), m2s.hash());
    }

    #[test]
    fn zero_amount_refused() {
        let mint = Pubkey::new_unique();
        assert!(build_message(Direction::SolanaToMachine, &event(1, 0), mint, 100).is_err());
    }
}
