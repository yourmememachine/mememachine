//! Building the destination-chain instructions for a quorum
//! attestation. Account order is copied exactly from the program
//! processors (and from their integration tests) — a wrong order is
//! a failed transaction, never a wrong mint, because the programs
//! re-check every PDA, the mint, and the destination owner on-chain.

use bridge_common::{
    authority_pda, build_ed25519_instruction, config_pda, nonce_pda, receipt_pda, BridgeMessage,
    CrossingReceipt, LockInstruction, MachineInstruction, RelayerSignature,
    DIR_MACHINE_TO_SOLANA, DIR_SOLANA_TO_MACHINE, DISCRIMINATOR_RECEIPT,
};
use borsh::BorshDeserialize as _;

/// Is the account at the receipt PDA a GENUINE crossing receipt
/// written by the destination program — right owner, parses as a
/// `CrossingReceipt`, right discriminator, direction, and nonce?
/// Existence alone is NOT proof (debate scope-2r2, A-F1): anyone
/// can pre-fund the predictable receipt PDA with an empty
/// system-owned account, and the destination programs deliberately
/// adopt such pristine shells when they write the real receipt —
/// so an existence-only check in the submitter permanently
/// suppresses a crossing that was never processed.
pub fn receipt_is_genuine(
    owner: &Pubkey,
    data: &[u8],
    program_id: &Pubkey,
    direction: u8,
    nonce: u64,
) -> bool {
    if owner != program_id {
        return false;
    }
    let Ok(r) = CrossingReceipt::try_from_slice(data) else {
        return false;
    };
    r.discriminator == DISCRIMINATOR_RECEIPT && r.direction == direction && r.nonce == nonce
}
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
    sysvar,
};

/// The submission for one attested message is TWO instructions in one
/// transaction: first the ed25519 precompile instruction carrying the
/// quorum signatures over the message hash (the runtime verifies them
/// before anything executes), then the bridge instruction, which reads
/// the attestation back from the instructions sysvar.
///
/// MintWrapped on Machine (machine-bridge processor, in order):
/// config, nonce record, wrapped mint, destination token account,
/// bridge authority, payer, token program, system program,
/// instructions sysvar. `wrapped_mint` comes from the relayer config —
/// the message itself carries the *original* Solana mint, not the
/// wrapped one.
pub fn build_mint_instructions(
    machine_program_id: &Pubkey,
    message: &BridgeMessage,
    signatures: Vec<RelayerSignature>,
    wrapped_mint: &Pubkey,
    destination_token_account: &Pubkey,
    payer: &Pubkey,
) -> Vec<Instruction> {
    let (config, _) = config_pda(machine_program_id);
    let (nonce, _) = nonce_pda(machine_program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (authority, _) = authority_pda(machine_program_id);
    let (receipt, _) = receipt_pda(machine_program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    let mint_ix = Instruction::new_with_borsh(
        *machine_program_id,
        &MachineInstruction::MintWrapped {
            message: message.clone(),
        },
        vec![
            AccountMeta::new(config, false),
            AccountMeta::new(nonce, false),
            AccountMeta::new(*wrapped_mint, false),
            AccountMeta::new(*destination_token_account, false),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(spl_token_id(), false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(sysvar::instructions::ID, false),
            AccountMeta::new(receipt, false),
        ],
    );
    vec![build_ed25519_instruction(&message.hash(), &signatures), mint_ix]
}

/// Unlock on Solana (solana-lock processor, in order): config, vault,
/// destination token account, nonce record, vault authority, payer,
/// token program, system program, instructions sysvar.
pub fn build_unlock_instructions(
    lock_program_id: &Pubkey,
    message: &BridgeMessage,
    signatures: Vec<RelayerSignature>,
    vault: &Pubkey,
    destination_token_account: &Pubkey,
    payer: &Pubkey,
) -> Vec<Instruction> {
    let (config, _) = config_pda(lock_program_id);
    let (nonce, _) = nonce_pda(lock_program_id, DIR_MACHINE_TO_SOLANA, message.nonce);
    let (authority, _) = authority_pda(lock_program_id);
    let (receipt, _) = receipt_pda(lock_program_id, DIR_MACHINE_TO_SOLANA, message.nonce);
    let unlock_ix = Instruction::new_with_borsh(
        *lock_program_id,
        &LockInstruction::Unlock {
            message: message.clone(),
        },
        vec![
            AccountMeta::new(config, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*destination_token_account, false),
            AccountMeta::new(nonce, false),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(spl_token_id(), false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(sysvar::instructions::ID, false),
            AccountMeta::new(receipt, false),
        ],
    );
    vec![build_ed25519_instruction(&message.hash(), &signatures), unlock_ix]
}

fn spl_token_id() -> Pubkey {
    // spl-token program id, TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA.
    // Written as a constant so this crate does not need a spl-token
    // dependency for one pubkey; the programs receive it as an
    // account and check nothing about it beyond passing it to CPI.
    Pubkey::new_from_array([
        6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133,
        237, 95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshDeserialize;
    use bridge_common::{CHAIN_MACHINE, CHAIN_SOLANA, MESSAGE_VERSION};

    fn msg(source: u8, dest: u8, nonce: u64) -> BridgeMessage {
        BridgeMessage {
            version: MESSAGE_VERSION,
            source_chain: source,
            dest_chain: dest,
            nonce,
            source_mint: Pubkey::new_unique(),
            sender: Pubkey::new_unique(),
            recipient: Pubkey::new_unique(),
            amount: 10,
            source_tx: [1u8; 64],
            source_slot: 5,
            expiry_timestamp: 100_005,
            source_timestamp: 5,
        }
    }

    #[test]
    fn receipt_genuineness_requires_owner_and_contents() {
        // Debate scope-2r2 (A-F1): a pre-funded empty account at
        // the receipt PDA, a foreign-owned account, and a
        // program-owned account with the wrong contents must all
        // read as NOT processed; only the program's own receipt
        // for this direction+nonce counts.
        let program = Pubkey::new_unique();
        let receipt = CrossingReceipt {
            discriminator: DISCRIMINATOR_RECEIPT,
            direction: DIR_SOLANA_TO_MACHINE,
            nonce: 7,
            message_hash: [9u8; 32],
            source_tx: [7u8; 64],
            amount: 1_000,
            recipient: Pubkey::new_unique(),
            slot: 123,
            timestamp: 1_700_000_000,
        };
        let bytes = borsh::to_vec(&receipt).unwrap();
        assert!(receipt_is_genuine(&program, &bytes, &program, DIR_SOLANA_TO_MACHINE, 7));
        // Pre-funded empty shell (system-owned, no data).
        assert!(!receipt_is_genuine(
            &solana_program::system_program::id(),
            &[],
            &program,
            DIR_SOLANA_TO_MACHINE,
            7
        ));
        // Right bytes, wrong owner.
        assert!(!receipt_is_genuine(
            &Pubkey::new_unique(),
            &bytes,
            &program,
            DIR_SOLANA_TO_MACHINE,
            7
        ));
        // Genuine receipt, wrong nonce or direction.
        assert!(!receipt_is_genuine(&program, &bytes, &program, DIR_SOLANA_TO_MACHINE, 8));
        assert!(!receipt_is_genuine(&program, &bytes, &program, DIR_MACHINE_TO_SOLANA, 7));
        // Program-owned garbage.
        assert!(!receipt_is_genuine(
            &program,
            &[1u8; 10],
            &program,
            DIR_SOLANA_TO_MACHINE,
            7
        ));
    }

    #[test]
    fn spl_token_id_is_tokenkeg() {
        assert_eq!(
            spl_token_id().to_string(),
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        );
    }

    #[test]
    fn mint_instruction_accounts_and_data() {
        let program = Pubkey::new_unique();
        let m = msg(CHAIN_SOLANA, CHAIN_MACHINE, 7);
        let wrapped = Pubkey::new_unique();
        let dest = Pubkey::new_unique();
        let payer = Pubkey::new_unique();
        let ixs = build_mint_instructions(&program, &m, vec![], &wrapped, &dest, &payer);
        assert_eq!(ixs.len(), 2);
        // First: the ed25519 precompile instruction.
        assert_eq!(ixs[0].program_id, solana_program::ed25519_program::ID);
        let ix = &ixs[1];
        assert_eq!(ix.program_id, program);
        assert_eq!(ix.accounts.len(), 10);
        assert_eq!(ix.accounts[0].pubkey, config_pda(&program).0);
        assert_eq!(
            ix.accounts[1].pubkey,
            nonce_pda(&program, DIR_SOLANA_TO_MACHINE, 7).0
        );
        assert_eq!(ix.accounts[2].pubkey, wrapped);
        assert_eq!(ix.accounts[3].pubkey, dest);
        assert_eq!(ix.accounts[4].pubkey, authority_pda(&program).0);
        assert_eq!(ix.accounts[5].pubkey, payer);
        assert!(ix.accounts[5].is_signer);
        assert_eq!(ix.accounts[8].pubkey, solana_program::sysvar::instructions::ID);
        let decoded = MachineInstruction::try_from_slice(&ix.data).unwrap();
        match decoded {
            MachineInstruction::MintWrapped { message, .. } => assert_eq!(message, m),
            other => panic!("wrong instruction: {other:?}"),
        }
    }

    #[test]
    fn unlock_instruction_accounts_and_data() {
        let program = Pubkey::new_unique();
        let m = msg(CHAIN_MACHINE, CHAIN_SOLANA, 9);
        let vault = Pubkey::new_unique();
        let dest = Pubkey::new_unique();
        let payer = Pubkey::new_unique();
        let ixs = build_unlock_instructions(&program, &m, vec![], &vault, &dest, &payer);
        assert_eq!(ixs.len(), 2);
        assert_eq!(ixs[0].program_id, solana_program::ed25519_program::ID);
        let ix = &ixs[1];
        assert_eq!(ix.accounts.len(), 10);
        assert_eq!(ix.accounts[0].pubkey, config_pda(&program).0);
        assert_eq!(ix.accounts[1].pubkey, vault);
        assert_eq!(ix.accounts[2].pubkey, dest);
        assert_eq!(
            ix.accounts[3].pubkey,
            nonce_pda(&program, DIR_MACHINE_TO_SOLANA, 9).0
        );
        assert_eq!(ix.accounts[4].pubkey, authority_pda(&program).0);
        assert_eq!(ix.accounts[8].pubkey, solana_program::sysvar::instructions::ID);
        let decoded = LockInstruction::try_from_slice(&ix.data).unwrap();
        match decoded {
            LockInstruction::Unlock { message, .. } => assert_eq!(message, m),
            other => panic!("wrong instruction: {other:?}"),
        }
    }
}
