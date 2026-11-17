//! `relayer-log` — inspect and verify a relayer's tamper-evident
//! audit log (see `relayer::audit_log` and bridge/RELAYER-LOG.md).
//!
//! Usage:
//!   relayer-log head <log-file>
//!       Print the current checkpoint (`seq:hash`). Publish this
//!       value somewhere the relayer operator cannot rewrite it —
//!       that publication is what turns the hash chain into
//!       evidence.
//!   relayer-log verify <log-file> [--checkpoint <seq>:<hash>]
//!       Verify the chain end to end. With a checkpoint, also
//!       verify the log still contains that published prefix
//!       unchanged (catches truncation and wholesale rewrites,
//!       which internal verification alone cannot).

use relayer::audit_log::{read_entries, verify_chain, verify_checkpoint};
use relayer::{RelayerError, Result};
use std::path::Path;

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("relayer-log: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().cloned().ok_or_else(|| {
        RelayerError::InvalidConfig(
            "usage: relayer-log <head|verify> <log-file> [--checkpoint <seq>:<hash>]".into(),
        )
    })?;
    let path_str = args.get(1).cloned().ok_or_else(|| {
        RelayerError::InvalidConfig("missing <log-file>".into())
    })?;
    let path = Path::new(&path_str);
    let entries = read_entries(path)?;

    match command.as_str() {
        "head" => match verify_chain(&entries)? {
            Some((seq, hash)) => println!("{seq}:{hash}"),
            None => println!("empty log (no checkpoint)"),
        },
        "verify" => {
            let head = match arg_value(&args, "--checkpoint") {
                Some(cp) => {
                    let (seq_str, hash) = cp.split_once(':').ok_or_else(|| {
                        RelayerError::InvalidConfig(
                            "--checkpoint must be <seq>:<hash>".into(),
                        )
                    })?;
                    let seq: u64 = seq_str.parse().map_err(|_| {
                        RelayerError::InvalidConfig("--checkpoint seq must be a number".into())
                    })?;
                    verify_checkpoint(&entries, seq, hash)?
                }
                None => verify_chain(&entries)?,
            };
            match head {
                Some((seq, hash)) => {
                    println!("ok: {} entries, head {seq}:{hash}", entries.len())
                }
                None => println!("ok: empty log"),
            }
        }
        other => {
            return Err(RelayerError::InvalidConfig(format!(
                "unknown command '{other}': use head | verify"
            )));
        }
    }
    Ok(())
}
