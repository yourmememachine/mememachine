//! `crossing-proof` — export and verify Machine bridge crossing
//! proof bundles (bridge/PROOF-BUNDLE.md).
//!
//! ```sh
//! # Verify a bundle, fully offline. Exit 0 = valid, 1 = invalid.
//! crossing-proof verify bundle.json
//!
//! # Export a bundle for a completed crossing from the relayer
//! # attestation-exchange directory and the receipt the destination
//! # program wrote (its fields, as read from the receipt PDA).
//! crossing-proof export \
//!   --attestations ./exchange \
//!   --receipt receipt.json \
//!   --direction solana-to-machine --nonce 7 \
//!   --relayers <PK1>,<PK2>,<PK3>,<PK4>,<PK5> \
//!   --out bundle.json
//! ```
//!
//! Export verifies the assembled bundle before writing it: the
//! tool never emits a bundle its own verifier would reject.

use proof_bundle::{export_bundle, verify_bundle, CrossingBundle, ReceiptFile};
use relayer::message::Direction;
use solana_program::pubkey::Pubkey;
use std::{env, path::Path, process::ExitCode, str::FromStr};

fn usage() -> ! {
    eprintln!("usage:");
    eprintln!("  crossing-proof verify <BUNDLE_JSON>");
    eprintln!("  crossing-proof export --attestations <DIR> --receipt <RECEIPT_JSON> \\");
    eprintln!("      --direction <solana-to-machine|machine-to-solana> --nonce <N> \\");
    eprintln!("      --relayers <PK,PK,PK,PK,PK> [--quorum 3] [--out <PATH>]");
    eprintln!();
    eprintln!("  verify   re-check a bundle offline; exit 0 valid, 1 invalid");
    eprintln!("  export   collect the quorum from the attestation-exchange directory,");
    eprintln!("           pair it with the receipt, verify, and write the bundle");
    eprintln!("           (stdout unless --out is given)");
    std::process::exit(2);
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("verify") => cmd_verify(&args[1..]),
        Some("export") => cmd_export(&args[1..]),
        _ => usage(),
    }
}

fn cmd_verify(args: &[String]) -> ExitCode {
    let [path] = args else { usage() };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("INVALID: cannot read {path}: {e}");
            return ExitCode::from(1);
        }
    };
    let bundle: CrossingBundle = match serde_json::from_str(&text) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("INVALID: {path} is not a well-formed bundle: {e}");
            return ExitCode::from(1);
        }
    };
    match verify_bundle(&bundle) {
        Ok(report) => {
            println!("VALID crossing proof");
            println!("  direction:       {}", report.direction);
            println!("  nonce:           {}", report.nonce);
            println!("  amount:          {}", report.amount);
            println!("  recipient:       {}", report.recipient);
            println!("  message hash:    {}", proof_bundle::hex_encode(&report.message_hash));
            println!(
                "  attestations:    {} distinct relayers (quorum {} of {})",
                report.distinct_signers, report.quorum, report.relayer_set_size
            );
            println!(
                "  receipt:         matches the attested message (destination slot {}, timestamp {})",
                report.receipt_slot, report.receipt_timestamp
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("INVALID: {e}");
            ExitCode::from(1)
        }
    }
}

struct ExportArgs {
    attestations: String,
    receipt: String,
    direction: Direction,
    nonce: u64,
    relayers: Vec<Pubkey>,
    quorum: u8,
    out: Option<String>,
}

fn parse_export_args(args: &[String]) -> ExportArgs {
    let mut attestations: Option<String> = None;
    let mut receipt: Option<String> = None;
    let mut direction: Option<Direction> = None;
    let mut nonce: Option<u64> = None;
    let mut relayers: Option<Vec<Pubkey>> = None;
    let mut quorum: u8 = bridge_common::QUORUM;
    let mut out: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let value = |it: &mut std::slice::Iter<String>| {
            it.next().cloned().unwrap_or_else(|| usage())
        };
        match arg.as_str() {
            "--attestations" => attestations = Some(value(&mut it)),
            "--receipt" => receipt = Some(value(&mut it)),
            "--direction" => {
                let v = value(&mut it);
                direction = Some(Direction::parse(&v).unwrap_or_else(|_| {
                    eprintln!("error: unknown direction '{v}'");
                    usage()
                }));
            }
            "--nonce" => {
                let v = value(&mut it);
                nonce = Some(v.parse().unwrap_or_else(|_| {
                    eprintln!("error: --nonce is not a number: {v}");
                    usage()
                }));
            }
            "--relayers" => {
                let v = value(&mut it);
                let mut set = Vec::new();
                for part in v.split(',') {
                    set.push(Pubkey::from_str(part.trim()).unwrap_or_else(|_| {
                        eprintln!("error: --relayers entry is not a pubkey: {part}");
                        usage()
                    }));
                }
                relayers = Some(set);
            }
            "--quorum" => {
                let v = value(&mut it);
                quorum = v.parse().unwrap_or_else(|_| {
                    eprintln!("error: --quorum is not a number: {v}");
                    usage()
                });
            }
            "--out" => out = Some(value(&mut it)),
            "--help" | "-h" => usage(),
            other => {
                eprintln!("error: unknown argument: {other}");
                usage();
            }
        }
    }
    let (Some(attestations), Some(receipt), Some(direction), Some(nonce), Some(relayers)) =
        (attestations, receipt, direction, nonce, relayers)
    else {
        eprintln!("error: --attestations, --receipt, --direction, --nonce and --relayers are all required");
        usage();
    };
    ExportArgs {
        attestations,
        receipt,
        direction,
        nonce,
        relayers,
        quorum,
        out,
    }
}

fn cmd_export(args: &[String]) -> ExitCode {
    let args = parse_export_args(args);
    let relayer_set: [Pubkey; bridge_common::MAX_RELAYERS] =
        match args.relayers.clone().try_into() {
            Ok(set) => set,
            Err(v) => {
                eprintln!(
                    "error: --relayers must list exactly {} pubkeys, got {}",
                    bridge_common::MAX_RELAYERS,
                    v.len()
                );
                return ExitCode::from(2);
            }
        };
    let receipt_text = match std::fs::read_to_string(&args.receipt) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read receipt file {}: {e}", args.receipt);
            return ExitCode::from(1);
        }
    };
    let receipt_file: ReceiptFile = match serde_json::from_str(&receipt_text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {} is not a well-formed receipt: {e}", args.receipt);
            return ExitCode::from(1);
        }
    };
    let receipt = match receipt_file.to_receipt() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: receipt file {}: {e}", args.receipt);
            return ExitCode::from(1);
        }
    };
    if receipt.nonce != args.nonce {
        eprintln!(
            "error: receipt is for nonce {}, but --nonce {} was given",
            receipt.nonce, args.nonce
        );
        return ExitCode::from(1);
    }
    let bundle = match export_bundle(
        Path::new(&args.attestations),
        args.direction,
        args.nonce,
        &relayer_set,
        args.quorum,
        &receipt,
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    let json = serde_json::to_string_pretty(&bundle).expect("bundle serializes");
    match &args.out {
        Some(path) => {
            if let Err(e) = std::fs::write(path, format!("{json}\n")) {
                eprintln!("error: cannot write {path}: {e}");
                return ExitCode::from(1);
            }
            eprintln!("wrote crossing proof bundle for nonce {} to {path}", args.nonce);
        }
        None => println!("{json}"),
    }
    ExitCode::SUCCESS
}
