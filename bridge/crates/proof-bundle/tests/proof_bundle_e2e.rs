//! End-to-end proof-bundle test: a real crossing through the
//! existing harness — deposit on the Solana-side lock program in
//! program-test, the relayer's actual file exchange and collector,
//! MintWrapped on the Machine-side program (which writes the real
//! crossing receipt) — then export a bundle for that crossing and
//! verify it: in-process, through the `crossing-proof` binary, and
//! against tampered copies that must all be rejected.
//!
//! The harness mirrors `bridge/crates/relayer/tests/full_drill.rs`
//! (the local drill): two local banks stand in for the two chains.
//! No network, no deployed programs, no real assets.

use borsh::BorshDeserialize;
use bridge_common::{
    authority_pda, config_pda, nonce_pda, receipt_pda, CrossingReceipt, EventRecord,
    LockInstruction, MachineInstruction, DIR_SOLANA_TO_MACHINE, QUORUM,
};
use proof_bundle::{export_bundle, verify_bundle, BundleError, ReceiptFile};
use relayer::attestation::{AttestationFile, Collector};
use relayer::message::{build_message, Direction, ObservedEvent};
use relayer::signing::RelayerSigner;
use relayer::submit::build_mint_instructions;
use solana_program::program_pack::Pack;
use solana_program_test::{processor, ProgramTest, ProgramTestContext};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use spl_token::state::{Account as TokenAccount, Mint};
use std::path::PathBuf;

const EXPIRY_WINDOW: u64 = 100_000;

fn machine_program_id() -> Pubkey {
    Pubkey::new_from_array([42u8; 32])
}
fn lock_program_id() -> Pubkey {
    Pubkey::new_from_array([43u8; 32])
}

/// ProgramData address for `program_id` under the BPF upgradeable
/// loader (self-audit F-1: Initialize is bound to the upgrade
/// authority recorded there).
#[allow(deprecated)]
fn programdata_address(program_id: &Pubkey) -> Pubkey {
    solana_program::bpf_loader_upgradeable::get_program_data_address(program_id)
}

/// Seed a ProgramData account naming `authority` as `program_id`'s
/// upgrade authority (the loader's bincode layout, constructed
/// directly — native program-test has no real loader state).
#[allow(deprecated)]
fn seed_programdata(test: &mut ProgramTest, program_id: &Pubkey, authority: &Pubkey) {
    let mut data = vec![0u8; 45];
    data[0..4].copy_from_slice(&3u32.to_le_bytes());
    data[12] = 1;
    data[13..45].copy_from_slice(authority.as_ref());
    test.add_account(
        programdata_address(program_id),
        solana_sdk::account::Account {
            lamports: 10_000_000,
            data,
            owner: solana_program::bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
}

async fn fund_deployer(ctx: &mut ProgramTestContext, deployer: &Pubkey) {
    let tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &ctx.payer.pubkey(),
            deployer,
            1_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

async fn create_mint(
    ctx: &mut ProgramTestContext,
    mint_authority: &Pubkey,
    freeze_authority: Option<&Pubkey>,
) -> Pubkey {
    let mint = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            system_instruction::create_account(
                &ctx.payer.pubkey(),
                &mint.pubkey(),
                rent.minimum_balance(Mint::LEN),
                Mint::LEN as u64,
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
        ctx.last_blockhash,
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
    let tx = Transaction::new_signed_with_payer(
        &[
            system_instruction::create_account(
                &ctx.payer.pubkey(),
                &account.pubkey(),
                rent.minimum_balance(TokenAccount::LEN),
                TokenAccount::LEN as u64,
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
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    account.pubkey()
}

async fn token_balance(ctx: &mut ProgramTestContext, token_account: &Pubkey) -> u64 {
    let acc = ctx
        .banks_client
        .get_account(*token_account)
        .await
        .unwrap()
        .unwrap();
    let state = TokenAccount::unpack_from_slice(&acc.data).unwrap();
    state.amount
}

async fn read_event(
    ctx: &mut ProgramTestContext,
    program_id: &Pubkey,
    direction: u8,
    nonce: u64,
) -> EventRecord {
    let (pda, _) = nonce_pda(program_id, direction, nonce);
    let acc = ctx
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("event record PDA must exist");
    EventRecord::try_from_slice(&acc.data).unwrap()
}

fn to_sdk(ixs: &[solana_program::instruction::Instruction]) -> Vec<Instruction> {
    ixs.iter()
        .map(|ix| Instruction {
            program_id: ix.program_id,
            accounts: ix
                .accounts
                .iter()
                .map(|a| AccountMeta {
                    pubkey: a.pubkey,
                    is_signer: a.is_signer,
                    is_writable: a.is_writable,
                })
                .collect(),
            data: ix.data.clone(),
        })
        .collect()
}

async fn submit(
    ctx: &mut ProgramTestContext,
    ixs: &[solana_program::instruction::Instruction],
) -> Result<(), solana_program_test::BanksClientError> {
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &to_sdk(ixs),
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

/// Native (builtin) wrapper for the SPL Token program, as a fn
/// pointer (same body the `processor!` macro generates; see
/// full_drill.rs for why the macro cannot be used here).
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

struct Chains {
    sol: ProgramTestContext,
    machine: ProgramTestContext,
    signers: Vec<RelayerSigner>,
    relayer_set: [Pubkey; 5],
    user: Keypair,
    user_token_account: Pubkey,
    vault: Pubkey,
    source_mint: Pubkey,
    recipient: Keypair,
    recipient_token_account: Pubkey,
    wrapped_mint: Pubkey,
    collector: Collector,
}

async fn setup_chains() -> Chains {
    // Five relayers: one identity as dalek signer (relayer) and as
    // solana keypair (program config), as in the drill.
    let mut signers = Vec::new();
    let mut relayer_pubs = Vec::new();
    for i in 0..5u8 {
        let dalek = ed25519_dalek::SigningKey::from_bytes(&[10 + i; 32]);
        signers.push(RelayerSigner::from_seed(&dalek.to_bytes()));
        relayer_pubs.push(Pubkey::new_from_array(dalek.verifying_key().to_bytes()));
    }
    let relayers: [Pubkey; 5] = relayer_pubs.try_into().unwrap();
    let admins: Vec<Keypair> = (0..3).map(|_| Keypair::new()).collect();
    let admin_pubs: [Pubkey; 3] = [admins[0].pubkey(), admins[1].pubkey(), admins[2].pubkey()];
    let deployer = Keypair::new();

    // --- Solana side: lock program ---
    let lock_pid = lock_program_id();
    let mut sol_test = ProgramTest::new(
        "solana_lock",
        lock_pid,
        processor!(solana_lock::processor::process),
    );
    sol_test.add_builtin_program("spl_token", spl_token::id(), spl_token_builtin);
    seed_programdata(&mut sol_test, &lock_pid, &deployer.pubkey());
    let mut sol = sol_test.start_with_context().await;
    fund_deployer(&mut sol, &deployer.pubkey()).await;
    let user = Keypair::new();
    let mint_authority = Keypair::new();
    let source_mint = create_mint(&mut sol, &mint_authority.pubkey(), None).await;
    let user_token_account = create_token_account(&mut sol, &source_mint, &user.pubkey()).await;
    let (lock_authority, _) = authority_pda(&lock_pid);
    let vault = create_token_account(&mut sol, &source_mint, &lock_authority).await;
    let blockhash = sol.banks_client.get_latest_blockhash().await.unwrap();
    let mint_tx = Transaction::new_signed_with_payer(
        &[spl_token::instruction::mint_to(
            &spl_token::id(),
            &source_mint,
            &user_token_account,
            &mint_authority.pubkey(),
            &[],
            20_000_000,
        )
        .unwrap()],
        Some(&sol.payer.pubkey()),
        &[&sol.payer, &mint_authority],
        blockhash,
    );
    sol.banks_client.process_transaction(mint_tx).await.unwrap();
    let (lock_config, _) = config_pda(&lock_pid);
    let init_lock = Instruction::new_with_borsh(
        lock_pid,
        &LockInstruction::Initialize {
            relayers,
            admins: admin_pubs,
            source_mint,
            vault,
            per_tx_cap: 10_000_000,
            daily_cap: 25_000_000,
            vault_ceiling: 50_000_000,
        },
        vec![
            AccountMeta::new(deployer.pubkey(), true),
            AccountMeta::new(lock_config, false),
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(programdata_address(&lock_pid), false),
        ],
    );
    let blockhash = sol.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[init_lock],
        Some(&sol.payer.pubkey()),
        &[&sol.payer, &deployer],
        blockhash,
    );
    sol.banks_client.process_transaction(tx).await.unwrap();

    // --- Machine side: bridge program ---
    let machine_pid = machine_program_id();
    let mut machine_test = ProgramTest::new(
        "machine_bridge",
        machine_pid,
        processor!(machine_bridge::processor::process),
    );
    machine_test.add_builtin_program("spl_token", spl_token::id(), spl_token_builtin);
    seed_programdata(&mut machine_test, &machine_pid, &deployer.pubkey());
    let mut machine = machine_test.start_with_context().await;
    fund_deployer(&mut machine, &deployer.pubkey()).await;
    let recipient = Keypair::new();
    let (bridge_authority, _) = authority_pda(&machine_pid);
    let wrapped_mint = create_mint(&mut machine, &bridge_authority, Some(&bridge_authority)).await;
    let recipient_token_account =
        create_token_account(&mut machine, &wrapped_mint, &recipient.pubkey()).await;
    let (machine_config, _) = config_pda(&machine_pid);
    let init_machine = Instruction::new_with_borsh(
        machine_pid,
        &MachineInstruction::Initialize {
            relayers,
            admins: admin_pubs,
            wrapped_mint,
            source_mint,
            per_tx_cap: 10_000_000,
            daily_cap: 25_000_000,
        },
        vec![
            AccountMeta::new(deployer.pubkey(), true),
            AccountMeta::new(machine_config, false),
            AccountMeta::new_readonly(wrapped_mint, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(programdata_address(&machine_pid), false),
        ],
    );
    let blockhash = machine.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[init_machine],
        Some(&machine.payer.pubkey()),
        &[&machine.payer, &deployer],
        blockhash,
    );
    machine.banks_client.process_transaction(tx).await.unwrap();

    Chains {
        sol,
        machine,
        signers,
        relayer_set: relayers,
        user,
        user_token_account,
        vault,
        source_mint,
        recipient,
        recipient_token_account,
        wrapped_mint,
        collector: Collector {
            relayers,
            quorum: QUORUM,
        },
    }
}

async fn deposit(chains: &mut Chains, nonce: u64, amount: u64, destination: Pubkey) {
    let lock_pid = lock_program_id();
    let (config, _) = config_pda(&lock_pid);
    let (record, _) = nonce_pda(&lock_pid, DIR_SOLANA_TO_MACHINE, nonce);
    let blockhash = chains.sol.banks_client.get_latest_blockhash().await.unwrap();
    let ix = Instruction::new_with_borsh(
        lock_pid,
        &LockInstruction::Deposit {
            amount,
            destination,
            nonce,
        },
        vec![
            AccountMeta::new(config, false),
            AccountMeta::new(chains.user_token_account, false),
            AccountMeta::new(chains.vault, false),
            AccountMeta::new(record, false),
            AccountMeta::new_readonly(chains.user.pubkey(), true),
            AccountMeta::new(chains.sol.payer.pubkey(), true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&chains.sol.payer.pubkey()),
        &[&chains.sol.payer, &chains.user],
        blockhash,
    );
    chains.sol.banks_client.process_transaction(tx).await.unwrap();
}

fn attest(
    dir: &PathBuf,
    direction: Direction,
    message: &bridge_common::BridgeMessage,
    signers: &[RelayerSigner],
    indices: &[usize],
) {
    let _ = std::fs::remove_dir_all(dir);
    for &i in indices {
        AttestationFile::new(direction, message, &signers[i].sign(message))
            .write_to(dir)
            .unwrap();
    }
}

fn run_cli_verify(bundle_path: &PathBuf) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_crossing-proof"))
        .arg("verify")
        .arg(bundle_path)
        .output()
        .expect("crossing-proof binary runs")
}

#[tokio::test]
async fn real_crossing_exports_a_bundle_that_verifies() {
    let mut chains = setup_chains().await;
    let lock_pid = lock_program_id();
    let machine_pid = machine_program_id();
    let machine_payer = chains.machine.payer.pubkey();

    // --- The crossing itself (drill phase A shape) ---
    let recipient_pub = chains.recipient.pubkey();
    deposit(&mut chains, 1, 5_000_000, recipient_pub).await;
    assert_eq!(token_balance(&mut chains.sol, &chains.vault).await, 5_000_000);
    let record = read_event(&mut chains.sol, &lock_pid, DIR_SOLANA_TO_MACHINE, 1).await;
    let event = ObservedEvent {
        record,
        sender: chains.user.pubkey(),
        source_tx: [7u8; 64],
    };
    let message = build_message(
        Direction::SolanaToMachine,
        &event,
        chains.source_mint,
        EXPIRY_WINDOW,
    )
    .unwrap();
    let dir = std::env::temp_dir().join(format!("proof-bundle-e2e-{}", std::process::id()));
    attest(&dir, Direction::SolanaToMachine, &message, &chains.signers, &[0, 1, 2]);
    let (collected, sigs) = chains
        .collector
        .collect(&dir, Direction::SolanaToMachine, 1, 50_000)
        .unwrap();
    assert_eq!(collected, message);
    let mint_ixs = build_mint_instructions(
        &machine_pid,
        &collected,
        sigs,
        &chains.wrapped_mint,
        &chains.recipient_token_account,
        &machine_payer,
    );
    submit(&mut chains.machine, &mint_ixs).await.unwrap();
    assert_eq!(
        token_balance(&mut chains.machine, &chains.recipient_token_account).await,
        5_000_000
    );

    // --- The receipt the program wrote for this crossing ---
    let (receipt_addr, _) = receipt_pda(&machine_pid, DIR_SOLANA_TO_MACHINE, 1);
    let acc = chains
        .machine
        .banks_client
        .get_account(receipt_addr)
        .await
        .unwrap()
        .expect("MintWrapped must have written a crossing receipt");
    let receipt = CrossingReceipt::try_from_slice(&acc.data).unwrap();
    assert_eq!(receipt.message_hash, message.hash());
    assert_eq!(receipt.amount, 5_000_000);
    assert_eq!(receipt.recipient, recipient_pub);

    // --- Export: collector + receipt -> bundle ---
    let bundle = export_bundle(
        &dir,
        Direction::SolanaToMachine,
        1,
        &chains.relayer_set,
        QUORUM,
        &receipt,
    )
    .unwrap();
    let report = verify_bundle(&bundle).unwrap();
    assert_eq!(report.amount, 5_000_000);
    assert_eq!(report.distinct_signers, 3);
    assert_eq!(report.message_hash, message.hash());

    // The receipt file the CLI consumes is exactly the on-chain
    // receipt's fields.
    let receipt_file = ReceiptFile::from(&receipt);
    assert_eq!(receipt_file.to_receipt().unwrap(), receipt);

    // --- The shipped binary verifies the exported file ---
    let bundle_path = dir.join("bundle.json");
    std::fs::write(&bundle_path, serde_json::to_string_pretty(&bundle).unwrap()).unwrap();
    let out = run_cli_verify(&bundle_path);
    assert!(
        out.status.success(),
        "CLI must accept the real bundle: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("VALID crossing proof"));

    // --- Tampered copies are rejected, in-process and by the CLI ---
    let mut tampered = bundle.clone();
    tampered.receipt.amount += 1;
    assert_eq!(
        verify_bundle(&tampered),
        Err(BundleError::ReceiptMismatch { field: "amount" })
    );
    let tampered_path = dir.join("bundle-tampered.json");
    std::fs::write(&tampered_path, serde_json::to_string_pretty(&tampered).unwrap()).unwrap();
    let out = run_cli_verify(&tampered_path);
    assert!(!out.status.success(), "CLI must reject a tampered bundle");
    assert!(String::from_utf8_lossy(&out.stderr).contains("receipt amount"));

    let mut short = bundle.clone();
    short.attestations.truncate(2);
    assert_eq!(
        verify_bundle(&short),
        Err(BundleError::Attestation(bridge_common::BridgeError::QuorumNotReached))
    );

    let _ = std::fs::remove_dir_all(&dir);
}
