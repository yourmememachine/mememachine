//! End-to-end relayer flow with no network: a deposit event is
//! observed, three relayers sign it, the collector reaches quorum,
//! the collected attestation passes bridge-common's own verifier
//! (the same code the destination program runs), and the submitter
//! builds a MintWrapped instruction whose data decodes back to that
//! exact message and signature set.

use borsh::BorshDeserialize;
use bridge_common::{
    verify_attestation, BridgeMessage, EventRecord, MachineInstruction, CHAIN_MACHINE,
    CHAIN_SOLANA, DISCRIMINATOR_EVENT_RECORD, QUORUM,
};
use relayer::attestation::{AttestationFile, Collector};
use relayer::message::{build_message, Direction, ObservedEvent};
use relayer::signing::RelayerSigner;
use relayer::submit::build_mint_instructions;
use solana_program::pubkey::Pubkey;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;

#[test]
fn deposit_to_quorum_to_mint_instruction() {
    // Five registered relayers.
    let mut rng = OsRng;
    let mut signers = Vec::new();
    let mut pubs = Vec::new();
    for _ in 0..5 {
        let k = SigningKey::generate(&mut rng);
        signers.push(RelayerSigner::from_seed(&k.to_bytes()));
        pubs.push(Pubkey::new_from_array(k.verifying_key().to_bytes()));
    }
    let relayers: [Pubkey; 5] = pubs.try_into().unwrap();

    // A finalized deposit: 250_000 units to a Machine recipient, nonce 7.
    let recipient = Pubkey::new_unique();
    let source_mint = Pubkey::new_unique();
    let event = ObservedEvent {
        record: EventRecord {
            discriminator: DISCRIMINATOR_EVENT_RECORD,
            nonce: 7,
            amount: 250_000,
            destination: recipient,
            slot: 12_345,
        },
        sender: Pubkey::new_unique(),
        source_tx: [42u8; 64],
    };
    let message = build_message(
        Direction::SolanaToMachine,
        &event,
        source_mint,
        100_000,
    )
    .unwrap();
    assert_eq!(message.source_chain, CHAIN_SOLANA);
    assert_eq!(message.dest_chain, CHAIN_MACHINE);

    // Three relayers sign and exchange files; two stay offline.
    let dir = std::env::temp_dir().join(format!("relayer-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for signer in signers.iter().take(3) {
        AttestationFile::new(Direction::SolanaToMachine, &message, &signer.sign(&message))
            .write_to(&dir)
            .unwrap();
    }

    // Collect the quorum and re-verify (collector does this internally;
    // do it again explicitly to mirror the on-chain check).
    let collector = Collector {
        relayers,
        quorum: QUORUM,
    };
    let (collected, signatures) = collector
        .collect(&dir, Direction::SolanaToMachine, 7, 50_000)
        .unwrap();
    assert_eq!(collected, message);
    assert_eq!(signatures.len(), 3);
    verify_attestation(
        &collected,
        &signatures,
        &relayers,
        QUORUM,
        CHAIN_SOLANA,
        CHAIN_MACHINE,
        50_000,
    )
    .unwrap();

    // Build the submission: an ed25519 precompile instruction carrying
    // the signatures, then the MintWrapped instruction carrying the
    // same message.
    let machine_program = Pubkey::new_unique();
    let wrapped_mint = Pubkey::new_unique();
    let dest_token_account = Pubkey::new_unique();
    let payer = Pubkey::new_unique();
    let ixs = build_mint_instructions(
        &machine_program,
        &collected,
        signatures.clone(),
        &wrapped_mint,
        &dest_token_account,
        &payer,
    );
    assert_eq!(ixs.len(), 2);
    assert_eq!(ixs[0].program_id, solana_program::ed25519_program::ID);
    // The precompile instruction's payload ends with the message hash.
    let hash = collected.hash();
    assert_eq!(&ixs[0].data[ixs[0].data.len() - 32..], &hash);
    let decoded = MachineInstruction::try_from_slice(&ixs[1].data).unwrap();
    match decoded {
        MachineInstruction::MintWrapped { message: m } => {
            assert_eq!(m, collected);
        }
        other => panic!("expected MintWrapped, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expired_attestation_is_rejected_by_collector() {
    let mut rng = OsRng;
    let mut signers = Vec::new();
    let mut pubs = Vec::new();
    for _ in 0..5 {
        let k = SigningKey::generate(&mut rng);
        signers.push(RelayerSigner::from_seed(&k.to_bytes()));
        pubs.push(Pubkey::new_from_array(k.verifying_key().to_bytes()));
    }
    let relayers: [Pubkey; 5] = pubs.try_into().unwrap();
    let message = BridgeMessage {
        version: bridge_common::MESSAGE_VERSION,
        source_chain: CHAIN_SOLANA,
        dest_chain: CHAIN_MACHINE,
        nonce: 1,
        source_mint: Pubkey::new_unique(),
        sender: Pubkey::new_unique(),
        recipient: Pubkey::new_unique(),
        amount: 1,
        source_tx: [1u8; 64],
        source_slot: 10,
        expiry_slot: 110,
    };
    let dir = std::env::temp_dir().join(format!("relayer-e2e-exp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for signer in signers.iter().take(3) {
        AttestationFile::new(Direction::SolanaToMachine, &message, &signer.sign(&message))
            .write_to(&dir)
            .unwrap();
    }
    let collector = Collector {
        relayers,
        quorum: QUORUM,
    };
    // Destination slot 111 is past expiry_slot 110.
    assert!(collector
        .collect(&dir, Direction::SolanaToMachine, 1, 111)
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
