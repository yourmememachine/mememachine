//! `launchguard-registry` — emit the public LaunchGuard registry as JSON.
//!
//! ```sh
//! launchguard-registry --url http://127.0.0.1:8899 --program-id <LAUNCHGUARD_PROGRAM_ID>
//! launchguard-registry --url http://127.0.0.1:8899 --program-id <ID> --verify --out registry.json
//! ```
//!
//! The program id is the deployed LaunchGuard program. There is no
//! deployed program yet (local only — see STATUS.md), so the flag is
//! required: the tool refuses to guess which program to read.
//!
//! `--verify` additionally fetches each launch's vault token account
//! and mint account and re-checks the registration invariants
//! (vault covers the locked amount; both mint authorities revoked).
//! Without it, the `vault_*` / `authorities_revoked` fields are null
//! — the registry never claims a check it did not run.

use registry_export::{build_registry, verify_registry, Registry};
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
};
use solana_sdk::{account::Account, pubkey::Pubkey};
use std::{collections::HashMap, env, process::ExitCode};

const DEFAULT_URL: &str = "http://127.0.0.1:8899";

fn usage() -> ! {
    eprintln!(
        "usage: launchguard-registry --program-id <PUBKEY> [--url <RPC_URL>] [--out <PATH>] [--verify]"
    );
    eprintln!("  --url         RPC endpoint (default {DEFAULT_URL})");
    eprintln!("  --program-id  deployed LaunchGuard program id (required)");
    eprintln!("  --out         write JSON to PATH instead of stdout");
    eprintln!("  --verify      re-check vault balances and revoked authorities");
    std::process::exit(2);
}

struct Args {
    url: String,
    program_id: Pubkey,
    out: Option<String>,
    verify: bool,
}

fn parse_args() -> Args {
    let mut url = DEFAULT_URL.to_string();
    let mut program_id: Option<Pubkey> = None;
    let mut out: Option<String> = None;
    let mut verify = false;
    let mut argv = env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--url" => url = argv.next().unwrap_or_else(|| usage()),
            "--program-id" => {
                let value = argv.next().unwrap_or_else(|| usage());
                program_id = Some(value.parse().unwrap_or_else(|_| {
                    eprintln!("error: --program-id is not a valid pubkey: {value}");
                    usage()
                }));
            }
            "--out" => out = Some(argv.next().unwrap_or_else(|| usage())),
            "--verify" => verify = true,
            "--help" | "-h" => usage(),
            other => {
                eprintln!("error: unknown argument: {other}");
                usage();
            }
        }
    }
    let Some(program_id) = program_id else {
        eprintln!("error: --program-id is required (no LaunchGuard program is deployed yet; pass the program id to read)");
        usage();
    };
    Args {
        url,
        program_id,
        out,
        verify,
    }
}

/// Fetch the program's accounts, build the registry, and — with
/// `--verify` — fetch the vault and mint accounts behind each entry
/// and re-check the invariants. Returns the JSON string.
fn export_registry(args: &Args) -> Result<String, String> {
    let client = RpcClient::new(args.url.clone());
    let slot = client
        .get_slot()
        .map_err(|e| format!("getSlot failed on {}: {e}", args.url))?;
    let accounts = client
        .get_program_accounts_with_config(
            &args.program_id,
            RpcProgramAccountsConfig {
                filters: None,
                account_config: RpcAccountInfoConfig {
                    encoding: Some(solana_account_decoder::UiAccountEncoding::Base64),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .map_err(|e| format!("getProgramAccounts failed: {e}"))?;
    let mut registry: Registry = build_registry(&accounts, &args.program_id, slot);
    if args.verify {
        verify_with_rpc(&client, &mut registry)?;
    }
    serde_json::to_string_pretty(&registry).map_err(|e| format!("JSON encode failed: {e}"))
}

fn verify_with_rpc(client: &RpcClient, registry: &mut Registry) -> Result<(), String> {
    let mut keys: Vec<Pubkey> = Vec::new();
    for entry in &registry.launches {
        if let Ok(vault) = entry.vault.parse::<Pubkey>() {
            keys.push(vault);
        }
        if let Ok(mint) = entry.mint.parse::<Pubkey>() {
            keys.push(mint);
        }
    }
    let mut vault_accounts: HashMap<Pubkey, Account> = HashMap::new();
    let mut mint_accounts: HashMap<Pubkey, Account> = HashMap::new();
    // getMultipleAccounts caps at 100 keys per call.
    for chunk in keys.chunks(100) {
        let fetched = client
            .get_multiple_accounts(chunk)
            .map_err(|e| format!("getMultipleAccounts failed: {e}"))?;
        for (key, account) in chunk.iter().zip(fetched.into_iter()) {
            if let Some(account) = account {
                // Vaults are token accounts owned by the SPL Token
                // program; mints are mint accounts. The verify step
                // sorts them by which entry field named the key.
                let is_vault = registry.launches.iter().any(|e| {
                    e.vault
                        .parse::<Pubkey>()
                        .map(|v| v == *key)
                        .unwrap_or(false)
                });
                if is_vault {
                    vault_accounts.insert(*key, account);
                } else {
                    mint_accounts.insert(*key, account);
                }
            }
        }
    }
    verify_registry(registry, &vault_accounts, &mint_accounts);
    Ok(())
}

fn main() -> ExitCode {
    let args = parse_args();
    match export_registry(&args) {
        Ok(json) => {
            if let Some(path) = &args.out {
                if let Err(e) = std::fs::write(path, format!("{json}\n")) {
                    eprintln!("error: cannot write {path}: {e}");
                    return ExitCode::from(1);
                }
            } else {
                println!("{json}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
