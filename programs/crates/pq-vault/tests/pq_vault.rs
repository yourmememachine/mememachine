//! Integration tests for PQ Vault, run against a real
//! program-test validator with the real SPL token program.
//! Each test maps to a numbered requirement in
//! programs/PQ-VAULT.md ("Required tests"); the mapping is in
//! programs/PQ-VERIFY.md.

use pq_vault::state::*;
use pq_vault::wots;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    system_instruction,
};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};
use spl_token::{
    instruction::{initialize_account3, initialize_mint2, mint_to, transfer},
    state::{Account as TokenAccount, Mint},
};

const SUPPLY: u64 = 1_000_000;
const CHUNK: usize = 700; // 4 chunks per 2,144-byte signature

fn program_id() -> Pubkey {
    Pubkey::new_from_array([80; 32])
}

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &program_id()).0
}

fn vault_pda(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    pda(&[SEED_VAULT, owner.as_ref(), mint.as_ref()])
}
fn token_vault_pda(vault: &Pubkey) -> Pubkey {
    pda(&[SEED_TOKEN_VAULT, vault.as_ref()])
}
fn authority_pda(vault: &Pubkey) -> Pubkey {
    pda(&[SEED_AUTHORITY, vault.as_ref()])
}
fn sig_pda(vault: &Pubkey, key_index: u32) -> Pubkey {
    pda(&[SEED_SIG, vault.as_ref(), &key_index.to_le_bytes()])
}

async fn start() -> ProgramTestContext {
    let test = ProgramTest::new(
        "pq_vault",
        program_id(),
        processor!(pq_vault::process_instruction),
    );
    test.start_with_context().await
}

async fn send(
    ctx: &mut ProgramTestContext,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> Result<(), BanksClientError> {
    let payer = ctx.payer.pubkey();
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer), &all, ctx.last_blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Warp forward and refresh the context blockhash. A
/// byte-identical transaction resubmitted under the same
/// blockhash can be answered from the bank's signature cache
/// instead of executing (the harness behavior LaunchGuard's
/// VERIFY.md records); several tests here deliberately
/// resubmit near-identical transactions — a replayed signature,
/// a second abort — so each new phase warps first, and the
/// refreshed blockhash gives its transactions fresh signatures.
async fn warp(ctx: &mut ProgramTestContext, slot: u64) {
    ctx.warp_to_slot(slot).unwrap();
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
}

fn expect_custom(err: &BanksClientError, code: u32) {
    let BanksClientError::TransactionError(TransactionError::InstructionError(
        _,
        solana_program::instruction::InstructionError::Custom(c),
    )) = err
    else {
        panic!("expected Custom({code}), got {err:?}");
    };
    assert_eq!(*c, code);
}

/// Create a mint and an owner token account holding SUPPLY.
/// Unlike LaunchGuard, PQ Vault does not care whether the
/// mint's authorities are revoked — it custodies whatever the
/// mint is; the mint authority stays with the owner so tests
/// can mint the initial supply.
async fn make_mint(ctx: &mut ProgramTestContext, owner: &Keypair) -> (Pubkey, Pubkey) {
    let mint = Keypair::new();
    let token_acct = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let fund = system_instruction::transfer(&ctx.payer.pubkey(), &owner.pubkey(), 5_000_000_000);
    let ixs = vec![
        fund,
        system_instruction::create_account(
            &owner.pubkey(),
            &mint.pubkey(),
            rent.minimum_balance(Mint::LEN),
            Mint::LEN as u64,
            &spl_token::id(),
        ),
        initialize_mint2(
            &spl_token::id(),
            &mint.pubkey(),
            &owner.pubkey(),
            Some(&owner.pubkey()),
            0,
        )
        .unwrap(),
        system_instruction::create_account(
            &owner.pubkey(),
            &token_acct.pubkey(),
            rent.minimum_balance(TokenAccount::LEN),
            TokenAccount::LEN as u64,
            &spl_token::id(),
        ),
        initialize_account3(
            &spl_token::id(),
            &token_acct.pubkey(),
            &mint.pubkey(),
            &owner.pubkey(),
        )
        .unwrap(),
        mint_to(
            &spl_token::id(),
            &mint.pubkey(),
            &token_acct.pubkey(),
            &owner.pubkey(),
            &[],
            SUPPLY,
        )
        .unwrap(),
    ];
    send(ctx, &ixs, &[owner, &mint, &token_acct])
        .await
        .expect("mint setup");
    (mint.pubkey(), token_acct.pubkey())
}

/// A plain token account of `mint` owned by a fresh keypair —
/// the withdrawal destination in most tests.
async fn make_token_account(
    ctx: &mut ProgramTestContext,
    payer: &Keypair,
    mint: &Pubkey,
) -> (Keypair, Pubkey) {
    let _ = payer; // the context payer funds the holder; kept for call-site symmetry
    let holder = Keypair::new();
    let token_acct = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let ixs = vec![
        system_instruction::transfer(&ctx.payer.pubkey(), &holder.pubkey(), 1_000_000_000),
        system_instruction::create_account(
            &holder.pubkey(),
            &token_acct.pubkey(),
            rent.minimum_balance(TokenAccount::LEN),
            TokenAccount::LEN as u64,
            &spl_token::id(),
        ),
        initialize_account3(&spl_token::id(), &token_acct.pubkey(), mint, &holder.pubkey())
            .unwrap(),
    ];
    send(ctx, &ixs, &[&holder, &token_acct])
        .await
        .expect("token account setup");
    (holder, token_acct.pubkey())
}

fn commitments(seed: &[u8; 32], count: u32) -> Vec<[u8; 32]> {
    (0..count).map(|i| wots::commitment(seed, i)).collect()
}

fn create_vault_ix(owner: &Pubkey, mint: &Pubkey, commitments: Vec<[u8; 32]>) -> Instruction {
    let vault = vault_pda(owner, mint);
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*owner, true),
            AccountMeta::new(vault, false),
            AccountMeta::new(token_vault_pda(&vault), false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(authority_pda(&vault), false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_program::system_program::id(), false),
        ],
        data: borsh::to_vec(&PqInstruction::CreateVault { commitments }).unwrap(),
    }
}

fn create_buffer_ix(payer: &Pubkey, vault: &Pubkey, key_index: u32, digest: [u8; 32]) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*vault, false),
            AccountMeta::new(sig_pda(vault, key_index), false),
            AccountMeta::new_readonly(solana_program::system_program::id(), false),
        ],
        data: borsh::to_vec(&PqInstruction::CreateSigBuffer { key_index, digest }).unwrap(),
    }
}

fn write_chunk_ix(vault: &Pubkey, key_index: u32, offset: u16, bytes: Vec<u8>) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![AccountMeta::new(sig_pda(vault, key_index), false)],
        data: borsh::to_vec(&PqInstruction::WriteSigChunk { offset, bytes }).unwrap(),
    }
}

fn execute_ix(
    payer: &Pubkey,
    vault: &Pubkey,
    _mint: &Pubkey,
    destination: &Pubkey,
    amount: u64,
    key_index: u32,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*vault, false),
            AccountMeta::new(token_vault_pda(vault), false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(authority_pda(vault), false),
            AccountMeta::new(sig_pda(vault, key_index), false),
            AccountMeta::new_readonly(spl_token::id(), false),
        ],
        data: borsh::to_vec(&PqInstruction::ExecuteWithdrawal { amount }).unwrap(),
    }
}

fn rotate_ix(
    payer: &Pubkey,
    vault: &Pubkey,
    key_index: u32,
    new_commitments: Vec<[u8; 32]>,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*vault, false),
            AccountMeta::new(sig_pda(vault, key_index), false),
        ],
        data: borsh::to_vec(&PqInstruction::RotateKeys {
            new_commitments,
        })
        .unwrap(),
    }
}

/// The compute-budget request every signature-verifying
/// transaction carries. Verification costs more than the
/// 200,000-unit default budget — measured 337,000–395,000
/// units against the SBF build (programs/PQ-VERIFY.md) — so
/// requesting headroom is part of submitting these
/// instructions, for a real client exactly as here. (Native
/// program-test execution meters almost nothing, which hides
/// this; the SBF run does not.)
fn budget_ix() -> Instruction {
    solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(1_000_000)
}

/// ExecuteWithdrawal as it is actually submitted: budget
/// request first, then the instruction.
fn execute_ixs(
    payer: &Pubkey,
    vault: &Pubkey,
    mint: &Pubkey,
    destination: &Pubkey,
    amount: u64,
    key_index: u32,
) -> Vec<Instruction> {
    vec![
        budget_ix(),
        execute_ix(payer, vault, mint, destination, amount, key_index),
    ]
}

/// RotateKeys as it is actually submitted: budget request
/// first, then the instruction.
fn rotate_ixs(
    payer: &Pubkey,
    vault: &Pubkey,
    key_index: u32,
    new_commitments: Vec<[u8; 32]>,
) -> Vec<Instruction> {
    vec![
        budget_ix(),
        rotate_ix(payer, vault, key_index, new_commitments),
    ]
}

fn abort_ix(owner: &Pubkey, vault: &Pubkey, key_index: u32) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*owner, true),
            AccountMeta::new_readonly(*vault, false),
            AccountMeta::new(sig_pda(vault, key_index), false),
        ],
        data: borsh::to_vec(&PqInstruction::AbortSigBuffer {}).unwrap(),
    }
}

async fn token_balance(ctx: &mut ProgramTestContext, acct: &Pubkey) -> u64 {
    let a = ctx.banks_client.get_account(*acct).await.unwrap().unwrap();
    TokenAccount::unpack(&a.data).unwrap().amount
}

async fn vault_state(ctx: &mut ProgramTestContext, vault: &Pubkey) -> VaultAccount {
    use borsh::BorshDeserialize;
    let a = ctx.banks_client.get_account(*vault).await.unwrap().unwrap();
    VaultAccount::try_from_slice(&a.data[..VaultAccount::LEN]).unwrap()
}

/// Create a vault with `keys` commitments from `seed`, deposit
/// `deposit` from the owner's token account. Returns the vault
/// PDA. The deposit is a plain SPL transfer — that is the point
/// of design test 2, exercised here on every setup.
async fn setup_vault(
    ctx: &mut ProgramTestContext,
    owner: &Keypair,
    mint: &Pubkey,
    owner_token: &Pubkey,
    seed: &[u8; 32],
    keys: u32,
    deposit: u64,
) -> Pubkey {
    let vault = vault_pda(&owner.pubkey(), mint);
    send(
        ctx,
        &[create_vault_ix(
            &owner.pubkey(),
            mint,
            commitments(seed, keys),
        )],
        &[owner],
    )
    .await
    .expect("create vault");
    if deposit > 0 {
        let ix = transfer(
            &spl_token::id(),
            owner_token,
            &token_vault_pda(&vault),
            &owner.pubkey(),
            &[],
            deposit,
        )
        .unwrap();
        send(ctx, &[ix], &[owner]).await.expect("deposit");
    }
    vault
}

/// Upload a full signature into a fresh buffer for `key_index`
/// bound to `digest`, in CHUNK-sized pieces.
async fn upload_signature(
    ctx: &mut ProgramTestContext,
    payer: &Keypair,
    vault: &Pubkey,
    key_index: u32,
    digest: [u8; 32],
    signature: &[u8],
) {
    send(
        ctx,
        &[create_buffer_ix(&payer.pubkey(), vault, key_index, digest)],
        &[payer],
    )
    .await
    .expect("create buffer");
    let mut offset = 0usize;
    while offset < signature.len() {
        let end = (offset + CHUNK).min(signature.len());
        send(
            ctx,
            &[write_chunk_ix(
                vault,
                key_index,
                offset as u16,
                signature[offset..end].to_vec(),
            )],
            &[],
        )
        .await
        .expect("write chunk");
        offset = end;
    }
}

/// The full honest withdrawal flow: digest, sign with the
/// vault's next key, upload, execute.
async fn withdraw(
    ctx: &mut ProgramTestContext,
    payer: &Keypair,
    seed: &[u8; 32],
    vault: &Pubkey,
    mint: &Pubkey,
    destination: &Pubkey,
    amount: u64,
) -> Result<(), BanksClientError> {
    let state = vault_state(ctx, vault).await;
    let key_index = state.next_key_index;
    let digest = withdrawal_digest(vault, key_index, destination, amount, mint);
    let sig = wots::sign(seed, key_index, &digest);
    upload_signature(ctx, payer, vault, key_index, digest, &sig).await;
    send(
        ctx,
        &execute_ixs(
            &payer.pubkey(),
            vault,
            mint,
            destination,
            amount,
            key_index,
        ),
        &[payer],
    )
    .await
}

// ---------------------------------------------------------------------------
// Design test 1 — create vault.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_vault_stores_commitments() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, _owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = vault_pda(&owner.pubkey(), &mint);

    send(
        &mut ctx,
        &[create_vault_ix(&owner.pubkey(), &mint, commitments(&seed, 4))],
        &[&owner],
    )
    .await
    .expect("create vault");

    let state = vault_state(&mut ctx, &vault).await;
    assert_eq!(state.owner, owner.pubkey());
    assert_eq!(state.mint, mint);
    assert_eq!(state.token_vault, token_vault_pda(&vault));
    assert_eq!(state.keys_total, 4);
    assert_eq!(state.next_key_index, 0);
    assert_eq!(state.state, STATE_ACTIVE);
    for i in 0..4u32 {
        assert_eq!(state.commitments[i as usize], wots::commitment(&seed, i));
    }
    for i in 4..MAX_KEYS {
        assert_eq!(state.commitments[i], [0u8; 32], "unused slots stay zero");
    }

    // The token vault exists, is empty, and is owned by the
    // authority PDA — only this program can ever sign for it.
    let tv = token_vault_pda(&vault);
    let a = ctx.banks_client.get_account(tv).await.unwrap().unwrap();
    let parsed = TokenAccount::unpack(&a.data).unwrap();
    assert_eq!(parsed.amount, 0);
    assert_eq!(parsed.mint, mint);
    assert_eq!(parsed.owner, authority_pda(&vault));

    // A commitment set of 0 or 17 keys is refused at creation.
    let owner2 = Keypair::new();
    let (mint2, _) = make_mint(&mut ctx, &owner2).await;
    let err = send(
        &mut ctx,
        &[create_vault_ix(&owner2.pubkey(), &mint2, vec![])],
        &[&owner2],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::InvalidCommitmentCount as u32);
    let err = send(
        &mut ctx,
        &[create_vault_ix(
            &owner2.pubkey(),
            &mint2,
            commitments(&seed, (MAX_KEYS + 1) as u32),
        )],
        &[&owner2],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::InvalidCommitmentCount as u32);
}

// ---------------------------------------------------------------------------
// Design test 2 — deposit is a plain transfer.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deposit_is_a_plain_transfer() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 0).await;

    // No PQ instruction, no signature material: an ordinary SPL
    // transfer into the token vault, signed by the depositor's
    // ordinary Ed25519 key.
    let ix = transfer(
        &spl_token::id(),
        &owner_token,
        &token_vault_pda(&vault),
        &owner.pubkey(),
        &[],
        500_000,
    )
    .unwrap();
    send(&mut ctx, &[ix], &[&owner]).await.expect("deposit");
    assert_eq!(
        token_balance(&mut ctx, &token_vault_pda(&vault)).await,
        500_000
    );
    // Depositing consumed nothing: the first key is still next.
    assert_eq!(vault_state(&mut ctx, &vault).await.next_key_index, 0);
}

// ---------------------------------------------------------------------------
// Design test 3 — valid withdrawal.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn valid_withdrawal_moves_funds_and_advances_index() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 500_000).await;
    let (_holder, dest) = make_token_account(&mut ctx, &owner, &mint).await;

    withdraw(&mut ctx, &owner, &seed, &vault, &mint, &dest, 200_000)
        .await
        .expect("withdrawal");

    assert_eq!(token_balance(&mut ctx, &dest).await, 200_000);
    assert_eq!(
        token_balance(&mut ctx, &token_vault_pda(&vault)).await,
        300_000
    );
    assert_eq!(vault_state(&mut ctx, &vault).await.next_key_index, 1);
    // The buffer closed after execution: its rent is back with
    // the payer and the account is gone.
    let buffer = ctx
        .banks_client
        .get_account(sig_pda(&vault, 0))
        .await
        .unwrap();
    assert!(buffer.is_none(), "buffer must be closed after execution");
}

// ---------------------------------------------------------------------------
// Design test 4 — replayed signature rejected.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replayed_signature_rejected() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 500_000).await;
    let (_holder, dest) = make_token_account(&mut ctx, &owner, &mint).await;

    // The honest withdrawal with key 0, keeping the exact
    // signature and digest it used.
    let digest0 = withdrawal_digest(&vault, 0, &dest, 200_000, &mint);
    let sig0 = wots::sign(&seed, 0, &digest0);
    upload_signature(&mut ctx, &owner, &vault, 0, digest0, &sig0).await;
    send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest, 200_000, 0),
        &[&owner],
    )
    .await
    .expect("first withdrawal");
    assert_eq!(token_balance(&mut ctx, &dest).await, 200_000);

    // Replay: the identical signature and digest, presented
    // again. The buffer must name the current key (1), but the
    // digest it carries is key 0's — execution re-derives the
    // digest with key index 1 and the two do not match.
    warp(&mut ctx, 10).await;
    upload_signature(&mut ctx, &owner, &vault, 1, digest0, &sig0).await;
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest, 200_000, 1),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::DigestMismatch as u32);
    assert_eq!(token_balance(&mut ctx, &dest).await, 200_000);
    assert_eq!(
        token_balance(&mut ctx, &token_vault_pda(&vault)).await,
        300_000
    );
    assert_eq!(vault_state(&mut ctx, &vault).await.next_key_index, 1);
}

// ---------------------------------------------------------------------------
// Design test 5 — forged signatures rejected.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn forged_signatures_rejected() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 500_000).await;
    let (_holder, dest) = make_token_account(&mut ctx, &owner, &mint).await;
    let digest = withdrawal_digest(&vault, 0, &dest, 200_000, &mint);

    // (a) A well-formed WOTS+ signature over the right digest,
    // made with the WRONG seed.
    let wrong_seed = [8u8; 32];
    let foreign = wots::sign(&wrong_seed, 0, &digest);
    upload_signature(&mut ctx, &owner, &vault, 0, digest, &foreign).await;
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest, 200_000, 0),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::SignatureInvalid as u32);

    // Clear the buffer and try again with the right seed's
    // signature but one chain value corrupted. (Warp first: the
    // abort and the uploads that follow are near-identical to
    // the previous phase's transactions.)
    warp(&mut ctx, 100).await;
    send(&mut ctx, &[abort_ix(&owner.pubkey(), &vault, 0)], &[&owner])
        .await
        .expect("abort");
    let mut corrupted = wots::sign(&seed, 0, &digest);
    corrupted[10 * 32 + 5] ^= 0x01;
    upload_signature(&mut ctx, &owner, &vault, 0, digest, &corrupted).await;
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest, 200_000, 0),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::SignatureInvalid as u32);

    // Neither failure consumed the key or moved a lamport of
    // tokens: the honest signature still spends key 0.
    assert_eq!(vault_state(&mut ctx, &vault).await.next_key_index, 0);
    assert_eq!(
        token_balance(&mut ctx, &token_vault_pda(&vault)).await,
        500_000
    );
    warp(&mut ctx, 200).await;
    send(&mut ctx, &[abort_ix(&owner.pubkey(), &vault, 0)], &[&owner])
        .await
        .expect("abort");
    withdraw(&mut ctx, &owner, &seed, &vault, &mint, &dest, 200_000)
        .await
        .expect("honest withdrawal after forgeries");
    assert_eq!(token_balance(&mut ctx, &dest).await, 200_000);
}

// ---------------------------------------------------------------------------
// Design test 6 — key reuse rejected.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn key_reuse_rejected() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 500_000).await;
    let (_holder, dest) = make_token_account(&mut ctx, &owner, &mint).await;

    withdraw(&mut ctx, &owner, &seed, &vault, &mint, &dest, 100_000)
        .await
        .expect("first withdrawal spends key 0");

    // The owner (or an attacker who talked them into it) signs a
    // SECOND, different withdrawal with key 0 — cryptographically
    // a perfectly valid WOTS+ signature for its digest. On-chain
    // it is worthless: key 0 is consumed, the program only ever
    // verifies against key 1 now, and the digest this signature
    // authorizes names key 0.
    warp(&mut ctx, 10).await;
    let reuse_digest = withdrawal_digest(&vault, 0, &dest, 50_000, &mint);
    let reuse_sig = wots::sign(&seed, 0, &reuse_digest);
    upload_signature(&mut ctx, &owner, &vault, 1, reuse_digest, &reuse_sig).await;
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest, 50_000, 1),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::DigestMismatch as u32);
    assert_eq!(token_balance(&mut ctx, &dest).await, 100_000);
    assert_eq!(
        token_balance(&mut ctx, &token_vault_pda(&vault)).await,
        400_000
    );
}

// ---------------------------------------------------------------------------
// Design test 7 — digest binding (amount and destination).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn digest_binding_amount_and_destination() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 500_000).await;
    let (_h1, dest_a) = make_token_account(&mut ctx, &owner, &mint).await;
    let (_h2, dest_b) = make_token_account(&mut ctx, &owner, &mint).await;

    // One signature, for exactly (dest A, 100_000).
    let digest = withdrawal_digest(&vault, 0, &dest_a, 100_000, &mint);
    let sig = wots::sign(&seed, 0, &digest);
    upload_signature(&mut ctx, &owner, &vault, 0, digest, &sig).await;

    // Amount + 1: the signature does not authorize it.
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest_a, 100_001, 0),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::DigestMismatch as u32);

    // A different destination: the signature does not authorize
    // that either. (The failed execution left the buffer intact,
    // so the same uploaded signature is still in place.)
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest_b, 100_000, 0),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::DigestMismatch as u32);

    // The exact signed parameters still execute — the failures
    // were the binding, not the signature.
    send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest_a, 100_000, 0),
        &[&owner],
    )
    .await
    .expect("the signed withdrawal itself");
    assert_eq!(token_balance(&mut ctx, &dest_a).await, 100_000);
    assert_eq!(token_balance(&mut ctx, &dest_b).await, 0);
}

// ---------------------------------------------------------------------------
// Design test 8 — wrong index rejected.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wrong_index_rejected() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 500_000).await;
    let (_holder, dest) = make_token_account(&mut ctx, &owner, &mint).await;

    // A valid signature for key 1, while next_key_index is 0,
    // fails on the plain flow too: the buffer for the current
    // key can only carry a digest naming key 0.
    let digest1 = withdrawal_digest(&vault, 1, &dest, 100_000, &mint);
    let sig1 = wots::sign(&seed, 1, &digest1);
    upload_signature(&mut ctx, &owner, &vault, 0, digest1, &sig1).await;
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest, 100_000, 0),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::DigestMismatch as u32);
    send(&mut ctx, &[abort_ix(&owner.pubkey(), &vault, 0)], &[&owner])
        .await
        .expect("abort");

    // The same attack against a forged buffer: an account at
    // key 0's PDA whose header claims key_index 1 (no honest
    // instruction sequence can produce this state — every
    // key-consuming instruction closes its buffer — so the
    // state is planted directly, the way a corrupted or foreign
    // writer would leave it). The rejection here is InvalidPda:
    // the buffer check re-derives the PDA from the header's own
    // binding, and an account claiming key 1 does not belong at
    // key 0's address. (WrongKeyIndex itself fires at
    // CreateSigBuffer, when a caller names a key that is not
    // the vault's next.)
    let mut data = vec![0u8; SigBuffer::LEN];
    SigBuffer::write_header(&mut data, &vault, 1, &digest1, wots::SIG_BYTES as u16);
    SigBuffer::signature_mut(&mut data).copy_from_slice(&sig1);
    let forged = Account {
        lamports: 20_000_000,
        data,
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    };
    warp(&mut ctx, 10).await;
    ctx.set_account(&sig_pda(&vault, 0), &forged.into());
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest, 100_000, 0),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::InvalidPda as u32);
    assert_eq!(
        token_balance(&mut ctx, &token_vault_pda(&vault)).await,
        500_000
    );
    assert_eq!(vault_state(&mut ctx, &vault).await.next_key_index, 0);
}

// ---------------------------------------------------------------------------
// Design test 9 — rotation.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rotation_installs_new_set_and_resets_index() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed_a = [3u8; 32];
    let seed_b = [11u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed_a, 3, 500_000).await;
    let (_holder, dest) = make_token_account(&mut ctx, &owner, &mint).await;

    // Rotate with key 0 of set A, installing set B (2 keys).
    let new_set = commitments(&seed_b, 2);
    let set_hash = commitment_set_hash(&new_set);
    let digest = rotation_digest(&vault, 0, &set_hash);
    let sig = wots::sign(&seed_a, 0, &digest);
    upload_signature(&mut ctx, &owner, &vault, 0, digest, &sig).await;
    send(
        &mut ctx,
        &rotate_ixs(&owner.pubkey(), &vault, 0, new_set.clone()),
        &[&owner],
    )
    .await
    .expect("rotation");

    let state = vault_state(&mut ctx, &vault).await;
    assert_eq!(state.keys_total, 2);
    assert_eq!(state.next_key_index, 0);
    assert_eq!(state.commitments[0], new_set[0]);
    assert_eq!(state.commitments[1], new_set[1]);
    assert_eq!(state.commitments[2], [0u8; 32]);

    // The new set's key 0 spends. Set A's keys are gone: the
    // withdrawal is signed by seed B and verifies — seed A
    // material authorizes nothing anymore.
    withdraw(&mut ctx, &owner, &seed_b, &vault, &mint, &dest, 125_000)
        .await
        .expect("withdrawal under the rotated set");
    assert_eq!(token_balance(&mut ctx, &dest).await, 125_000);
}

#[tokio::test]
async fn rotation_without_wots_signature_fails() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 500_000).await;

    // The owner's Ed25519 signature is on the transaction — it
    // is the fee payer and the vault owner — and it buys
    // nothing: rotation requires a WOTS+ signature from the
    // next key, and a buffer full of zeros is not one.
    let new_set = commitments(&[11u8; 32], 2);
    let set_hash = commitment_set_hash(&new_set);
    let digest = rotation_digest(&vault, 0, &set_hash);
    upload_signature(&mut ctx, &owner, &vault, 0, digest, &[0u8; wots::SIG_BYTES]).await;
    let err = send(
        &mut ctx,
        &rotate_ixs(&owner.pubkey(), &vault, 0, new_set),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::SignatureInvalid as u32);

    let state = vault_state(&mut ctx, &vault).await;
    assert_eq!(state.keys_total, 2);
    assert_eq!(state.next_key_index, 0);
    assert_eq!(state.commitments[0], wots::commitment(&seed, 0));
}

// ---------------------------------------------------------------------------
// Design test 10 — exhaustion freezes the vault.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exhaustion_freezes_vault() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 300_000).await;
    let (_holder, dest) = make_token_account(&mut ctx, &owner, &mint).await;

    // Spend both keys — no rotation chained.
    withdraw(&mut ctx, &owner, &seed, &vault, &mint, &dest, 100_000)
        .await
        .expect("spend key 0");
    withdraw(&mut ctx, &owner, &seed, &vault, &mint, &dest, 100_000)
        .await
        .expect("spend key 1");
    assert_eq!(vault_state(&mut ctx, &vault).await.next_key_index, 2);

    // A further withdrawal cannot even open its buffer: there
    // is no next key to bind it to.
    let digest = withdrawal_digest(&vault, 2, &dest, 100_000, &mint);
    let err = send(
        &mut ctx,
        &[create_buffer_ix(&owner.pubkey(), &vault, 2, digest)],
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::KeysExhausted as u32);

    // Rotation is equally closed — it needed one unconsumed key
    // to authorize it, and there are none. The remaining
    // 100,000 does not move. Fail-closed, by design.
    assert_eq!(
        token_balance(&mut ctx, &token_vault_pda(&vault)).await,
        100_000
    );
    assert_eq!(token_balance(&mut ctx, &dest).await, 200_000);
}

// ---------------------------------------------------------------------------
// Design test 11 — buffer discipline.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn buffer_discipline() {
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 500_000).await;
    let (_holder, dest) = make_token_account(&mut ctx, &owner, &mint).await;

    let digest = withdrawal_digest(&vault, 0, &dest, 100_000, &mint);
    let sig = wots::sign(&seed, 0, &digest);

    // A buffer for any key but the next one is refused at
    // creation: keys are spent strictly in order.
    let err = send(
        &mut ctx,
        &[create_buffer_ix(&owner.pubkey(), &vault, 1, digest)],
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::WrongKeyIndex as u32);

    // Out-of-order chunk: the first write must start at 0.
    send(
        &mut ctx,
        &[create_buffer_ix(&owner.pubkey(), &vault, 0, digest)],
        &[&owner],
    )
    .await
    .expect("create buffer");
    let err = send(
        &mut ctx,
        &[write_chunk_ix(&vault, 0, CHUNK as u16, sig[CHUNK..2 * CHUNK].to_vec())],
        &[],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::ChunkOutOfOrder as u32);

    // The probe transaction above and the legitimate second
    // chunk below are byte-identical; without a warp the bank
    // could answer the chunk from its signature cache instead
    // of executing it.
    warp(&mut ctx, 10).await;

    // Incomplete buffer: two of four chunks uploaded, execution
    // refuses.
    send(&mut ctx, &[write_chunk_ix(&vault, 0, 0, sig[..CHUNK].to_vec())], &[])
        .await
        .expect("chunk 1");
    send(
        &mut ctx,
        &[write_chunk_ix(&vault, 0, CHUNK as u16, sig[CHUNK..2 * CHUNK].to_vec())],
        &[],
    )
    .await
    .expect("chunk 2");
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest, 100_000, 0),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::BufferIncomplete as u32);

    // Crossed wires: a buffer bound to one withdrawal's digest,
    // executed against another withdrawal, fails — chunks cannot
    // be mixed across withdrawals because the binding is the
    // digest, checked at execution.
    warp(&mut ctx, 20).await;
    send(&mut ctx, &[abort_ix(&owner.pubkey(), &vault, 0)], &[&owner])
        .await
        .expect("abort");
    upload_signature(&mut ctx, &owner, &vault, 0, digest, &sig).await;
    let err = send(
        &mut ctx,
        &execute_ixs(&owner.pubkey(), &vault, &mint, &dest, 222_000, 0),
        &[&owner],
    )
    .await
    .unwrap_err();
    expect_custom(&err, PqError::DigestMismatch as u32);
    assert_eq!(
        token_balance(&mut ctx, &token_vault_pda(&vault)).await,
        500_000
    );
}

// ---------------------------------------------------------------------------
// Design test 12 — measured compute units.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compute_units_execute_withdrawal() {
    // Measurement test: prints the compute units a full
    // ExecuteWithdrawal (buffer reads + WOTS+ verification +
    // token transfer) consumes, for the record in
    // programs/PQ-VERIFY.md. The figure that belongs in the
    // record is the one printed when this suite runs against
    // the SBF build (BPF_OUT_DIR set) — native program-test
    // execution meters almost nothing, the lesson bridge/
    // VERIFY.md records. The bound below is a sanity ceiling,
    // not a target.
    let mut ctx = start().await;
    let owner = Keypair::new();
    let (mint, owner_token) = make_mint(&mut ctx, &owner).await;
    let seed = [3u8; 32];
    let vault = setup_vault(&mut ctx, &owner, &mint, &owner_token, &seed, 2, 500_000).await;
    let (_holder, dest) = make_token_account(&mut ctx, &owner, &mint).await;

    let digest = withdrawal_digest(&vault, 0, &dest, 200_000, &mint);
    let sig = wots::sign(&seed, 0, &digest);
    upload_signature(&mut ctx, &owner, &vault, 0, digest, &sig).await;

    let ix = execute_ix(&owner.pubkey(), &vault, &mint, &dest, 200_000, 0);
    let payer = ctx.payer.pubkey();
    // The measurement transaction requests the maximum compute
    // budget so the figure can be read even if verification costs
    // more than the 200,000 default — whether it fits the default
    // is part of what the record states.
    let budget =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(1_400_000);
    let tx = Transaction::new_signed_with_payer(
        &[budget, ix],
        Some(&payer),
        &[&ctx.payer, &owner],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    result.result.unwrap();
    let cu = result.metadata.unwrap().compute_units_consumed;
    println!("CU ExecuteWithdrawal (incl. WOTS+ verify): {cu}");
    assert!(
        cu < 1_400_000,
        "ExecuteWithdrawal does not fit in a transaction at all: {cu}"
    );
    assert_eq!(token_balance(&mut ctx, &dest).await, 200_000);

    // RotateKeys runs the same verification (key 1, the next
    // unconsumed key, authorizes a fresh set) — measure it too.
    let new_set = commitments(&[11u8; 32], 2);
    let set_hash = commitment_set_hash(&new_set);
    let rdigest = rotation_digest(&vault, 1, &set_hash);
    let rsig = wots::sign(&seed, 1, &rdigest);
    upload_signature(&mut ctx, &owner, &vault, 1, rdigest, &rsig).await;
    let rix = rotate_ix(&owner.pubkey(), &vault, 1, new_set);
    let budget =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(1_400_000);
    let tx = Transaction::new_signed_with_payer(
        &[budget, rix],
        Some(&payer),
        &[&ctx.payer, &owner],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    result.result.unwrap();
    let cu = result.metadata.unwrap().compute_units_consumed;
    println!("CU RotateKeys (incl. WOTS+ verify): {cu}");
    assert!(
        cu < 1_400_000,
        "RotateKeys does not fit in a transaction at all: {cu}"
    );
}
