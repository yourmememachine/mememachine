//! Building the destination-chain instructions for a quorum
//! attestation. Account order is copied exactly from the program
//! processors (and from their integration tests) — a wrong order is
//! a failed transaction, never a wrong mint, because the programs
//! re-check every PDA, the mint, and the destination owner on-chain.

use bridge_common::{
    authority_pda, config_pda, nonce_pda, BridgeMessage, LockInstruction, MachineInstruction,
    RelayerSignature, DIR_MACHINE_TO_SOLANA, DIR_SOLANA_TO_MACHINE,
};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

/// MintWrapped on Machine (machine-bridge processor, in order):
/// config, nonce record, wrapped mint, destination token account,
/// bridge authority, payer, token program, system program.
/// `wrapped_mint` comes from the relayer config — the message itself
/// carries the *original* Solana mint, not the wrapped one.
pub fn build_mint_instruction(
    machine_program_id: &Pubkey,
    message: &BridgeMessage,
    signatures: Vec<RelayerSignature>,
    wrapped_mint: &Pubkey,
    destination_token_account: &Pubkey,
    payer: &Pubkey,
) -> Instruction {
    let (config, _) = config_pda(machine_program_id);
    let (nonce, _) = nonce_pda(machine_program_id, DIR_SOLANA_TO_MACHINE, message.nonce);
    let (authority, _) = authority_pda(machine_program_id);
    Instruction::new_with_borsh(
        *machine_program_id,
        &MachineInstruction::MintWrapped {
            message: message.clone(),
            signatures,
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
        ],
    )
}

/// Unlock on Solana (solana-lock processor, in order):
/// config, vault, destination token account, nonce record,
/// vault authority, payer, token program, system program.
pub fn build_unlock_instruction(
    lock_program_id: &Pubkey,
    message: &BridgeMessage,
    signatures: Vec<RelayerSignature>,
    vault: &Pubkey,
    destination_token_account: &Pubkey,
    payer: &Pubkey,
) -> Instruction {
    let (config, _) = config_pda(lock_program_id);
    let (nonce, _) = nonce_pda(lock_program_id, DIR_MACHINE_TO_SOLANA, message.nonce);
    let (authority, _) = authority_pda(lock_program_id);
    Instruction::new_with_borsh(
        *lock_program_id,
        &LockInstruction::Unlock {
            message: message.clone(),
            signatures,
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
        ],
    )
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
            expiry_slot: 100_005,
        }
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
        let ix = build_mint_instruction(&program, &m, vec![], &wrapped, &dest, &payer);
        assert_eq!(ix.program_id, program);
        assert_eq!(ix.accounts.len(), 8);
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
        let ix = build_unlock_instruction(&program, &m, vec![], &vault, &dest, &payer);
        assert_eq!(ix.accounts.len(), 8);
        assert_eq!(ix.accounts[0].pubkey, config_pda(&program).0);
        assert_eq!(ix.accounts[1].pubkey, vault);
        assert_eq!(ix.accounts[2].pubkey, dest);
        assert_eq!(
            ix.accounts[3].pubkey,
            nonce_pda(&program, DIR_MACHINE_TO_SOLANA, 9).0
        );
        assert_eq!(ix.accounts[4].pubkey, authority_pda(&program).0);
        let decoded = LockInstruction::try_from_slice(&ix.data).unwrap();
        match decoded {
            LockInstruction::Unlock { message, .. } => assert_eq!(message, m),
            other => panic!("wrong instruction: {other:?}"),
        }
    }
}
