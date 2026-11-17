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
use relayer::message::{Direction, ObservedEvent};
use relayer::rpc::{sender_from_verified_source_transfer, RpcClient, SourceTransferCheck};
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
        let (source_tx, _slot) = self.client.first_signature_for_address(&record_pda)?;
        let tx = self.client.transaction_parsed(&source_tx)?;
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
            source_tx,
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
        cfg.expiry_window_slots,
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
            let sig = signer.sign(&message);
            let path = AttestationFile::new(direction, &message, &sig)
                .write_to(Path::new(&cfg.attestation_dir))?;
            // Log before advancing the cursor: if the log cannot be
            // written, the nonce stays current and the next poll
            // re-signs the identical message (signatures are
            // deterministic; the file write is idempotent).
            log_event(
                cfg,
                AuditEvent::Signed {
                    direction: direction.label().to_string(),
                    nonce: message.nonce,
                    message_hash: hash_hex(&message),
                    relayer: signer.pubkey().to_string(),
                },
            )?;
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

fn registered_relayers(cfg: &RelayerConfig, direction: Direction) -> Result<[Pubkey; 5]> {
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
            Ok(c.relayers.relayers)
        }
        Direction::MachineToSolana => {
            let c = bridge_common::LockConfig::try_from_slice(&data)
                .map_err(RelayerError::Borsh)?;
            Ok(c.relayers.relayers)
        }
    }
}

fn collect(
    cfg: &RelayerConfig,
    direction: Direction,
    nonce: u64,
) -> Result<(bridge_common::BridgeMessage, Vec<bridge_common::RelayerSignature>)> {
    let relayers = registered_relayers(cfg, direction)?;
    let collector = Collector {
        relayers,
        quorum: bridge_common::QUORUM,
    };
    let dest_slot = dest_client(cfg, direction).finalized_slot()?;
    match collector.collect(Path::new(&cfg.attestation_dir), direction, nonce, dest_slot) {
        Ok((message, signatures)) => {
            log_event(
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
                let _ = log_event(
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

fn submit(
    cfg: &RelayerConfig,
    direction: Direction,
    nonce: u64,
    destination_token_account: &Pubkey,
) -> Result<()> {
    let (message, signatures) = collect(cfg, direction, nonce)?;
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
    let payer = solana_sdk::signature::read_keypair_file(&cfg.payer_keypair_path)
        .map_err(|e| RelayerError::InvalidKeypair(format!("payer: {e}")))?;
    use solana_sdk::signer::Signer as _;
    let payer_pubkey = payer.pubkey();
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
        log_event(
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
            let (message, sigs) = collect(&cfg, direction, nonce)?;
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
            submit(&cfg, direction, nonce, &dest)
        }
        "run" => {
            let signer = RelayerSigner::load(&cfg.relayer_keypair_path)?;
            let mut state = RelayerState::load(&cfg.state_path, cfg.start_nonce)?;
            let dest_arg = arg_value(&args, "--destination-token-account");
            println!(
                "relayer {} running (submit_enabled={})",
                signer.pubkey(),
                cfg.submit_enabled
            );
            loop {
                for direction in [Direction::SolanaToMachine, Direction::MachineToSolana] {
                    if let Err(e) = sign_once(&cfg, &signer, &mut state, direction) {
                        eprintln!("{}: sign: {e}", direction.label());
                    }
                    // If a quorum already exists for the nonce we just
                    // passed (or an earlier one), try to submit it when
                    // a destination token account was supplied.
                    if let Some(dest_str) = &dest_arg {
                        if let Ok(dest) = parse_pubkey("destination-token-account", dest_str) {
                            // The last signed nonce in this direction:
                            let nonce = state.next_nonce(direction).saturating_sub(1);
                            if state.next_nonce(direction) > cfg.start_nonce {
                                if let Err(e) = submit(&cfg, direction, nonce, &dest) {
                                    // QuorumNotReached / already processed are
                                    // normal in the loop; report and continue.
                                    eprintln!("{}: submit nonce {}: {e}", direction.label(), nonce);
                                }
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
