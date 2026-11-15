//! Full local end-to-end drill (bridge/drills/LOCAL-DRILL.md):
//! both crossing directions executed against the real programs in
//! program-test, with the relayer's actual file-exchange transport
//! and collector in the middle — plus a pause forced mid-flow and a
//! relayer disagreement. No network, no deployed programs, no real
//! assets: two local banks stand in for the two chains.
//!
//! Flow:
//!   A. Deposit on the Solana side -> attestation files -> quorum ->
//!      MintWrapped on the Machine side.
//!   B. Second deposit -> quorum -> Machine bridge paused -> mint
//!      rejected, nothing moves -> unpaused -> mint succeeds.
//!   C. Burn on the Machine side -> attestation files -> quorum ->
//!      Unlock on the Solana side.
//!   D. Third deposit -> relayers disagree on the message -> the
//!      collector refuses quorum -> nothing mints.

use borsh::BorshDeserialize;
use bridge_common::{
    authority_pda, config_pda, nonce_pda, BridgeError, BridgeMessage, EventRecord,
    LockInstruction, MachineInstruction, DIR_MACHINE_TO_SOLANA,
    DIR_SOLANA_TO_MACHINE, QUORUM,
};
use relayer::attestation::{AttestationFile, Collector};
use relayer::message::{build_message, Direction, ObservedEvent};
use relayer::signing::RelayerSigner;
use relayer::submit::{build_mint_instructions, build_unlock_instructions};
use solana_program_test::{processor, ProgramTest, ProgramTestContext};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use solana_program::program_pack::Pack;
use spl_token::state::{Account as TokenAccount, Mint};
use std::path::PathBuf;

const EXPIRY_WINDOW: u64 = 100_000;
const DEST_SLOT: u64 = 50_000;

fn machine_program_id() -> Pubkey {
    Pubkey::new_from_array([42u8; 32])
}
fn lock_program_id() -> Pubkey {
    Pubkey::new_from_array([43u8; 32])
}

// ---------------------------------------------------------------------------
// Setup helpers (mirroring the program suites)
// ---------------------------------------------------------------------------

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

fn attest(
    dir: &PathBuf,
    direction: Direction,
    message: &BridgeMessage,
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

struct Chains {
    sol: ProgramTestContext,
    machine: ProgramTestContext,
    signers: Vec<RelayerSigner>,
    relayer_keys: Vec<Keypair>,
    admins: Vec<Keypair>,
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
    // solana keypair (program config + pause signing).
    let mut signers = Vec::new();
    let mut relayer_keys = Vec::new();
    let mut relayer_pubs = Vec::new();
    for i in 0..5u8 {
        let dalek = ed25519_dalek::SigningKey::from_bytes(&[10 + i; 32]);
        signers.push(RelayerSigner::from_seed(&dalek.to_bytes()));
        relayer_keys.push(Keypair::try_from(dalek.to_keypair_bytes().as_slice()).unwrap());
        relayer_pubs.push(Pubkey::new_from_array(dalek.verifying_key().to_bytes()));
    }
    let relayers: [Pubkey; 5] = relayer_pubs.try_into().unwrap();
    let admins: Vec<Keypair> = (0..3).map(|_| Keypair::new()).collect();
    let admin_pubs: [Pubkey; 3] = [
        admins[0].pubkey(),
        admins[1].pubkey(),
        admins[2].pubkey(),
    ];

    // --- Solana side: lock program ---
    let lock_pid = lock_program_id();
    let mut sol_test = ProgramTest::new(
        "solana_lock",
        lock_pid,
        processor!(solana_lock::processor::process),
    );
    sol_test.add_program(
        "spl_token",
        spl_token::id(),
        processor!(spl_token::processor::Processor::process),
    );
    let mut sol = sol_test.start_with_context().await;
    let user = Keypair::new();
    let mint_authority = Keypair::new();
    let source_mint = create_mint(&mut sol, &mint_authority.pubkey(), None).await;
    let user_token_account = create_token_account(&mut sol, &source_mint, &user.pubkey()).await;
    let (lock_authority, _) = authority_pda(&lock_pid);
    let vault = create_token_account(&mut sol, &source_mint, &lock_authority).await;
    // Fund the user with 20,000,000 source tokens.
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
            AccountMeta::new(sol.payer.pubkey(), true),
            AccountMeta::new(lock_config, false),
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let blockhash = sol.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[init_lock],
        Some(&sol.payer.pubkey()),
        &[&sol.payer],
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
    machine_test.add_program(
        "spl_token",
        spl_token::id(),
        processor!(spl_token::processor::Processor::process),
    );
    let mut machine = machine_test.start_with_context().await;
    let recipient = Keypair::new();
    let (bridge_authority, _) = authority_pda(&machine_pid);
    let wrapped_mint = create_mint(
        &mut machine,
        &bridge_authority,
        Some(&bridge_authority),
    )
    .await;
    let recipient_token_account =
        create_token_account(&mut machine, &wrapped_mint, &recipient.pubkey()).await;
    let (machine_config, _) = config_pda(&machine_pid);
    let init_machine = Instruction::new_with_borsh(
        machine_pid,
        &MachineInstruction::Initialize {
            relayers,
            admins: admin_pubs,
            wrapped_mint,
            per_tx_cap: 10_000_000,
            daily_cap: 25_000_000,
        },
        vec![
            AccountMeta::new(machine.payer.pubkey(), true),
            AccountMeta::new(machine_config, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let blockhash = machine.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[init_machine],
        Some(&machine.payer.pubkey()),
        &[&machine.payer],
        blockhash,
    );
    machine.banks_client.process_transaction(tx).await.unwrap();

    Chains {
        sol,
        machine,
        signers,
        relayer_keys,
        admins,
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

fn drill_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("relayer-drill-{}-{}", name, std::process::id()))
}

#[tokio::test]
async fn full_local_drill() {
    let mut chains = setup_chains().await;
    let lock_pid = lock_program_id();
    let machine_pid = machine_program_id();
    let machine_payer = chains.machine.payer.pubkey();
    let sol_payer = chains.sol.payer.pubkey();

    // ---------------- Phase A: deposit -> attest -> mint ----------
    let recipient_pub = chains.recipient.pubkey();
    deposit(&mut chains, 1, 5_000_000, recipient_pub).await;
    assert_eq!(token_balance(&mut chains.sol, &chains.vault).await, 5_000_000);
    assert_eq!(
        token_balance(&mut chains.sol, &chains.user_token_account).await,
        15_000_000
    );
    let record = read_event(&mut chains.sol, &lock_pid, DIR_SOLANA_TO_MACHINE, 1).await;
    assert_eq!(record.amount, 5_000_000);
    assert_eq!(record.destination, chains.recipient.pubkey());
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
    let dir_a = drill_dir("a");
    attest(&dir_a, Direction::SolanaToMachine, &message, &chains.signers, &[0, 1, 2]);
    let (collected, sigs) = chains
        .collector
        .collect(&dir_a, Direction::SolanaToMachine, 1, DEST_SLOT)
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
    println!("drill A ok: deposit 5,000,000 -> minted 5,000,000 wrapped");

    // ---------------- Phase B: pause mid-flow ---------------------
    deposit(&mut chains, 2, 2_000_000, recipient_pub).await;
    let record = read_event(&mut chains.sol, &lock_pid, DIR_SOLANA_TO_MACHINE, 2).await;
    let event = ObservedEvent {
        record,
        sender: chains.user.pubkey(),
        source_tx: [8u8; 64],
    };
    let message = build_message(
        Direction::SolanaToMachine,
        &event,
        chains.source_mint,
        EXPIRY_WINDOW,
    )
    .unwrap();
    let dir_b = drill_dir("b");
    attest(&dir_b, Direction::SolanaToMachine, &message, &chains.signers, &[0, 1, 2]);
    let (collected, sigs) = chains
        .collector
        .collect(&dir_b, Direction::SolanaToMachine, 2, DEST_SLOT)
        .unwrap();
    let mint_ixs = build_mint_instructions(
        &machine_pid,
        &collected,
        sigs,
        &chains.wrapped_mint,
        &chains.recipient_token_account,
        &machine_payer,
    );
    // Pause the Machine bridge with 2 relayer signers.
    let (machine_config, _) = config_pda(&machine_pid);
    let blockhash = chains.machine.banks_client.get_latest_blockhash().await.unwrap();
    let pause_ix = Instruction::new_with_borsh(
        machine_pid,
        &MachineInstruction::Pause,
        vec![
            AccountMeta::new(machine_config, false),
            AccountMeta::new_readonly(chains.relayer_keys[0].pubkey(), true),
            AccountMeta::new_readonly(chains.relayer_keys[1].pubkey(), true),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&machine_payer),
        &[
            &chains.machine.payer,
            &chains.relayer_keys[0],
            &chains.relayer_keys[1],
        ],
        blockhash,
    );
    chains.machine.banks_client.process_transaction(tx).await.unwrap();
    // The mint must now fail with BridgePaused and nothing may move.
    let result = submit(&mut chains.machine, &mint_ixs).await;
    let err = format!("{:?}", result.expect_err("mint must fail while paused"));
    assert!(
        err.contains(&format!("Custom({})", BridgeError::BridgePaused as u32)),
        "expected BridgePaused, got: {err}"
    );
    assert_eq!(
        token_balance(&mut chains.machine, &chains.recipient_token_account).await,
        5_000_000
    );
    // Unpause with 2 admins and resubmit: the crossing completes.
    let blockhash = chains.machine.banks_client.get_latest_blockhash().await.unwrap();
    let unpause_ix = Instruction::new_with_borsh(
        machine_pid,
        &MachineInstruction::Unpause,
        vec![
            AccountMeta::new(machine_config, false),
            AccountMeta::new_readonly(chains.admins[0].pubkey(), true),
            AccountMeta::new_readonly(chains.admins[1].pubkey(), true),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&machine_payer),
        &[&chains.machine.payer, &chains.admins[0], &chains.admins[1]],
        blockhash,
    );
    chains.machine.banks_client.process_transaction(tx).await.unwrap();
    // Advance the slot before resubmitting the identical mint
    // transaction: program-test reuses the blockhash within a slot,
    // and the bank answers a duplicate signature with the cached
    // (failed) result instead of re-executing.
    chains.machine.warp_to_slot(200).unwrap();
    submit(&mut chains.machine, &mint_ixs).await.unwrap();
    assert_eq!(
        token_balance(&mut chains.machine, &chains.recipient_token_account).await,
        7_000_000
    );
    println!("drill B ok: pause blocked the mint (BridgePaused), unpause released it");

    // ---------------- Phase C: burn -> attest -> unlock -----------
    let (machine_config2, _) = config_pda(&machine_pid);
    let (burn_record_pda, _) = nonce_pda(&machine_pid, DIR_MACHINE_TO_SOLANA, 1);
    let blockhash = chains.machine.banks_client.get_latest_blockhash().await.unwrap();
    let burn_ix = Instruction::new_with_borsh(
        machine_pid,
        &MachineInstruction::Burn {
            amount: 50_000,
            destination: chains.user.pubkey(),
            nonce: 1,
        },
        vec![
            AccountMeta::new(machine_config2, false),
            AccountMeta::new(chains.recipient_token_account, false),
            AccountMeta::new(chains.wrapped_mint, false),
            AccountMeta::new(burn_record_pda, false),
            AccountMeta::new_readonly(chains.recipient.pubkey(), true),
            AccountMeta::new(machine_payer, true),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
    );
    let tx = Transaction::new_signed_with_payer(
        &[burn_ix],
        Some(&machine_payer),
        &[&chains.machine.payer, &chains.recipient],
        blockhash,
    );
    chains.machine.banks_client.process_transaction(tx).await.unwrap();
    assert_eq!(
        token_balance(&mut chains.machine, &chains.recipient_token_account).await,
        6_950_000
    );
    let record = read_event(&mut chains.machine, &machine_pid, DIR_MACHINE_TO_SOLANA, 1).await;
    assert_eq!(record.amount, 50_000);
    let event = ObservedEvent {
        record,
        sender: chains.recipient.pubkey(),
        source_tx: [9u8; 64],
    };
    let message = build_message(
        Direction::MachineToSolana,
        &event,
        chains.source_mint,
        EXPIRY_WINDOW,
    )
    .unwrap();
    let dir_c = drill_dir("c");
    attest(&dir_c, Direction::MachineToSolana, &message, &chains.signers, &[0, 1, 2]);
    let (collected, sigs) = chains
        .collector
        .collect(&dir_c, Direction::MachineToSolana, 1, DEST_SLOT)
        .unwrap();
    let unlock_ixs = build_unlock_instructions(
        &lock_pid,
        &collected,
        sigs,
        &chains.vault,
        &chains.user_token_account,
        &sol_payer,
    );
    submit(&mut chains.sol, &unlock_ixs).await.unwrap();
    assert_eq!(
        token_balance(&mut chains.sol, &chains.user_token_account).await,
        13_050_000
    );
    assert_eq!(token_balance(&mut chains.sol, &chains.vault).await, 6_950_000);
    println!("drill C ok: burn 50,000 -> unlocked 50,000 (within the dynamic caps)");

    // ---------------- Phase D: relayer disagreement ---------------
    deposit(&mut chains, 3, 10_000, recipient_pub).await;
    let record = read_event(&mut chains.sol, &lock_pid, DIR_SOLANA_TO_MACHINE, 3).await;
    let event = ObservedEvent {
        record,
        sender: chains.user.pubkey(),
        source_tx: [11u8; 64],
    };
    let message = build_message(
        Direction::SolanaToMachine,
        &event,
        chains.source_mint,
        EXPIRY_WINDOW,
    )
    .unwrap();
    let mut tampered = message.clone();
    tampered.amount += 1;
    let dir_d = drill_dir("d");
    let _ = std::fs::remove_dir_all(&dir_d);
    for i in [0usize, 1] {
        AttestationFile::new(Direction::SolanaToMachine, &message, &chains.signers[i].sign(&message))
            .write_to(&dir_d)
            .unwrap();
    }
    for i in [2usize, 3] {
        AttestationFile::new(
            Direction::SolanaToMachine,
            &tampered,
            &chains.signers[i].sign(&tampered),
        )
        .write_to(&dir_d)
        .unwrap();
    }
    // Two relayers attest one hash, two another: no quorum may form.
    let result = chains
        .collector
        .collect(&dir_d, Direction::SolanaToMachine, 3, DEST_SLOT);
    assert!(
        result.is_err(),
        "collector must refuse quorum on disagreement, got: {result:?}"
    );
    assert_eq!(
        token_balance(&mut chains.machine, &chains.recipient_token_account).await,
        6_950_000
    );
    println!("drill D ok: disagreement blocked quorum; nothing minted");

    for d in [dir_a, dir_b, dir_c, dir_d] {
        let _ = std::fs::remove_dir_all(d);
    }
}
