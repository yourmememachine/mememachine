//! `receipt-index` — resolve a Machine bridge crossing receipt to
//! the transaction that created it and print the full crossing
//! record (bridge/RECEIPT-INDEXER.md).
//!
//! ```sh
//! # Live: read the destination chain's RPC (finalized commitment).
//! receipt-index --rpc http://127.0.0.1:8899 \
//!   --program <DEST_PROGRAM_ID> \
//!   --direction solana-to-machine --nonce 7
//!
//! # Offline / drill mode: the same binding over saved RPC output
//! # (a getAccountInfo result, a getSignaturesForAddress result
//! # array, and a getTransaction result, as JSON files).
//! receipt-index offline --program <DEST_PROGRAM_ID> \
//!   --direction solana-to-machine --nonce 7 \
//!   --account account.json --signatures sigs.json --transaction tx.json
//! ```
//!
//! Exit 0 = the record is printed (stdout, JSON with --json, plain
//! text otherwise); exit 1 = the crossing could not be bound — the
//! reason names the exact check that failed.

use receipt_indexer::{index_crossing, ReceiptRecord};
use relayer::message::Direction;
use serde_json::{json, Value};
use solana_program::pubkey::Pubkey;
use std::{env, process::ExitCode, str::FromStr};

fn usage() -> ! {
    eprintln!("usage:");
    eprintln!("  receipt-index --rpc <URL> --program <PK> --direction <solana-to-machine|machine-to-solana> --nonce <N> [--json]");
    eprintln!("  receipt-index offline --program <PK> --direction <D> --nonce <N> \\");
    eprintln!("      --account <FILE> --signatures <FILE> --transaction <FILE> [--json]");
    eprintln!();
    eprintln!("  live mode reads the destination chain at finalized commitment;");
    eprintln!("  offline mode runs the identical binding over saved RPC output.");
    std::process::exit(2);
}

// ---------------------------------------------------------------------------
// Minimal JSON-RPC client (same shape as the relayer's: finalized
// commitment everywhere — an indexer must not bind a crossing from
// a merely processed view of the chain)
// ---------------------------------------------------------------------------

struct RpcClient {
    url: String,
    agent: ureq::Agent,
}

impl RpcClient {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
        }
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let resp: Value = self
            .agent
            .post(&self.url)
            .send_json(body)
            .map_err(|e| format!("{method}: {e}"))?
            .into_json()
            .map_err(|e| format!("{method}: response json: {e}"))?;
        if let Some(err) = resp.get("error") {
            return Err(format!("{method}: {err}"));
        }
        resp.get("result")
            .cloned()
            .ok_or_else(|| format!("{method}: no result field"))
    }

    fn account_info(&self, pubkey: &Pubkey) -> Result<Value, String> {
        self.call(
            "getAccountInfo",
            json!([pubkey.to_string(), {"encoding": "base64", "commitment": "finalized"}]),
        )
    }

    fn signatures_for_address(&self, pubkey: &Pubkey) -> Result<Value, String> {
        self.call(
            "getSignaturesForAddress",
            json!([pubkey.to_string(), {"limit": 1000, "commitment": "finalized"}]),
        )
    }

    fn transaction(&self, signature_b58: &str) -> Result<Value, String> {
        self.call(
            "getTransaction",
            json!([signature_b58, {"encoding": "jsonParsed", "commitment": "finalized", "maxSupportedTransactionVersion": 0}]),
        )
    }
}

// ---------------------------------------------------------------------------
// Argument handling
// ---------------------------------------------------------------------------

struct Common {
    program: Pubkey,
    direction: Direction,
    nonce: u64,
    json_out: bool,
}

fn take(it: &mut std::slice::Iter<String>) -> String {
    it.next().cloned().unwrap_or_else(|| usage())
}

fn parse_common(args: &[String]) -> (Common, Vec<(String, String)>) {
    let mut program: Option<Pubkey> = None;
    let mut direction: Option<Direction> = None;
    let mut nonce: Option<u64> = None;
    let mut json_out = false;
    let mut rest: Vec<(String, String)> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--program" => {
                let v = take(&mut it);
                program = Some(Pubkey::from_str(&v).unwrap_or_else(|_| {
                    eprintln!("error: --program is not a pubkey: {v}");
                    usage()
                }));
            }
            "--direction" => {
                let v = take(&mut it);
                direction = Some(Direction::parse(&v).unwrap_or_else(|_| {
                    eprintln!("error: unknown direction '{v}'");
                    usage()
                }));
            }
            "--nonce" => {
                let v = take(&mut it);
                nonce = Some(v.parse().unwrap_or_else(|_| {
                    eprintln!("error: --nonce is not a number: {v}");
                    usage()
                }));
            }
            "--json" => json_out = true,
            "--help" | "-h" => usage(),
            other if other.starts_with("--") => {
                rest.push((other.to_string(), take(&mut it)));
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                usage();
            }
        }
    }
    let (Some(program), Some(direction), Some(nonce)) = (program, direction, nonce) else {
        eprintln!("error: --program, --direction and --nonce are all required");
        usage();
    };
    (
        Common {
            program,
            direction,
            nonce,
            json_out,
        },
        rest,
    )
}

fn get_opt(rest: &[(String, String)], name: &str) -> Option<String> {
    rest.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

fn print_record(record: &ReceiptRecord, json_out: bool) -> ExitCode {
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(record).expect("record serializes")
        );
    } else {
        println!("Crossing record");
        println!("  direction:       {}", record.direction);
        println!("  nonce:           {}", record.nonce);
        println!("  receipt PDA:     {}", record.receipt_pda);
        println!("  program:         {}", record.program_id);
        println!("  amount:          {}", record.receipt.amount);
        println!("  recipient:       {}", record.receipt.recipient);
        println!("  message hash:    {}", record.receipt.message_hash);
        println!("  source tx:       {}", record.receipt.source_tx);
        println!("  destination tx:  {}", record.destination_tx);
        println!(
            "  destination:     slot {} (receipt timestamp {})",
            record.destination_slot, record.receipt.timestamp
        );
        println!(
            "  signatures:      {} for this receipt ({} successful)",
            record.signatures_for_receipt, record.successful_signatures
        );
        if record.extra_successful_signatures {
            println!(
                "  WARNING:         more than one successful transaction touched this receipt PDA; \
                 the receipt is written exactly once, so the extras are an anomaly — \
                 the binding above is the oldest (the creation)"
            );
        }
    }
    ExitCode::SUCCESS
}

fn report_err(e: receipt_indexer::IndexError) -> ExitCode {
    eprintln!("NOT BOUND: {e}");
    ExitCode::from(1)
}

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

fn cmd_live(args: &[String]) -> ExitCode {
    let (common, rest) = parse_common(args);
    let Some(rpc_url) = get_opt(&rest, "--rpc") else {
        eprintln!("error: --rpc is required in live mode");
        usage();
    };
    let client = RpcClient::new(&rpc_url);
    let (pda, _) = bridge_common::receipt_pda(
        &common.program,
        common.direction.nonce_dir(),
        common.nonce,
    );
    let account = match client.account_info(&pda) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("NOT BOUND: rpc: {e}");
            return ExitCode::from(1);
        }
    };
    // The creating signature must be known before its transaction
    // can be fetched, so resolve it from the signature list first,
    // then run the full binding (which re-checks everything).
    let signatures = match client.signatures_for_address(&pda) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("NOT BOUND: rpc: {e}");
            return ExitCode::from(1);
        }
    };
    let creation = match receipt_indexer::resolve_creation(&signatures) {
        Ok(c) => c,
        Err(e) => return report_err(e),
    };
    let sig_b58 = bs58::encode(creation.signature).into_string();
    let tx = match client.transaction(&sig_b58) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("NOT BOUND: rpc: {e}");
            return ExitCode::from(1);
        }
    };
    match index_crossing(
        &common.program,
        common.direction,
        common.nonce,
        &account,
        &signatures,
        &tx,
    ) {
        Ok(record) => print_record(&record, common.json_out),
        Err(e) => report_err(e),
    }
}

fn read_json_file(path: &str) -> Result<Value, ExitCode> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        eprintln!("error: cannot read {path}: {e}");
        ExitCode::from(1)
    })?;
    serde_json::from_str(&text).map_err(|e| {
        eprintln!("error: {path} is not well-formed JSON: {e}");
        ExitCode::from(1)
    })
}

fn cmd_offline(args: &[String]) -> ExitCode {
    let (common, rest) = parse_common(args);
    let (Some(account_path), Some(sigs_path), Some(tx_path)) = (
        get_opt(&rest, "--account"),
        get_opt(&rest, "--signatures"),
        get_opt(&rest, "--transaction"),
    ) else {
        eprintln!("error: --account, --signatures and --transaction are all required offline");
        usage();
    };
    let account = match read_json_file(&account_path) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let signatures = match read_json_file(&sigs_path) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let tx = match read_json_file(&tx_path) {
        Ok(v) => v,
        Err(code) => return code,
    };
    match index_crossing(
        &common.program,
        common.direction,
        common.nonce,
        &account,
        &signatures,
        &tx,
    ) {
        Ok(record) => print_record(&record, common.json_out),
        Err(e) => report_err(e),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("offline") => cmd_offline(&args[1..]),
        Some("--help") | Some("-h") | None => usage(),
        _ => cmd_live(&args),
    }
}
