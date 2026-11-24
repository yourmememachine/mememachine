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

use proof_bundle::{
    export_bundle, verify_bundle, verify_bundle_against_set, CrossingBundle, ReceiptFile,
};
use relayer::message::Direction;
use solana_program::pubkey::Pubkey;
use std::{env, path::Path, process::ExitCode, str::FromStr};

/// Cap on files the CLI will load into memory (debate round
/// 2026-11-18, B-F8): the verifier exists to be run on files handed
/// over by other people, and a real bundle is a few kilobytes — an
/// unbounded read let a crafted multi-GB "bundle" exhaust memory
/// before any check ran. The relayer caps its exchange files at
/// 64 KiB for the same reason; 1 MiB is generous headroom here.
const MAX_INPUT_BYTES: u64 = 1 << 20;

fn read_capped(path: &str) -> Result<String, String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("cannot read {path}: {e}"))?
        .len();
    if len > MAX_INPUT_BYTES {
        return Err(format!(
            "{path} is {len} bytes, over the {MAX_INPUT_BYTES}-byte input cap"
        ));
    }
    // The metadata length alone does not bound the read: a special
    // file whose stated size is 0 (e.g. /dev/zero, a FIFO, a /proc
    // file) or a file grown between the check and the read would
    // stream without limit into `read_to_string` and exhaust
    // memory — the exact outcome the cap exists to prevent
    // (debate round 2026-11-23, B-F1). Read through a bounded
    // reader instead and reject if one byte past the cap exists;
    // that closes the special-file and growth cases together.
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut buf = Vec::new();
    file.take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read {path}: {e}"))?;
    if buf.len() as u64 > MAX_INPUT_BYTES {
        return Err(format!(
            "{path} is over the {MAX_INPUT_BYTES}-byte input cap"
        ));
    }
    String::from_utf8(buf).map_err(|e| format!("cannot read {path}: {e}"))
}

fn usage() -> ! {
    eprintln!("usage:");
    eprintln!("  crossing-proof verify <BUNDLE_JSON> [--expected-relayers <PK,PK,PK,PK,PK>]");
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
    // verify <BUNDLE_JSON> [--expected-relayers <PK,PK,PK,PK,PK>]
    let mut path: Option<&String> = None;
    let mut expected: Option<[Pubkey; bridge_common::MAX_RELAYERS]> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--expected-relayers" => {
                let Some(v) = it.next() else { usage() };
                let mut set = Vec::new();
                for part in v.split(',') {
                    match Pubkey::from_str(part.trim()) {
                        Ok(pk) => set.push(pk),
                        Err(_) => {
                            eprintln!("error: --expected-relayers entry is not a pubkey: {part}");
                            return ExitCode::from(2);
                        }
                    }
                }
                match set.try_into() {
                    Ok(s) => expected = Some(s),
                    Err(v) => {
                        eprintln!(
                            "error: --expected-relayers must list exactly {} pubkeys, got {}",
                            bridge_common::MAX_RELAYERS,
                            v.len()
                        );
                        return ExitCode::from(2);
                    }
                }
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown argument: {other}");
                usage();
            }
            _ => {
                if path.is_some() {
                    usage();
                }
                path = Some(arg);
            }
        }
    }
    let Some(path) = path else { usage() };
    let text = match read_capped(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("INVALID: {e}");
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
    let verified = match &expected {
        Some(set) => verify_bundle_against_set(&bundle, set),
        None => verify_bundle(&bundle),
    };
    match verified {
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
            // Debate round 2026-11-18 (A-5): name the set the verdict
            // is about — a count alone hid whose quorum it was.
            println!(
                "  relayer set:     {}",
                report
                    .relayer_set
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            if expected.is_some() {
                println!("  set check:       matches the expected set given on the command line");
            }
            // Debate round 2026-11-18 (A-4): the receipt fields are
            // the bundle's statement, not an on-chain observation.
            println!(
                "  receipt:         stated fields match the attested message (stated destination slot {}, timestamp {})",
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
    let receipt_text = match read_capped(&args.receipt) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, contents: &[u8]) -> String {
        let path = std::env::temp_dir().join(format!(
            "crossing-proof-{name}-{}",
            std::process::id()
        ));
        std::fs::write(&path, contents).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn read_capped_reads_a_small_file() {
        let path = temp_file("small", b"{\"ok\":true}");
        assert_eq!(read_capped(&path).unwrap(), "{\"ok\":true}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_capped_rejects_a_file_over_the_cap() {
        let contents = vec![b'x'; (MAX_INPUT_BYTES + 1) as usize];
        let path = temp_file("big", &contents);
        assert!(read_capped(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    #[test]
    fn read_capped_rejects_an_unbounded_special_file() {
        // Debate round 2026-11-23 (B-F1): /dev/zero reports a
        // metadata size of 0, so the pre-read length check passed
        // and the unbounded read grew until memory exhaustion.
        // The bounded read must reject it at the cap instead.
        // (Pre-fix this test does not terminate promptly; it is a
        // regression pin for the fixed behaviour.)
        assert!(read_capped("/dev/zero").is_err());
    }
}
