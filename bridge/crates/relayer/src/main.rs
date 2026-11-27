//! Relayer binary. See lib.rs for the design and BRIDGE.md sections
//! 2.5 / 3 for the rules it implements.
//!
//! Usage:
//!   relayer --config relayer.json sign-once
//!   relayer --config relayer.json collect --direction solana-to-machine --nonce 7
//!   relayer --config relayer.json submit  --direction solana-to-machine --nonce 7 \
//!       --destination-token-account <pubkey>
//!   relayer --config relayer.json run --destination-token-account <pubkey>
//!
//! `submit` and `run` are dry-run unless the config sets
//! `"submit_enabled": true`. The destination token account must be
//! owned by the message recipient and hold the right mint — the
//! destination program enforces both, and rejects otherwise.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use bridge_common::{nonce_pda, EventRecord};
use relayer::attestation::{hash_hex, AttestationFile, Collector};
use relayer::audit_log::{AuditEvent, AuditLog};
use relayer::config::{parse_pubkey, RelayerConfig};
use relayer::message::{check_source_block_time, Direction, ObservedEvent};
use relayer::rpc::{
    block_time_from_transaction, sender_from_verified_source_transfer, RpcClient,
    SourceTransferCheck,
};
use relayer::signing::RelayerSigner;
use relayer::submit::{build_mint_instructions, build_unlock_instructions};
use relayer::watcher::{poll_once, PollOutcome, RelayerState, SourceChain};
use relayer::{RelayerError, Result};
use solana_program::pubkey::Pubkey;
use std::path::Path;
use std::str::FromStr;

struct RpcSource {
    client: RpcClient,
    program_id: Pubkey,
    direction: Direction,
    /// Deposit direction: the lock program's vault token account —
    /// the source transfer must land here (rpc.rs).
    vault: Option<Pubkey>,
    /// Burn direction: the wrapped mint that must be burned.
    wrapped_mint: Option<Pubkey>,
}

impl SourceChain for RpcSource {
    fn event_program_id(&self) -> Pubkey {
        self.program_id
    }
    fn finalized_slot(&self) -> Result<u64> {
        self.client.finalized_slot()
    }
    fn event_record(&self, record_pda: &Pubkey) -> Result<Option<EventRecord>> {
        self.client.event_record(record_pda)
    }
    fn enrich(&self, record: &EventRecord) -> Result<ObservedEvent> {
        let (record_pda, _) = nonce_pda(&self.program_id, self.direction.nonce_dir(), record.nonce);
        // Candidates: every successful transaction in the record's
        // OWN slot that references the record PDA (paged, so spam
        // referencing the predictable PDA cannot push the creating
        // transaction out of reach — debate scope-2r2, A-F2). Try
        // each candidate: only the one whose inner instructions
        // actually contain this record's transfer verifies, and
        // that verification is what binds the message to the
        // record. If no candidate verifies, fail closed.
        let candidates = self.client.signatures_at_slot(&record_pda, record.slot)?;
        let mut last_err: Option<RelayerError> = None;
        for source_tx in &candidates {
            match self.enrich_candidate(record, source_tx) {
                Ok(ev) => return Ok(ev),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            RelayerError::SourceTransferNotVerified(format!(
                "no candidate transaction at slot {} created record {record_pda}",
                record.slot
            ))
        }))
    }
}

impl RpcSource {
    fn enrich_candidate(
        &self,
        record: &EventRecord,
        source_tx: &[u8; 64],
    ) -> Result<ObservedEvent> {
        let (record_pda, _) = nonce_pda(&self.program_id, self.direction.nonce_dir(), record.nonce);
        let tx = self.client.transaction_parsed(source_tx)?;
        // The message's expiry is derived from this transaction's
        // block time (A-F1 fix, 2026-11-20), which every relayer
        // reads from the same finalized transaction. If the chain
        // does not report one, fail closed — never substitute a
        // local clock or zero, which would either split the quorum
        // or forge an expiry. The value is also plausibility-checked
        // against the local clock (war room 2026-11-21, F-ADV-1):
        // the local clock never supplies the value, it only vetoes
        // an implausible one.
        let source_block_time = block_time_from_transaction(&tx)?;
        let local_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        check_source_block_time(source_block_time, local_now)?;
        // The sender is proven, not asserted: the source transaction
        // must contain the transfer behind this record — into the
        // vault for a deposit, burning the wrapped mint for a burn —
        // for exactly the record's amount. Anything else fails
        // closed (see rpc.rs; Coreum, Aug 2026).
        let (kind, check) = match self.direction {
            Direction::SolanaToMachine => (
                "transfer",
                SourceTransferCheck {
                    destination: self.vault,
                    mint: None,
                    amount: record.amount,
                },
            ),
            Direction::MachineToSolana => (
                "burn",
                SourceTransferCheck {
                    destination: None,
                    mint: self.wrapped_mint,
                    amount: record.amount,
                },
            ),
        };
        let sender = sender_from_verified_source_transfer(
            &tx,
            kind,
            &check,
            &self.program_id,
            &record_pda,
        )?;
        Ok(ObservedEvent {
            record: record.clone(),
            sender,
            source_tx: *source_tx,
            source_block_time,
        })
    }
}

/// Append one event to the tamper-evident audit log. Opening the
/// log verifies its existing chain, so a tampered log fails every
/// action closed — the relayer does not sign, collect, or submit
/// without its evidence trail (see audit_log.rs).
fn log_event(cfg: &RelayerConfig, event: AuditEvent) -> Result<()> {
    let mut log = AuditLog::open(Path::new(&cfg.audit_log_path()))?;
    log.append(event)?;
    Ok(())
}

/// Audit events this process has already written, for the `run`
/// loop's dedupe (debate scope-2r2, A-F4/B-F2): the loop rescans
/// every nonce from `start_nonce` on every poll, and a nonce whose
/// submission never lands (dry-run — the default — or a permanent
/// collection failure) used to re-append the identical Collected /
/// DryRun / CollectionFailed entry on EVERY poll, each append
/// re-reading and re-verifying the whole log: unbounded growth
/// and quadratic slowdown with no attacker involved. Within one
/// process an identical event is written once; a changed event
/// (new hash, new failure reason) is written again. One-shot
/// commands use a fresh set, so their behaviour is unchanged.
#[derive(Default)]
struct LoggedEvents(std::collections::HashSet<String>);

impl LoggedEvents {
    /// True the first time this exact event is seen in this
    /// process, false for repeats.
    fn mark(&mut self, event: &AuditEvent) -> bool {
        self.0.insert(format!("{event:?}"))
    }

    fn log(&mut self, cfg: &RelayerConfig, event: AuditEvent) -> Result<()> {
        if self.mark(&event) {
            log_event(cfg, event)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dry_run(nonce: u64) -> AuditEvent {
        AuditEvent::DryRun {
            direction: "solana-to-machine".to_string(),
            nonce,
            message_hash: "ab".repeat(32),
        }
    }

    #[test]
    fn debate_r3_derived_destination_is_recipient_ata() {
        // Debate scope-2r3 (B-F2): the derived account is the
        // recipient's ATA for the mint, and differs per recipient.
        let mint = Pubkey::new_unique();
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        let da = derived_destination_token_account(&a, &mint);
        assert_eq!(da, derived_destination_token_account(&a, &mint));
        assert_ne!(da, derived_destination_token_account(&b, &mint));
        let token_program =
            Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();
        let ata_program =
            Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap();
        let expected = Pubkey::find_program_address(
            &[a.as_ref(), token_program.as_ref(), mint.as_ref()],
            &ata_program,
        )
        .0;
        assert_eq!(da, expected);
    }

    #[test]
    fn logged_events_dedupes_identical_events_only() {
        // Debate scope-2r2 (A-F4/B-F2): the same event repeated by
        // the run loop's rescan is marked once; a different nonce
        // or a different failure reason is a new event.
        let mut logged = LoggedEvents::default();
        assert!(logged.mark(&dry_run(7)));
        assert!(!logged.mark(&dry_run(7)));
        assert!(logged.mark(&dry_run(8)));
        let fail = |reason: &str| AuditEvent::CollectionFailed {
            direction: "solana-to-machine".to_string(),
            nonce: 7,
            reason: reason.to_string(),
        };
        assert!(logged.mark(&fail("disagreement")));
        assert!(!logged.mark(&fail("disagreement")));
        assert!(logged.mark(&fail("quorum changed")));
    }
}

/// Verify the audit log's existing chain WITHOUT appending. This
/// is the gate the log's invariant promises: it runs BEFORE an
/// attestation is published or a transaction is sent, so a tampered
/// log actually fails the action closed instead of being discovered
/// after the irreversible step.
fn verify_audit_log(cfg: &RelayerConfig) -> Result<()> {
    AuditLog::open(Path::new(&cfg.audit_log_path()))?;
    Ok(())
}

fn source_for(cfg: &RelayerConfig, direction: Direction) -> Result<RpcSource> {
    use borsh::BorshDeserialize;
    match direction {
        Direction::SolanaToMachine => {
            let client = RpcClient::new(&cfg.solana_rpc_url);
            let program_id = cfg.lock_program_id();
            let (config_pda, _) = bridge_common::config_pda(&program_id);
            let data = client.account_data(&config_pda)?.ok_or_else(|| {
                RelayerError::Rpc(format!("no lock config account at {config_pda}"))
            })?;
            let config =
                bridge_common::LockConfig::try_from_slice(&data).map_err(RelayerError::Borsh)?;
            if config.discriminator != bridge_common::DISCRIMINATOR_CONFIG {
                return Err(RelayerError::Rpc(format!(
                    "lock config at {config_pda} has wrong discriminator"
                )));
            }
            if config.source_mint != cfg.source_mint() {
                return Err(RelayerError::Rpc(format!(
                    "on-chain source_mint {} does not match config file {}",
                    config.source_mint,
                    cfg.source_mint()
                )));
            }
            // Debate scope-2r3 (A-F4/B-F3): cfg.vault() builds
            // every Unlock instruction; cross-check it too.
            if config.vault != cfg.vault() {
                return Err(RelayerError::Rpc(format!(
                    "on-chain vault {} does not match config file {}",
                    config.vault,
                    cfg.vault()
                )));
            }
            Ok(RpcSource {
                client,
                program_id,
                direction,
                vault: Some(config.vault),
                wrapped_mint: None,
            })
        }
        Direction::MachineToSolana => {
            let client = RpcClient::new(&cfg.machine_rpc_url);
            let program_id = cfg.machine_program_id();
            let (config_pda, _) = bridge_common::config_pda(&program_id);
            let data = client.account_data(&config_pda)?.ok_or_else(|| {
                RelayerError::Rpc(format!("no bridge config account at {config_pda}"))
            })?;
            let config = bridge_common::MachineConfig::try_from_slice(&data)
                .map_err(RelayerError::Borsh)?;
            if config.discriminator != bridge_common::DISCRIMINATOR_CONFIG {
                return Err(RelayerError::Rpc(format!(
                    "bridge config at {config_pda} has wrong discriminator"
                )));
            }
            if config.wrapped_mint != cfg.wrapped_mint() {
                return Err(RelayerError::Rpc(format!(
                    "on-chain wrapped_mint {} does not match config file {}",
                    config.wrapped_mint,
                    cfg.wrapped_mint()
                )));
            }
            // Debate round 2026-11-18 (A-1): the Machine config now
            // registers the source mint; cross-check it the same way
            // the lock side does, so a relayer configured for a
            // different asset fails at startup, not at mint time.
            if config.source_mint != cfg.source_mint() {
                return Err(RelayerError::Rpc(format!(
                    "on-chain source_mint {} does not match config file {}",
                    config.source_mint,
                    cfg.source_mint()
                )));
            }
            Ok(RpcSource {
                client,
                program_id,
                direction,
                vault: None,
                wrapped_mint: Some(config.wrapped_mint),
            })
        }
    }
}

fn dest_client(cfg: &RelayerConfig, direction: Direction) -> RpcClient {
    match direction {
        // Destination of a Solana->Machine crossing is Machine.
        Direction::SolanaToMachine => RpcClient::new(&cfg.machine_rpc_url),
        Direction::MachineToSolana => RpcClient::new(&cfg.solana_rpc_url),
    }
}

fn margin(cfg: &RelayerConfig, direction: Direction) -> u64 {
    match direction {
        Direction::SolanaToMachine => cfg.solana_safety_margin_slots,
        Direction::MachineToSolana => cfg.machine_safety_margin_slots,
    }
}

/// Poll one direction once; if a finalized event is ready, sign it
/// and write the attestation file. Returns true if a file was written.
fn sign_once(
    cfg: &RelayerConfig,
    signer: &RelayerSigner,
    state: &mut RelayerState,
    direction: Direction,
) -> Result<bool> {
    let source = source_for(cfg, direction)?;
    let nonce = state.next_nonce(direction);
    // The message always carries the original Solana mint.
    match poll_once(
        &source,
        direction,
        nonce,
        cfg.source_mint(),
        cfg.expiry_window_secs,
        margin(cfg, direction),
    )? {
        PollOutcome::NoEvent { .. } => {
            println!("{}: no event at nonce {}", direction.label(), nonce);
            Ok(false)
        }
        PollOutcome::NotFinalized { .. } => {
            println!(
                "{}: event at nonce {} is not finalized yet",
                direction.label(),
                nonce
            );
            Ok(false)
        }
        PollOutcome::Ready { message } => {
            // Gate BEFORE publishing: a tampered audit log must stop
            // the signature from reaching the shared directory.
            verify_audit_log(cfg)?;
            let sig = signer.sign(&message);
            // Debate scope-2r3 (A-F1): append the Signed entry
            // BEFORE publishing — every field is known in advance,
            // and the old order left a usable attestation in the
            // shared directory with no evidence trail when the
            // append failed (disk full, fs error). If publishing
            // then fails, the next poll re-signs deterministically
            // and the duplicate Signed entry is the honest record
            // of a second signing attempt.
            log_event(
                cfg,
                AuditEvent::Signed {
                    direction: direction.label().to_string(),
                    nonce: message.nonce,
                    message_hash: hash_hex(&message),
                    relayer: signer.pubkey().to_string(),
                },
            )?;
            let path = AttestationFile::new(direction, &message, &sig)
                .write_to(Path::new(&cfg.attestation_dir))?;
            println!(
                "{}: signed nonce {} amount {} -> {}",
                direction.label(),
                message.nonce,
                message.amount,
                path.display()
            );
            state.advance(direction);
            state.save(&cfg.state_path)?;
            Ok(true)
        }
    }
}

/// The registered set/quorum WITHOUT the paused/killed gate
/// (debate scope-2r3, B-F1): signing must not be blocked by a
/// destination pause — the kill switch stops minting only and
/// burns (the exit path) stay open by design, so a restarted
/// relayer must still be able to sign burn attestations while
/// the Machine bridge is killed. Paused/killed gates only
/// collection/submission, per direction, in
/// `registered_relayers`.
fn registered_relayers_set(cfg: &RelayerConfig, direction: Direction) -> Result<([Pubkey; 5], u8)> {
    registered_relayers_inner(cfg, direction, false)
}

fn registered_relayers(cfg: &RelayerConfig, direction: Direction) -> Result<([Pubkey; 5], u8)> {
    registered_relayers_inner(cfg, direction, true)
}

fn registered_relayers_inner(
    cfg: &RelayerConfig,
    direction: Direction,
    enforce_active: bool,
) -> Result<([Pubkey; 5], u8)> {
    // Read the destination program's config: its relayer set is the
    // set that will verify the attestation on-chain, so collect
    // against exactly that set. (Both programs are initialized with
    // the same set; the destination's is the one that matters here.)
    let (dest_program, client) = match direction {
        Direction::SolanaToMachine => (cfg.machine_program_id(), RpcClient::new(&cfg.machine_rpc_url)),
        Direction::MachineToSolana => (cfg.lock_program_id(), RpcClient::new(&cfg.solana_rpc_url)),
    };
    let (config_pda, _) = bridge_common::config_pda(&dest_program);
    let data = client
        .account_data(&config_pda)?
        .ok_or_else(|| RelayerError::Rpc(format!("no config account at {config_pda}")))?;
    use borsh::BorshDeserialize;
    match direction {
        Direction::SolanaToMachine => {
            let c = bridge_common::MachineConfig::try_from_slice(&data)
                .map_err(RelayerError::Borsh)?;
            if c.discriminator != bridge_common::DISCRIMINATOR_CONFIG {
                return Err(RelayerError::Rpc("destination config has wrong discriminator".into()));
            }
            if enforce_active && (c.paused || c.killed) {
                return Err(RelayerError::Rpc(
                    "destination bridge is paused or killed on-chain".into(),
                ));
            }
            // Use the ON-CHAIN quorum, not the compiled-in constant:
            // governance can change it, and the destination program
            // will enforce the on-chain value.
            Ok((c.relayers.relayers, c.relayers.quorum))
        }
        Direction::MachineToSolana => {
            let c = bridge_common::LockConfig::try_from_slice(&data)
                .map_err(RelayerError::Borsh)?;
            if c.discriminator != bridge_common::DISCRIMINATOR_CONFIG {
                return Err(RelayerError::Rpc("destination config has wrong discriminator".into()));
            }
            if enforce_active && c.paused {
                return Err(RelayerError::Rpc(
                    "destination bridge is paused on-chain".into(),
                ));
            }
            // Debate scope-2r3 (A-F4/B-F3): the config file's vault
            // builds every Unlock instruction, so cross-check it
            // against the on-chain LockConfig here (collection path)
            // as well as in source_for (signing path) — a stale
            // vault must fail at startup/collection, not as an
            // unexplained on-chain rejection on every poll.
            if c.vault != cfg.vault() {
                return Err(RelayerError::Rpc(format!(
                    "on-chain vault {} does not match config file {}",
                    c.vault,
                    cfg.vault()
                )));
            }
            Ok((c.relayers.relayers, c.relayers.quorum))
        }
    }
}

/// Fail at startup if this signer is not in the destination's
/// registered relayer set: an unregistered key would sign forever
/// and every peer's collector would discard its files.
fn check_signer_registered(cfg: &RelayerConfig, signer: &RelayerSigner) -> Result<()> {
    for direction in [Direction::SolanaToMachine, Direction::MachineToSolana] {
        // Set-only read (debate scope-2r3, B-F1): a paused/killed
        // destination must not block signing for either direction.
        let (relayers, _) = registered_relayers_set(cfg, direction)?;
        if !relayers.contains(&signer.pubkey()) {
            return Err(RelayerError::InvalidConfig(format!(
                "signer {} is not in the registered relayer set for {}",
                signer.pubkey(),
                direction.label()
            )));
        }
    }
    Ok(())
}

fn collect(
    cfg: &RelayerConfig,
    direction: Direction,
    nonce: u64,
    logged: &mut LoggedEvents,
) -> Result<(bridge_common::BridgeMessage, Vec<bridge_common::RelayerSignature>)> {
    let (relayers, quorum) = registered_relayers(cfg, direction)?;
    let collector = Collector { relayers, quorum };
    let dest_now = dest_client(cfg, direction).finalized_timestamp()?;
    match collector.collect(Path::new(&cfg.attestation_dir), direction, nonce, dest_now) {
        Ok((message, signatures)) => {
            logged.log(
                cfg,
                AuditEvent::Collected {
                    direction: direction.label().to_string(),
                    nonce,
                    message_hash: hash_hex(&message),
                    signers: signatures.len(),
                },
            )?;
            Ok((message, signatures))
        }
        Err(e) => {
            // Non-routine failures are dispute evidence (disagreement,
            // a malformed or forged exchange file): record them.
            // QuorumNotReached is routine in the poll loop and is not
            // logged. Recording is best-effort here — the collection
            // error is the one the caller must see.
            if !matches!(e, RelayerError::QuorumNotReached { .. }) {
                let _ = logged.log(
                    cfg,
                    AuditEvent::CollectionFailed {
                        direction: direction.label().to_string(),
                        nonce,
                        reason: e.to_string(),
                    },
                );
            }
            Err(e)
        }
    }
}

/// Derive the recipient's associated token account — the
/// deterministic destination for a crossing when the operator did
/// not name an explicit account (debate scope-2r3, B-F2).
fn derived_destination_token_account(recipient: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();
    let ata_program = Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap();
    Pubkey::find_program_address(
        &[recipient.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    )
    .0
}

fn submit(
    cfg: &RelayerConfig,
    direction: Direction,
    nonce: u64,
    destination_token_account: Option<Pubkey>,
    logged: &mut LoggedEvents,
) -> Result<()> {
    // Already processed on the destination? A GENUINE receipt —
    // owned by the destination program and parsing as its
    // CrossingReceipt for this direction+nonce — is the on-chain
    // proof. Mere existence is not (debate scope-2r2, A-F1):
    // anyone can pre-fund the predictable receipt PDA with an
    // empty account, and the programs adopt such pristine shells
    // when writing the real receipt, so an existence-only check
    // permanently suppresses an unprocessed crossing.
    {
        let (dest_program, dir_u8) = match direction {
            Direction::SolanaToMachine => (
                cfg.machine_program_id(),
                bridge_common::DIR_SOLANA_TO_MACHINE,
            ),
            Direction::MachineToSolana => (
                cfg.lock_program_id(),
                bridge_common::DIR_MACHINE_TO_SOLANA,
            ),
        };
        let (receipt, _) = bridge_common::receipt_pda(&dest_program, dir_u8, nonce);
        if let Some((owner, data)) = dest_client(cfg, direction).account_info(&receipt)? {
            if relayer::submit::receipt_is_genuine(&owner, &data, &dest_program, dir_u8, nonce) {
                println!(
                    "{}: nonce {} already processed on the destination (receipt exists); skipping",
                    direction.label(),
                    nonce
                );
                return Ok(());
            }
        }
    }
    let (message, signatures) = collect(cfg, direction, nonce, logged)?;
    // Debate scope-2r3 (B-F2): with no explicit account (the `run`
    // daemon's per-nonce path), derive the recipient's associated
    // token account from the message itself — one configured
    // account for the whole daemon could only ever serve one
    // recipient and stranded every other crossing forever.
    let expected_mint_for_dest = match direction {
        Direction::SolanaToMachine => cfg.wrapped_mint(),
        Direction::MachineToSolana => cfg.source_mint(),
    };
    let destination_token_account = destination_token_account.unwrap_or_else(|| {
        derived_destination_token_account(&message.recipient, &expected_mint_for_dest)
    });
    let destination_token_account = &destination_token_account;
    // The destination token account must belong to the message
    // recipient and hold the expected mint. The destination program
    // enforces both, but checking here turns a guaranteed on-chain
    // rejection (and a wrong-recipient run-loop configuration) into
    // a clear local error before any transaction is built.
    {
        let data = dest_client(cfg, direction)
            .account_data(destination_token_account)?
            .ok_or_else(|| {
                RelayerError::Rpc(format!(
                    "no destination token account at {destination_token_account}"
                ))
            })?;
        if data.len() < 64 {
            return Err(RelayerError::Rpc(
                "destination token account data too short".into(),
            ));
        }
        let mint = Pubkey::try_from(&data[0..32]).unwrap();
        let owner = Pubkey::try_from(&data[32..64]).unwrap();
        let expected_mint = match direction {
            Direction::SolanaToMachine => cfg.wrapped_mint(),
            Direction::MachineToSolana => cfg.source_mint(),
        };
        if mint != expected_mint || owner != message.recipient {
            return Err(RelayerError::Rpc(format!(
                "destination token account {destination_token_account} has mint {mint} / owner {owner}, expected mint {expected_mint} / recipient {}",
                message.recipient
            )));
        }
    }
    println!(
        "{}: quorum for nonce {} ({} signatures), amount {} to {}",
        direction.label(),
        nonce,
        signatures.len(),
        message.amount,
        message.recipient
    );

    // The payer keypair is loaded through solana-sdk, whose keypair
    // file format is the same JSON array the relayer signer uses.
    // It gets the same 0600 permission refusal as the relayer key
    // (debate scope-2r2, A-F5/B-F4) — it holds the fee float. In
    // dry-run mode a MISSING payer file must not block the report:
    // nothing is signed or sent, so a placeholder payer stands in
    // for instruction building and the run says so.
    relayer::signing::check_keypair_permissions(&cfg.payer_keypair_path).or_else(|e| {
        if !cfg.submit_enabled && !Path::new(&cfg.payer_keypair_path).exists() {
            Ok(())
        } else {
            Err(e)
        }
    })?;
    let payer = match solana_sdk::signature::read_keypair_file(&cfg.payer_keypair_path) {
        Ok(p) => Some(p),
        Err(e) if !cfg.submit_enabled => {
            eprintln!(
                "warning: payer keypair {} unreadable ({e}); dry-run continues with a placeholder payer",
                cfg.payer_keypair_path
            );
            None
        }
        Err(e) => {
            return Err(RelayerError::InvalidKeypair(format!("payer: {e}")));
        }
    };
    use solana_sdk::signer::Signer as _;
    let payer_pubkey = payer
        .as_ref()
        .map(|p| p.pubkey())
        .unwrap_or_else(|| solana_sdk::pubkey::Pubkey::new_from_array([0u8; 32]));
    // Convert the sdk pubkey to a solana-program pubkey via string
    // form (they are the same bytes; the crates are distinct re-exports
    // in SDK 2.x and string conversion is unambiguous).
    let payer_pk = Pubkey::from_str(&payer_pubkey.to_string())
        .map_err(|e| RelayerError::InvalidPubkey(e.to_string()))?;

    let ixs = match direction {
        Direction::SolanaToMachine => build_mint_instructions(
            &cfg.machine_program_id(),
            &message,
            signatures,
            &cfg.wrapped_mint(),
            destination_token_account,
            &payer_pk,
        ),
        Direction::MachineToSolana => build_unlock_instructions(
            &cfg.lock_program_id(),
            &message,
            signatures,
            &cfg.vault(),
            destination_token_account,
            &payer_pk,
        ),
    };

    if !cfg.submit_enabled {
        println!(
            "dry-run (submit_enabled=false): would submit {} as {} instructions (ed25519 precompile + bridge) to program {}",
            direction.label(),
            ixs.len(),
            ixs[1].program_id
        );
        logged.log(
            cfg,
            AuditEvent::DryRun {
                direction: direction.label().to_string(),
                nonce,
                message_hash: hash_hex(&message),
            },
        )?;
        return Ok(());
    }

    let client = dest_client(cfg, direction);
    let blockhash_str = client.latest_blockhash()?;
    let blockhash = solana_sdk::hash::Hash::from_str(&blockhash_str)
        .map_err(|e| RelayerError::Rpc(format!("blockhash: {e}")))?;
    // solana_program::instruction::Instruction and the sdk's are the
    // same solana-instruction type; convert through the sdk constructor
    // fields to be explicit about it.
    let sdk_ixs: Vec<solana_sdk::instruction::Instruction> = ixs
        .iter()
        .map(|ix| solana_sdk::instruction::Instruction {
            program_id: solana_sdk::pubkey::Pubkey::from_str(&ix.program_id.to_string()).unwrap(),
            accounts: ix
                .accounts
                .iter()
                .map(|a| solana_sdk::instruction::AccountMeta {
                    pubkey: solana_sdk::pubkey::Pubkey::from_str(&a.pubkey.to_string()).unwrap(),
                    is_signer: a.is_signer,
                    is_writable: a.is_writable,
                })
                .collect(),
            data: ix.data.clone(),
        })
        .collect();
    // Gate BEFORE broadcasting: a tampered audit log must stop the
    // transaction from being sent, not be noticed afterwards.
    verify_audit_log(cfg)?;
    // Reaching here means submit_enabled is true, which means the
    // payer load above succeeded (a missing/unreadable payer is a
    // hard error in live mode, a placeholder only in dry-run).
    let payer = payer.expect("live submission implies a loaded payer");
    let tx = solana_sdk::transaction::Transaction::new_signed_with_payer(
        &sdk_ixs,
        Some(&payer_pubkey),
        &[&payer],
        blockhash,
    );
    let wire = bincode::serialize(&tx)
        .map_err(|e| RelayerError::Rpc(format!("transaction serialize: {e}")))?;
    let sig = client.send_transaction(&B64.encode(wire))?;
    println!("submitted: {sig}");
    log_event(
        cfg,
        AuditEvent::Submitted {
            direction: direction.label().to_string(),
            nonce,
            message_hash: hash_hex(&message),
            tx_signature: sig,
        },
    )?;
    Ok(())
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("relayer: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = arg_value(&args, "--config").ok_or_else(|| {
        RelayerError::InvalidConfig("usage: relayer --config <file> <sign-once|collect|submit|run> [options]".into())
    })?;
    let cfg = RelayerConfig::load(&config_path)?;
    let command = args
        .iter()
        .find(|a| matches!(a.as_str(), "sign-once" | "collect" | "submit" | "run"))
        .ok_or_else(|| RelayerError::InvalidConfig("missing command: sign-once | collect | submit | run".into()))?
        .clone();

    match command.as_str() {
        "sign-once" => {
            let signer = RelayerSigner::load(&cfg.relayer_keypair_path)?;
            check_signer_registered(&cfg, &signer)?;
            let mut state = RelayerState::load(&cfg.state_path, cfg.start_nonce)?;
            for direction in [Direction::SolanaToMachine, Direction::MachineToSolana] {
                sign_once(&cfg, &signer, &mut state, direction)?;
            }
            Ok(())
        }
        "collect" => {
            let direction = Direction::parse(
                &arg_value(&args, "--direction")
                    .ok_or_else(|| RelayerError::InvalidConfig("--direction required".into()))?,
            )?;
            let nonce: u64 = arg_value(&args, "--nonce")
                .ok_or_else(|| RelayerError::InvalidConfig("--nonce required".into()))?
                .parse()
                .map_err(|_| RelayerError::InvalidConfig("--nonce must be a number".into()))?;
            let (message, sigs) = collect(&cfg, direction, nonce, &mut LoggedEvents::default())?;
            println!(
                "quorum reached for {} nonce {}: {} signatures, message hash {}",
                direction.label(),
                nonce,
                sigs.len(),
                relayer::attestation::hash_hex(&message)
            );
            Ok(())
        }
        "submit" => {
            let direction = Direction::parse(
                &arg_value(&args, "--direction")
                    .ok_or_else(|| RelayerError::InvalidConfig("--direction required".into()))?,
            )?;
            let nonce: u64 = arg_value(&args, "--nonce")
                .ok_or_else(|| RelayerError::InvalidConfig("--nonce required".into()))?
                .parse()
                .map_err(|_| RelayerError::InvalidConfig("--nonce must be a number".into()))?;
            let dest = parse_pubkey(
                "destination-token-account",
                &arg_value(&args, "--destination-token-account").ok_or_else(|| {
                    RelayerError::InvalidConfig("--destination-token-account required".into())
                })?,
            )?;
            submit(&cfg, direction, nonce, Some(dest), &mut LoggedEvents::default())
        }
        "run" => {
            let signer = RelayerSigner::load(&cfg.relayer_keypair_path)?;
            check_signer_registered(&cfg, &signer)?;
            let mut state = RelayerState::load(&cfg.state_path, cfg.start_nonce)?;
            // Validate the destination account ONCE at startup: a
            // typo must fail loudly here, not silently disable every
            // submission for the life of the daemon.
            let dest = match arg_value(&args, "--destination-token-account") {
                Some(s) => Some(parse_pubkey("destination-token-account", &s)?),
                None => None,
            };
            println!(
                "relayer {} running (submit_enabled={})",
                signer.pubkey(),
                cfg.submit_enabled
            );
            let mut logged = LoggedEvents::default();
            // Debate scope-2r3 (B-F4): nonces this process has
            // already seen submit() succeed for (genuine receipt,
            // submitted, or dry-run at quorum) are not re-scanned —
            // the old rescan re-paid a receipt RPC for every
            // historical nonce on every poll, unbounded as history
            // grows. Failed nonces (quorum not yet reached) are
            // NOT cached and are retried next poll.
            let mut processed: std::collections::HashSet<(String, u64)> =
                std::collections::HashSet::new();
            loop {
                for direction in [Direction::SolanaToMachine, Direction::MachineToSolana] {
                    if let Err(e) = sign_once(&cfg, &signer, &mut state, direction) {
                        eprintln!("{}: sign: {e}", direction.label());
                    }
                    // Submit EVERY signed-but-unprocessed nonce in
                    // this direction, not only the most recent one:
                    // a quorum that forms late for an earlier nonce
                    // must still be submitted. With no explicit
                    // --destination-token-account, submit derives
                    // each recipient's associated token account
                    // from the message (debate scope-2r3, B-F2);
                    // an explicit account remains an operator
                    // override for non-ATA recipients.
                    for nonce in cfg.start_nonce..state.next_nonce(direction) {
                        let key = (direction.label().to_string(), nonce);
                        if processed.contains(&key) {
                            continue;
                        }
                        match submit(&cfg, direction, nonce, dest, &mut logged) {
                            Ok(()) => {
                                processed.insert(key);
                            }
                            Err(e) => {
                                // QuorumNotReached is normal in the
                                // loop; report and continue.
                                eprintln!("{}: submit nonce {}: {e}", direction.label(), nonce);
                            }
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(cfg.poll_interval_secs));
            }
        }
        _ => unreachable!(),
    }
}
