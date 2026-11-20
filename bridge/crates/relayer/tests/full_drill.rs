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

const EXPIRY_WINDOW: i64 = 40_000;
/// Fixed source-event block time for the drill's messages (A-F1
/// fix, 2026-11-20: expiry is timestamp-based), and the destination
/// "now" both drill chains are pinned to. The program-test harness
/// clock starts at wall-clock time, years past any fixed fixture
/// window, so both contexts are pinned in `setup_chains`.
const TEST_SOURCE_TS: i64 = 1_700_000_000;
const DEST_NOW: i64 = TEST_SOURCE_TS + 60;

/// Set a context's Clock unix_timestamp (slot untouched) and prove
/// it landed by reading the sysvar back.
async fn set_clock_ts(ctx: &mut ProgramTestContext, ts: i64) {
    let mut clock: solana_program::clock::Clock =
        ctx.banks_client.get_sysvar().await.unwrap();
    clock.unix_timestamp = ts;
    ctx.set_sysvar(&clock);
    let readback: solana_program::clock::Clock =
        ctx.banks_client.get_sysvar().await.unwrap();
    assert_eq!(
        readback.unix_timestamp, ts,
        "harness clock probe: set_sysvar did not take effect"
    );
}

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
/// upgrade authority. The bytes are the loader's bincode layout —
/// variant tag 3 (ProgramData), slot, option tag, authority — the
/// layout bridge-common parses. Native program-test has no real
/// loader state for a `processor!` program, so the account is
/// constructed directly.
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

/// Fund `deployer` (the upgrade authority for both programs in
/// this drill) from the context payer.
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

/// Native (builtin) wrapper for the SPL Token program, as a fn pointer.
///
/// `ProgramTest::add_builtin_program` takes a bare fn pointer, while the
/// `processor!` macro yields an `Option<closure>` whose lifetime is not
/// higher-ranked, so the macro cannot be used here directly. This is the
/// same body the macro generates.
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

    // One deployer is the upgrade authority of both programs;
    // both Initialize calls are bound to it (self-audit F-1).
    let deployer = Keypair::new();

    // --- Solana side: lock program ---
    let lock_pid = lock_program_id();
    let mut sol_test = ProgramTest::new(
        "solana_lock",
        lock_pid,
        processor!(solana_lock::processor::process),
    );
    // SPL Token is a builtin (native) program on-chain, so register it as a
    // builtin: `add_program` would panic in SBF mode (BPF_OUT_DIR set) because
    // no spl_token.so exists to load.
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
    // SPL Token is a builtin (native) program on-chain, so register it as a
    // builtin: `add_program` would panic in SBF mode (BPF_OUT_DIR set) because
    // no spl_token.so exists to load.
    machine_test.add_builtin_program("spl_token", spl_token::id(), spl_token_builtin);
    seed_programdata(&mut machine_test, &machine_pid, &deployer.pubkey());
    let mut machine = machine_test.start_with_context().await;
    fund_deployer(&mut machine, &deployer.pubkey()).await;
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

    // Pin both chains' clocks to the drill's destination "now" so
    // attestation expiry checks run against the fixtures' window.
    set_clock_ts(&mut sol, DEST_NOW).await;
    set_clock_ts(&mut machine, DEST_NOW).await;

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
        source_block_time: TEST_SOURCE_TS,
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
        .collect(&dir_a, Direction::SolanaToMachine, 1, DEST_NOW)
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
        source_block_time: TEST_SOURCE_TS,
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
        .collect(&dir_b, Direction::SolanaToMachine, 2, DEST_NOW)
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
        source_block_time: TEST_SOURCE_TS,
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
        .collect(&dir_c, Direction::MachineToSolana, 1, DEST_NOW)
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
        source_block_time: TEST_SOURCE_TS,
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
        .collect(&dir_d, Direction::SolanaToMachine, 3, DEST_NOW);
    assert!(
        result.is_err(),
        "collector must refuse quorum on disagreement, got: {result:?}"
    );
    assert_eq!(
        token_balance(&mut chains.machine, &chains.recipient_token_account).await,
        6_950_000
    );
    println!("drill D ok: disagreement blocked quorum; nothing minted");

    write_drill_result(
        "full_local_drill",
        &[
            "crossings succeeded: 3 (deposit->mint x2, burn->unlock x1)".to_string(),
            "crossings blocked as designed: 2 (mint while paused, relayer disagreement)".to_string(),
            "crossings failed unexpectedly: 0".to_string(),
            "final state: vault 6,950,000 locked == 6,950,000 wrapped outstanding".to_string(),
        ],
    );

    for d in [dir_a, dir_b, dir_c, dir_d] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Record a drill outcome in bridge/drills/last-drill.md so the
/// repo always shows the most recent drill's numbers (the ops
/// metrics habit: slots/crossings are only claimed with a run
/// behind them). Last writer wins; the header says which drill ran.
fn write_drill_result(drill: &str, lines: &[String]) {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut out = String::from("# Last local drill result\n\n");
    out.push_str(
        "Written by the drill test itself at the end of a passing run.\n\
         Program-test localnet only — no live chain is involved.\n\n",
    );
    out.push_str(&format!("- drill: `{drill}`\n- unix time: {unix}\n"));
    for line in lines {
        out.push_str(&format!("- {line}\n"));
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drills/last-drill.md");
    let _ = std::fs::write(path, out);
}

/// One full Solana -> Machine crossing through the file-based
/// attestation exchange, exactly as a relayer round would do it.
async fn cross_to_machine(chains: &mut Chains, nonce: u64, amount: u64) {
    let lock_pid = lock_program_id();
    let machine_pid = machine_program_id();
    let machine_payer = chains.machine.payer.pubkey();
    deposit(chains, nonce, amount, chains.recipient.pubkey()).await;
    let record = read_event(&mut chains.sol, &lock_pid, DIR_SOLANA_TO_MACHINE, nonce).await;
    let event = ObservedEvent {
        record,
        sender: chains.user.pubkey(),
        source_tx: [nonce as u8; 64],
        source_block_time: TEST_SOURCE_TS,
    };
    let message = build_message(
        Direction::SolanaToMachine,
        &event,
        chains.source_mint,
        EXPIRY_WINDOW,
    )
    .unwrap();
    let dir = drill_dir(&format!("vm{nonce}"));
    attest(&dir, Direction::SolanaToMachine, &message, &chains.signers, &[0, 1, 2]);
    let (collected, sigs) = chains
        .collector
        .collect(&dir, Direction::SolanaToMachine, nonce, DEST_NOW)
        .unwrap();
    let mint_ixs = build_mint_instructions(
        &machine_pid,
        &collected,
        sigs,
        &chains.wrapped_mint,
        &chains.recipient_token_account,
        &machine_payer,
    );
    submit(&mut chains.machine, &mint_ixs).await.unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// One full Machine -> Solana crossing (burn -> attest -> unlock).
async fn cross_to_solana(chains: &mut Chains, nonce: u64, amount: u64) {
    let lock_pid = lock_program_id();
    let machine_pid = machine_program_id();
    let machine_payer = chains.machine.payer.pubkey();
    let sol_payer = chains.sol.payer.pubkey();
    let (machine_config, _) = config_pda(&machine_pid);
    let (burn_record_pda, _) = nonce_pda(&machine_pid, DIR_MACHINE_TO_SOLANA, nonce);
    let blockhash = chains.machine.banks_client.get_latest_blockhash().await.unwrap();
    let burn_ix = Instruction::new_with_borsh(
        machine_pid,
        &MachineInstruction::Burn {
            amount,
            destination: chains.user.pubkey(),
            nonce,
        },
        vec![
            AccountMeta::new(machine_config, false),
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
    let record = read_event(&mut chains.machine, &machine_pid, DIR_MACHINE_TO_SOLANA, nonce).await;
    let event = ObservedEvent {
        record,
        sender: chains.recipient.pubkey(),
        source_tx: [100 + nonce as u8; 64],
        source_block_time: TEST_SOURCE_TS,
    };
    let message = build_message(
        Direction::MachineToSolana,
        &event,
        chains.source_mint,
        EXPIRY_WINDOW,
    )
    .unwrap();
    let dir = drill_dir(&format!("vs{nonce}"));
    attest(&dir, Direction::MachineToSolana, &message, &chains.signers, &[0, 1, 2]);
    let (collected, sigs) = chains
        .collector
        .collect(&dir, Direction::MachineToSolana, nonce, DEST_NOW)
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
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn volume_drill_many_crossings_reconcile() {
    // The soak item's local slice: volume through the real programs
    // and the real relayer path, ending in the reconciliation
    // invariant (locked == wrapped outstanding). Unlocks are sized
    // to respect the dynamic caps — 1% of the live locked total per
    // transaction, 5% per window — which is why the return leg is
    // smaller than the outbound leg in a single window.
    let mut chains = setup_chains().await;
    const DEPOSITS: u64 = 40;
    const DEPOSIT_AMOUNT: u64 = 250_000;
    const UNLOCKS: u64 = 5;
    const UNLOCK_AMOUNT: u64 = 90_000;

    for nonce in 1..=DEPOSITS {
        cross_to_machine(&mut chains, nonce, DEPOSIT_AMOUNT).await;
    }
    let locked = DEPOSITS * DEPOSIT_AMOUNT;
    assert_eq!(token_balance(&mut chains.sol, &chains.vault).await, locked);
    assert_eq!(
        token_balance(&mut chains.machine, &chains.recipient_token_account).await,
        locked
    );

    for nonce in 1..=UNLOCKS {
        cross_to_solana(&mut chains, nonce, UNLOCK_AMOUNT).await;
    }
    let returned = UNLOCKS * UNLOCK_AMOUNT;
    let vault_final = token_balance(&mut chains.sol, &chains.vault).await;
    let wrapped_final =
        token_balance(&mut chains.machine, &chains.recipient_token_account).await;
    // Reconciliation: every wrapped token is backed, 1:1, always.
    assert_eq!(vault_final, locked - returned);
    assert_eq!(wrapped_final, vault_final);
    assert_eq!(
        token_balance(&mut chains.sol, &chains.user_token_account).await,
        20_000_000 - locked + returned
    );
    println!(
        "volume drill ok: {DEPOSITS} crossings in, {UNLOCKS} back, reconciled at {vault_final}"
    );
    write_drill_result(
        "volume_drill_many_crossings_reconcile",
        &[
            format!("crossings succeeded: {} ({} in, {} back)", DEPOSITS + UNLOCKS, DEPOSITS, UNLOCKS),
            "crossings failed unexpectedly: 0".to_string(),
            format!("final state: vault {vault_final} locked == {wrapped_final} wrapped outstanding"),
        ],
    );
}
