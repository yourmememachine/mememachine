//! LaunchGuard registry export — the off-chain half of the launch standard.
//!
//! The program writes the truth on-chain (`LaunchRecord` + `CreatorReputation`
//! PDAs); this crate turns those accounts into a public, machine-readable
//! JSON registry that wallets, bots, and checkers can consume without
//! knowing LaunchGuard's account layout:
//!
//! ```json
//! {
//!   "program_id": "…",
//!   "generated_at_slot": 123,
//!   "total_launches": 2,
//!   "total_locked": 225000000,
//!   "launches": [
//!     {
//!       "mint": "…", "creator": "…", "record": "…", "vault": "…",
//!       "total_supply": 1000000000, "locked_amount": 100000000,
//!       "locked_pct_bps": 1000,
//!       "lock_until_slot": 6480003, "registered_slot": 3,
//!       "status": "locked",
//!       "reputation": { "launches_registered": 1, "locks_completed": 0,
//!                       "first_launch_slot": 3, "last_completed_slot": 0 },
//!       "vault_balance": 100000000, "vault_verified": true,
//!       "authorities_revoked": true
//!     }
//!   ]
//! }
//! ```
//!
//! The `vault_*` / `authorities_revoked` fields are `null` unless the
//! caller also fetched the vault and mint accounts (the `--verify` flag
//! of the `launchguard-registry` binary) — the registry never claims a
//! check it did not run.
//!
//! Trust model, stated plainly: this reads whatever the RPC endpoint
//! returns. A lying RPC can lie about the registry. What the export
//! does guarantee is that *given* the accounts, the parsing is strict
//! (owner + discriminator + exact length) and the verification
//! re-checks the two registration invariants — vault balance covers
//! the locked amount, and both mint authorities are revoked —
//! against the fetched vault and mint accounts instead of trusting
//! the record's word for it.

use borsh::BorshDeserialize;
use launch_guard::state::{
    CreatorReputation, LaunchRecord, DISCRIMINATOR_LAUNCH, DISCRIMINATOR_REPUTATION,
    STATE_LOCKED, STATE_RELEASED,
};
use serde::{Deserialize, Serialize};
use solana_program::program_pack::Pack;
use solana_sdk::{account::Account, pubkey::Pubkey};
use spl_token::state::{Account as TokenAccount, Mint};
use std::collections::HashMap;

/// Lock status derived from the record and the slot the registry was
/// generated at. `expired_unreleased` is a real state: the lock has
/// elapsed but the creator has not called Release yet, so the tokens
/// are still in the vault.
pub const STATUS_LOCKED: &str = "locked";
pub const STATUS_EXPIRED_UNRELEASED: &str = "expired_unreleased";
pub const STATUS_RELEASED: &str = "released";

/// A creator's public track record, as the registry reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReputationSummary {
    pub launches_registered: u64,
    pub locks_completed: u64,
    pub first_launch_slot: u64,
    pub last_completed_slot: u64,
}

/// One protected launch, as the registry reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub mint: String,
    pub creator: String,
    pub record: String,
    pub vault: String,
    pub total_supply: u64,
    pub locked_amount: u64,
    /// Locked share of supply in basis points (10,000 = 100%).
    pub locked_pct_bps: u64,
    pub lock_until_slot: u64,
    pub registered_slot: u64,
    /// "locked" | "expired_unreleased" | "released".
    pub status: String,
    /// `None` when the creator has no reputation account (cannot
    /// happen for a real launch — registration creates one — but the
    /// registry reports the absence instead of inventing a record).
    pub reputation: Option<ReputationSummary>,
    /// Vault token balance at generation time (`--verify` only).
    pub vault_balance: Option<u64>,
    /// Vault holds at least the locked amount (locked launches), or
    /// is empty (released launches). `None` without `--verify`.
    pub vault_verified: Option<bool>,
    /// Both mint and freeze authorities revoked (`--verify` only).
    pub authorities_revoked: Option<bool>,
}

/// The whole registry: every LaunchGuard launch the RPC returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub program_id: String,
    pub generated_at_slot: u64,
    pub total_launches: usize,
    /// Sum of `locked_amount` over entries whose status is "locked"
    /// or "expired_unreleased" (released locks no longer hold funds).
    pub total_locked: u64,
    pub launches: Vec<RegistryEntry>,
}

/// Parse one account as a `LaunchRecord`, or `None` if it is not one.
/// Strict: the owner must be the LaunchGuard program, the first byte
/// must be the launch discriminator, and the length must be exactly
/// `LaunchRecord::LEN`. Anything else — a mint account, a reputation
/// account, a token account, garbage — is ignored, never misread.
pub fn parse_launch_record(
    pubkey: &Pubkey,
    account: &Account,
    program_id: &Pubkey,
) -> Option<(Pubkey, LaunchRecord)> {
    if account.owner != *program_id {
        return None;
    }
    if account.data.len() != LaunchRecord::LEN {
        return None;
    }
    if account.data[0] != DISCRIMINATOR_LAUNCH {
        return None;
    }
    let record = LaunchRecord::try_from_slice(&account.data).ok()?;
    Some((*pubkey, record))
}

/// Parse one account as a `CreatorReputation`, or `None` if it is not
/// one. Same strictness as [`parse_launch_record`].
pub fn parse_reputation(
    pubkey: &Pubkey,
    account: &Account,
    program_id: &Pubkey,
) -> Option<(Pubkey, CreatorReputation)> {
    if account.owner != *program_id {
        return None;
    }
    if account.data.len() != CreatorReputation::LEN {
        return None;
    }
    if account.data[0] != DISCRIMINATOR_REPUTATION {
        return None;
    }
    let reputation = CreatorReputation::try_from_slice(&account.data).ok()?;
    Some((*pubkey, reputation))
}

fn status_of(record: &LaunchRecord, current_slot: u64) -> &'static str {
    if record.state == STATE_RELEASED {
        STATUS_RELEASED
    } else if record.state == STATE_LOCKED && current_slot >= record.lock_until_slot {
        STATUS_EXPIRED_UNRELEASED
    } else {
        STATUS_LOCKED
    }
}

/// Build a registry from program accounts (e.g. one
/// `getProgramAccounts` response). Foreign accounts are skipped;
/// reputation accounts are joined to launches by creator; entries
/// are sorted by mint so the output is deterministic.
pub fn build_registry(
    accounts: &[(Pubkey, Account)],
    program_id: &Pubkey,
    current_slot: u64,
) -> Registry {
    let mut records: Vec<(Pubkey, LaunchRecord)> = Vec::new();
    let mut reputations: HashMap<Pubkey, CreatorReputation> = HashMap::new();
    for (pubkey, account) in accounts {
        if let Some(record) = parse_launch_record(pubkey, account, program_id) {
            records.push(record);
        } else if let Some((_, reputation)) = parse_reputation(pubkey, account, program_id) {
            reputations.insert(reputation.creator, reputation);
        }
    }

    let mut launches: Vec<RegistryEntry> = records
        .iter()
        .map(|(record_pda, record)| {
            let status = status_of(record, current_slot);
            let locked_pct_bps = if record.total_supply == 0 {
                0
            } else {
                (record.locked_amount as u128 * 10_000 / record.total_supply as u128) as u64
            };
            RegistryEntry {
                mint: record.mint.to_string(),
                creator: record.creator.to_string(),
                record: record_pda.to_string(),
                vault: record.vault.to_string(),
                total_supply: record.total_supply,
                locked_amount: record.locked_amount,
                locked_pct_bps,
                lock_until_slot: record.lock_until_slot,
                registered_slot: record.registered_slot,
                status: status.to_string(),
                reputation: reputations.get(&record.creator).map(|rep| ReputationSummary {
                    launches_registered: rep.launches_registered,
                    locks_completed: rep.locks_completed,
                    first_launch_slot: rep.first_launch_slot,
                    last_completed_slot: rep.last_completed_slot,
                }),
                vault_balance: None,
                vault_verified: None,
                authorities_revoked: None,
            }
        })
        .collect();
    launches.sort_by(|a, b| a.mint.cmp(&b.mint));

    let total_locked = launches
        .iter()
        .filter(|e| e.status != STATUS_RELEASED)
        .map(|e| e.locked_amount)
        .sum();

    Registry {
        program_id: program_id.to_string(),
        generated_at_slot: current_slot,
        total_launches: launches.len(),
        total_locked,
        launches,
    }
}

/// Fill in the `--verify` fields of a registry from separately
/// fetched vault and mint accounts. This re-checks the registration
/// invariants against live account data instead of trusting the
/// record: the vault must hold at least the locked amount while the
/// lock is outstanding (and be empty once released), and both mint
/// authorities must be revoked. An entry whose vault or mint account
/// is missing or unparsable keeps `None` fields — the registry
/// reports what it could not check rather than guessing.
pub fn verify_registry(
    registry: &mut Registry,
    vault_accounts: &HashMap<Pubkey, Account>,
    mint_accounts: &HashMap<Pubkey, Account>,
) {
    for entry in &mut registry.launches {
        let vault_pubkey: Pubkey = match entry.vault.parse() {
            Ok(pk) => pk,
            Err(_) => continue,
        };
        if let Some(vault) = vault_accounts.get(&vault_pubkey) {
            if let Ok(token_account) = TokenAccount::unpack(&vault.data) {
                let balance = token_account.amount;
                entry.vault_balance = Some(balance);
                entry.vault_verified = Some(if entry.status == STATUS_RELEASED {
                    balance == 0
                } else {
                    balance >= entry.locked_amount
                });
            }
        }
        let mint_pubkey: Pubkey = match entry.mint.parse() {
            Ok(pk) => pk,
            Err(_) => continue,
        };
        if let Some(mint_account) = mint_accounts.get(&mint_pubkey) {
            if let Ok(mint) = Mint::unpack(&mint_account.data) {
                entry.authorities_revoked = Some(
                    mint.mint_authority.is_none() && mint.freeze_authority.is_none(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshSerialize;
    use solana_program::program_option::COption;

    fn test_program_id() -> Pubkey {
        Pubkey::new_from_array([77; 32])
    }

    fn account(owner: Pubkey, data: Vec<u8>) -> Account {
        Account {
            lamports: 1_000_000,
            data,
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    fn record_account(
        mint: Pubkey,
        creator: Pubkey,
        state: u8,
        lock_until: u64,
    ) -> (Pubkey, Account) {
        let record = LaunchRecord {
            discriminator: DISCRIMINATOR_LAUNCH,
            mint,
            creator,
            vault: Pubkey::new_unique(),
            total_supply: 1_000_000_000,
            locked_amount: 100_000_000,
            lock_until_slot: lock_until,
            registered_slot: 3,
            state,
        };
        let pda = Pubkey::find_program_address(
            &[b"launch", mint.as_ref()],
            &test_program_id(),
        )
        .0;
        (pda, account(test_program_id(), borsh::to_vec(&record).unwrap()))
    }

    fn reputation_account(creator: Pubkey, registered: u64, completed: u64) -> (Pubkey, Account) {
        let rep = CreatorReputation {
            discriminator: DISCRIMINATOR_REPUTATION,
            creator,
            launches_registered: registered,
            locks_completed: completed,
            first_launch_slot: 3,
            last_completed_slot: if completed > 0 { 6_480_003 } else { 0 },
        };
        let pda = Pubkey::find_program_address(
            &[b"reputation", creator.as_ref()],
            &test_program_id(),
        )
        .0;
        (pda, account(test_program_id(), borsh::to_vec(&rep).unwrap()))
    }

    fn mint_account_data(revoked: bool) -> Vec<u8> {
        let mint = Mint {
            mint_authority: if revoked {
                COption::None
            } else {
                COption::Some(Pubkey::new_unique())
            },
            supply: 1_000_000_000,
            decimals: 0,
            is_initialized: true,
            freeze_authority: if revoked {
                COption::None
            } else {
                COption::Some(Pubkey::new_unique())
            },
        };
        let mut data = vec![0u8; Mint::LEN];
        Mint::pack(mint, &mut data).unwrap();
        data
    }

    fn vault_account_data(amount: u64) -> Vec<u8> {
        let token_account = TokenAccount {
            mint: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            amount,
            delegate: COption::None,
            state: spl_token::state::AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };
        let mut data = vec![0u8; TokenAccount::LEN];
        TokenAccount::pack(token_account, &mut data).unwrap();
        data
    }

    #[test]
    fn parse_accepts_a_well_formed_record() {
        let mint = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        let (pda, acc) = record_account(mint, creator, STATE_LOCKED, 6_480_003);
        let (parsed_pda, record) =
            parse_launch_record(&pda, &acc, &test_program_id()).expect("record parses");
        assert_eq!(parsed_pda, pda);
        assert_eq!(record.mint, mint);
        assert_eq!(record.creator, creator);
        assert_eq!(record.locked_amount, 100_000_000);
    }

    #[test]
    fn parse_rejects_wrong_owner_wrong_discriminator_and_truncation() {
        let mint = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        let (pda, acc) = record_account(mint, creator, STATE_LOCKED, 6_480_003);

        // Owned by the token program, not LaunchGuard.
        let foreign = account(spl_token::id(), acc.data.clone());
        assert!(parse_launch_record(&pda, &foreign, &test_program_id()).is_none());

        // Right owner and length, wrong discriminator.
        let mut bad_disc = acc.data.clone();
        bad_disc[0] = DISCRIMINATOR_REPUTATION;
        let bad_disc = account(test_program_id(), bad_disc);
        assert!(parse_launch_record(&pda, &bad_disc, &test_program_id()).is_none());

        // Truncated by one byte.
        let mut truncated = acc.data.clone();
        truncated.pop();
        let truncated = account(test_program_id(), truncated);
        assert!(parse_launch_record(&pda, &truncated, &test_program_id()).is_none());

        // Trailing garbage byte.
        let mut trailing = acc.data.clone();
        trailing.push(0);
        let trailing = account(test_program_id(), trailing);
        assert!(parse_launch_record(&pda, &trailing, &test_program_id()).is_none());
    }

    #[test]
    fn parse_reputation_accepts_and_rejects() {
        let creator = Pubkey::new_unique();
        let (pda, acc) = reputation_account(creator, 2, 1);
        let (_, rep) = parse_reputation(&pda, &acc, &test_program_id()).expect("parses");
        assert_eq!(rep.launches_registered, 2);
        assert_eq!(rep.locks_completed, 1);
        // A launch record is not a reputation account.
        let (record_pda, record_acc) =
            record_account(Pubkey::new_unique(), creator, STATE_LOCKED, 6_480_003);
        assert!(parse_reputation(&record_pda, &record_acc, &test_program_id()).is_none());
    }

    #[test]
    fn registry_joins_reputation_and_sorts_by_mint() {
        let creator_a = Pubkey::new_unique();
        let creator_b = Pubkey::new_unique();
        let mint_a = Pubkey::new_unique();
        let mint_b = Pubkey::new_unique();
        let mut accounts = vec![
            record_account(mint_b, creator_b, STATE_LOCKED, 6_480_003),
            reputation_account(creator_b, 1, 0),
            record_account(mint_a, creator_a, STATE_LOCKED, 6_480_003),
            reputation_account(creator_a, 3, 2),
        ];
        // A foreign account (a mint) must be ignored.
        accounts.push((
            mint_a,
            account(spl_token::id(), mint_account_data(true)),
        ));
        let registry = build_registry(&accounts, &test_program_id(), 100);
        assert_eq!(registry.total_launches, 2);
        assert_eq!(registry.total_locked, 200_000_000);
        assert_eq!(registry.launches[0].mint, mint_a.to_string());
        assert_eq!(registry.launches[1].mint, mint_b.to_string());
        assert_eq!(registry.launches[0].locked_pct_bps, 1_000);
        assert_eq!(registry.launches[0].status, STATUS_LOCKED);
        assert_eq!(
            registry.launches[0].reputation.as_ref().unwrap().launches_registered,
            3
        );
        assert_eq!(
            registry.launches[0].reputation.as_ref().unwrap().locks_completed,
            2
        );
        assert_eq!(
            registry.launches[1].reputation.as_ref().unwrap().launches_registered,
            1
        );
    }

    #[test]
    fn registry_statuses_cover_released_and_expired() {
        let creator = Pubkey::new_unique();
        let accounts = vec![
            record_account(Pubkey::new_unique(), creator, STATE_RELEASED, 6_480_003),
            record_account(Pubkey::new_unique(), creator, STATE_LOCKED, 100),
            record_account(Pubkey::new_unique(), creator, STATE_LOCKED, 6_480_003),
        ];
        let registry = build_registry(&accounts, &test_program_id(), 500);
        let statuses: Vec<&str> = registry
            .launches
            .iter()
            .map(|e| e.status.as_str())
            .collect();
        assert!(statuses.contains(&STATUS_RELEASED));
        assert!(statuses.contains(&STATUS_EXPIRED_UNRELEASED));
        assert!(statuses.contains(&STATUS_LOCKED));
        // Released locks no longer count toward total_locked.
        assert_eq!(registry.total_locked, 200_000_000);
    }

    #[test]
    fn registry_without_reputation_reports_none() {
        let accounts = vec![record_account(
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            STATE_LOCKED,
            6_480_003,
        )];
        let registry = build_registry(&accounts, &test_program_id(), 100);
        assert_eq!(registry.total_launches, 1);
        assert!(registry.launches[0].reputation.is_none());
    }

    #[test]
    fn registry_is_empty_without_launch_accounts() {
        let registry = build_registry(&[], &test_program_id(), 100);
        assert_eq!(registry.total_launches, 0);
        assert_eq!(registry.total_locked, 0);
        assert!(registry.launches.is_empty());
    }

    #[test]
    fn registry_serializes_to_stable_json() {
        let creator = Pubkey::new_unique();
        let accounts = vec![
            record_account(Pubkey::new_unique(), creator, STATE_LOCKED, 6_480_003),
            reputation_account(creator, 1, 0),
        ];
        let registry = build_registry(&accounts, &test_program_id(), 100);
        let json = serde_json::to_string_pretty(&registry).unwrap();
        assert!(json.contains("\"status\": \"locked\""));
        assert!(json.contains("\"locked_pct_bps\": 1000"));
        assert!(json.contains("\"vault_balance\": null"));
        // Round-trip: what we emit, we can read back exactly.
        let back: Registry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, registry);
    }

    #[test]
    fn verify_checks_vault_balance_and_revoked_authorities() {
        let creator = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (record_pda, record_acc) = record_account(mint, creator, STATE_LOCKED, 6_480_003);
        let record = LaunchRecord::try_from_slice(&record_acc.data).unwrap();
        let mut registry = build_registry(&[(record_pda, record_acc)], &test_program_id(), 100);

        let mut vaults = HashMap::new();
        vaults.insert(
            record.vault,
            account(spl_token::id(), vault_account_data(100_000_000)),
        );
        let mut mints = HashMap::new();
        mints.insert(mint, account(spl_token::id(), mint_account_data(true)));
        verify_registry(&mut registry, &vaults, &mints);
        let entry = &registry.launches[0];
        assert_eq!(entry.vault_balance, Some(100_000_000));
        assert_eq!(entry.vault_verified, Some(true));
        assert_eq!(entry.authorities_revoked, Some(true));

        // Short vault: the lock is NOT covered.
        let mut vaults = HashMap::new();
        vaults.insert(
            record.vault,
            account(spl_token::id(), vault_account_data(1)),
        );
        let mut registry2 = build_registry(
            &[record_account(mint, creator, STATE_LOCKED, 6_480_003)],
            &test_program_id(),
            100,
        );
        // The second record has a fresh random vault, so re-derive it.
        let vault_pk: Pubkey = registry2.launches[0].vault.parse().unwrap();
        vaults.clear();
        vaults.insert(
            vault_pk,
            account(spl_token::id(), vault_account_data(1)),
        );
        verify_registry(&mut registry2, &vaults, &mints);
        assert_eq!(registry2.launches[0].vault_verified, Some(false));

        // Live mint authority: not revoked.
        let mut mints = HashMap::new();
        mints.insert(mint, account(spl_token::id(), mint_account_data(false)));
        let mut registry3 = build_registry(
            &[record_account(mint, creator, STATE_LOCKED, 6_480_003)],
            &test_program_id(),
            100,
        );
        let vault_pk: Pubkey = registry3.launches[0].vault.parse().unwrap();
        let mut vaults = HashMap::new();
        vaults.insert(
            vault_pk,
            account(spl_token::id(), vault_account_data(100_000_000)),
        );
        verify_registry(&mut registry3, &vaults, &mints);
        assert_eq!(registry3.launches[0].authorities_revoked, Some(false));

        // Missing accounts: fields stay null, never guessed.
        let mut registry4 = build_registry(
            &[record_account(mint, creator, STATE_LOCKED, 6_480_003)],
            &test_program_id(),
            100,
        );
        verify_registry(&mut registry4, &HashMap::new(), &HashMap::new());
        assert_eq!(registry4.launches[0].vault_balance, None);
        assert_eq!(registry4.launches[0].vault_verified, None);
        assert_eq!(registry4.launches[0].authorities_revoked, None);
    }
}
