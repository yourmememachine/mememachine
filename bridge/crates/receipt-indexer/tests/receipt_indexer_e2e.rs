//! End-to-end receipt-indexer test (the drill-mode demo): a real
//! crossing on the Machine-side bridge program in program-test —
//! a 3-of-5 attested MintWrapped, which writes a real crossing
//! receipt — plus a real on-chain replay attempt against the same
//! nonce, which fails. The receipt account the program wrote is
//! then bound to its creating transaction through the indexer's
//! own code path, using RPC-shaped fixtures built from the actual
//! transactions (their real signatures, the receipt's real slot,
//! the mint instruction's real account list), and through the
//! shipped `receipt-index` binary in offline mode.
//!
//! No network, no deployed programs, no real assets.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use borsh::BorshDeserialize;
use bridge_common::*;
use machine_bridge::processor;
use receipt_indexer::{index_crossing, IndexError};
use relayer::message::Direction;
use solana_program::program_pack::Pack;
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use std::path::PathBuf;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([42u8; 32])
}

fn spl_token_builtin<'a>(
    vm: *mut solana_program_test::EbpfVm<'a, solana_program_test::InvokeContext<'static>>,
    _arg0: u64,
    _arg1: u64,
    _arg2: u64,
    _arg3: u64,
    _arg4: u64,
) {
    let vm = unsafe {
        &mut *((vm as *mut u64)
            .offset(-(solana_program_test::get_runtime_environment_key() as isize))
            as *mut solana_program_test::EbpfVm<solana_program_test::InvokeContext>)
    };
    vm.program_result = solana_program_test::invoke_builtin_function(
        spl_token::processor::Processor::process,
        vm.context_object_pointer,
    )
    .map_err(|err| solana_program_test::EbpfError::SyscallError(err))
    .into();
}

#[allow(deprecated)]
fn programdata_address() -> Pubkey {
    solana_program::bpf_loader_upgradeable::get_program_data_address(&program_id())
}

#[allow(deprecated)]
fn seed_programdata(test: &mut ProgramTest, authority: &Pubkey) {
    let mut data = vec![0u8; 45];
    data[0..4].copy_from_slice(&3u32.to_le_bytes());
    data[12] = 1;
    data[13..45].copy_from_slice(authority.as_ref());
    test.add_account(
        programdata_address(),
        Account {
            lamports: 10_000_000,
            data,
            owner: solana_program::bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
}

struct Env {
    context: ProgramTestContext,
    relayers: [Keypair; 5],
    wrapped_mint: Pubkey,
    recipient_token_account: Pubkey,
    recipient: Pubkey,
}

async fn create_mint(
    ctx: &mut ProgramTestContext,
    mint_authority: &Pubkey,
    freeze_authority: Option<&Pubkey>,
) -> Pubkey {
    let mint = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            system_instruction::create_account(
                &ctx.payer.pubkey(),
                &mint.pubkey(),
                rent.minimum_balance(spl_token::state::Mint::LEN),
                spl_token::state::Mint::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_mint(
                &spl_token::id(),
                &mint.pubkey(),
                mint_authority,
                freeze_authority,
                6,
            )
            .unwrap(),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &mint],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    mint.pubkey()
}

async fn create_token_account(
    ctx: &mut ProgramTestContext,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Pubkey {
    let account = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            system_instruction::create_account(
                &ctx.payer.pubkey(),
                &account.pubkey(),
                rent.minimum_balance(spl_token::state::Account::LEN),
                spl_token::state::Account::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_account(
                &spl_token::id(),
                &account.pubkey(),
                mint,
                owner,
            )
            .unwrap(),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &account],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    account.pubkey()
}

async fn setup() -> Env {
    let pid = program_id();
    let deployer = Keypair::new();
    let mut test = ProgramTest::new("machine_bridge", pid, solana_program_test::processor!(processor::process));
    test.add_builtin_program("spl_token", spl_token::id(), spl_token_builtin);
    seed_programdata(&mut test, &deployer.pubkey());
    let mut ctx = test.start_with_context().await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(&ctx.payer.pubkey(), &deployer.pubkey(), 1_000_000_000)],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund).await.unwrap();

    let relayers: [Keypair; 5] = [
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
    ];
    let relayer_pubs: [Pubkey; 5] = [
        relayers[0].pubkey(),
        relayers[1].pubkey(),
        relayers[2].pubkey(),
        relayers[3].pubkey(),
        relayers[4].pubkey(),
    ];
    let admins: [Keypair; 3] = [Keypair::new(), Keypair::new(), Keypair::new()];
    let admin_pubs: [Pubkey; 3] = [admins[0].pubkey(), admins[1].pubkey(), admins[2].pubkey()];

    let (authority, _) = authority_pda(&pid);
    let wrapped_mint = create_mint(&mut ctx, &authority, Some(&authority)).await;
    let recipient = Keypair::new();
    let recipient_token_account = create_token_account(&mut ctx, &wrapped_mint, &recipient.pubkey()).await;

    let (config, _) = config_pda(&pid);
    let init = Instruction::new_with_borsh(
        pid,
        &MachineInstruction::Initialize {
            relayers: relayer_pubs,
            admins: admin_pubs,
            wrapped_mint,
            source_mint: Pubkey::new_from_array([42u8; 32]),
            per_tx_cap: 10_000_000,
            daily_cap: 25_000_000,
        },
        vec![
            AccountMeta::new(deployer.pubkey(), true),
            AccountMeta::new(config, false),
            AccountMeta::new_readonly(wrapped_mint, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(programdata_address(), false),
        ],
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[init],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &deployer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    Env {
        context: ctx,
        relayers,
        wrapped_mint,
        recipient_token_account,
        recipient: recipient.pubkey(),
    }
}

fn test_message(env: &Env, nonce: u64, amount: u64) -> BridgeMessage {
    BridgeMessage {
        version: MESSAGE_VERSION,
        source_chain: CHAIN_SOLANA,
        dest_chain: CHAIN_MACHINE,
        nonce,
        source_mint: Pubkey::new_from_array([42u8; 32]),
        sender: Pubkey::new_unique(),
        recipient: env.recipient,
        amount,
        source_tx: [7u8; 64],
        source_slot: 100,
        expiry_slot: 1_000_000,
    }
}

fn sign(msg: &BridgeMessage, keys: &[Keypair], n: usize) -> Vec<RelayerSignature> {
    let hash = msg.hash();
    keys.iter()
        .take(n)
        .map(|k| {
            let sig = k.sign_message(&hash);
            let mut bytes = [0u8; 64];
            bytes.copy_from_slice(sig.as_ref());
            RelayerSignature {
                relayer: k.pubkey(),
                signature: bytes,
            }
        })
        .collect()
}

/// Build the MintWrapped transaction exactly as the submitter
/// would; returns the transaction (for its real signature and
/// instruction shape) without processing it.
async fn build_mint_tx(env: &mut Env, message: &BridgeMessage) -> Transaction {
    let pid = program_id();
    let (config, _) = config_pda(&pid);
    let (nonce_rec, _) = nonce_pda(&pid, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (receipt, _) = receipt_pda(&pid, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (authority, _) = authority_pda(&pid);
    let sigs = sign(message, &env.relayers, 3);
    let ed25519_ix = build_ed25519_instruction(&message.hash(), &sigs);
    let mint_ix = Instruction::new_with_borsh(
        pid,
        &MachineInstruction::MintWrapped {
            message: message.clone(),
        },
        vec![
            AccountMeta::new(config, false),
            AccountMeta::new(nonce_rec, false),
            AccountMeta::new(env.wrapped_mint, false),
            AccountMeta::new(env.recipient_token_account, false),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(env.context.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
            AccountMeta::new(receipt, false),
        ],
    );
    let blockhash = env.context.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        &[ed25519_ix, mint_ix],
        Some(&env.context.payer.pubkey()),
        &[&env.context.payer],
        blockhash,
    )
}

fn sig_b58(tx: &Transaction) -> String {
    bs58::encode(tx.signatures[0].as_ref()).into_string()
}

/// The getTransaction (jsonParsed) shape for the real mint
/// transaction: its real slot is the receipt's slot (the program
/// wrote it from the Clock as the receipt was created), its real
/// instruction account lists come from the transaction itself.
fn tx_fixture(tx: &Transaction, receipt: &CrossingReceipt) -> serde_json::Value {
    let instructions: Vec<serde_json::Value> = tx
        .message
        .instructions
        .iter()
        .map(|ix| {
            serde_json::json!({
                "programId": tx.message.account_keys[ix.program_id_index as usize].to_string(),
                "accounts": ix.accounts.iter().map(|i| tx.message.account_keys[*i as usize].to_string()).collect::<Vec<_>>(),
                "data": bs58::encode(&ix.data).into_string(),
            })
        })
        .collect();
    let account_keys: Vec<serde_json::Value> = tx
        .message
        .account_keys
        .iter()
        .map(|k| serde_json::json!({"pubkey": k.to_string(), "signer": false, "writable": true}))
        .collect();
    serde_json::json!({
        "slot": receipt.slot,
        "blockTime": receipt.timestamp,
        "meta": {"err": null},
        "transaction": {"message": {"accountKeys": account_keys, "instructions": instructions}},
    })
}

#[tokio::test]
async fn real_crossing_receipt_binds_to_its_creating_transaction() {
    let mut env = setup().await;
    let pid = program_id();
    let message = test_message(&env, 1, 5_000_000);

    // --- The crossing, and a replay attempt against the same nonce ---
    env.context.warp_to_slot(50).unwrap();
    let mint_tx = build_mint_tx(&mut env, &message).await;
    let mint_sig = sig_b58(&mint_tx);
    env.context
        .banks_client
        .process_transaction(mint_tx.clone())
        .await
        .unwrap();

    env.context.warp_to_slot(100).unwrap();
    let replay_tx = build_mint_tx(&mut env, &message).await;
    let replay_sig = sig_b58(&replay_tx);
    assert_ne!(mint_sig, replay_sig);
    let replay_result = env.context.banks_client.process_transaction(replay_tx).await;
    assert!(replay_result.is_err(), "replaying nonce 1 must fail on-chain");

    // --- The receipt the program wrote ---
    let (pda, _) = receipt_pda(&pid, DIR_SOLANA_TO_MACHINE, 1);
    let account = env
        .context
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("MintWrapped must have written a crossing receipt");
    let receipt = CrossingReceipt::try_from_slice(&account.data).unwrap();
    assert_eq!(receipt.amount, 5_000_000);
    assert_eq!(receipt.message_hash, message.hash());

    // --- RPC-shaped fixtures from the real transactions ---
    let account_result = serde_json::json!({
        "value": {
            "owner": account.owner.to_string(),
            "data": [B64.encode(&account.data), "base64"],
            "lamports": account.lamports,
            "executable": false,
        }
    });
    // Newest first, as the RPC returns them: the failed replay is
    // the most recent signature for the PDA; the creation is older.
    let signatures = serde_json::json!([
        {
            "signature": replay_sig,
            "slot": 100,
            "err": {"InstructionError": [1, {"Custom": 0}]},
            "blockTime": receipt.timestamp + 20,
            "confirmationStatus": "finalized",
        },
        {
            "signature": mint_sig,
            "slot": receipt.slot,
            "err": null,
            "blockTime": receipt.timestamp,
            "confirmationStatus": "finalized",
        },
    ]);
    let tx = tx_fixture(&mint_tx, &receipt);

    // --- The binding, in-process ---
    let record = index_crossing(
        &pid,
        Direction::SolanaToMachine,
        1,
        &account_result,
        &signatures,
        &tx,
    )
    .unwrap();
    assert_eq!(record.destination_tx, mint_sig);
    assert_eq!(record.receipt_pda, pda.to_string());
    assert_eq!(record.destination_slot, receipt.slot);
    assert_eq!(record.signatures_for_receipt, 2);
    assert_eq!(record.successful_signatures, 1);
    assert!(!record.extra_successful_signatures);

    // A transaction from the wrong slot does not bind, even with
    // the right accounts and instruction shape.
    let mut wrong_slot = tx.clone();
    wrong_slot["slot"] = serde_json::json!(receipt.slot + 1);
    assert!(matches!(
        index_crossing(&pid, Direction::SolanaToMachine, 1, &account_result, &signatures, &wrong_slot),
        Err(IndexError::SlotMismatch { .. })
    ));

    // --- The shipped binary, offline mode, on the same fixtures ---
    let dir: PathBuf = std::env::temp_dir().join(format!("receipt-indexer-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let account_path = dir.join("account.json");
    let sigs_path = dir.join("sigs.json");
    let tx_path = dir.join("tx.json");
    std::fs::write(&account_path, serde_json::to_string_pretty(&account_result).unwrap()).unwrap();
    std::fs::write(&sigs_path, serde_json::to_string_pretty(&signatures).unwrap()).unwrap();
    std::fs::write(&tx_path, serde_json::to_string_pretty(&tx).unwrap()).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_receipt-index"))
        .args([
            "offline",
            "--program",
            &pid.to_string(),
            "--direction",
            "solana-to-machine",
            "--nonce",
            "1",
            "--account",
            account_path.to_str().unwrap(),
            "--signatures",
            sigs_path.to_str().unwrap(),
            "--transaction",
            tx_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("receipt-index binary runs");
    assert!(
        out.status.success(),
        "CLI must bind the real crossing: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed: ReceiptRecordPrint =
        serde_json::from_slice(&out.stdout).expect("CLI prints the record as JSON");
    assert_eq!(printed.destination_tx, mint_sig);
    assert_eq!(printed.receipt_pda, pda.to_string());

    let _ = std::fs::remove_dir_all(&dir);
}

/// The subset of the CLI's JSON output this test pins.
#[derive(serde::Deserialize)]
struct ReceiptRecordPrint {
    receipt_pda: String,
    destination_tx: String,
}
