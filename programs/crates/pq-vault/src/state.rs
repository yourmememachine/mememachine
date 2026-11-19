//! PQ Vault state, instructions, errors and parameters.
//! Design: programs/PQ-VAULT.md.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{hash::hashv, program_error::ProgramError, pubkey::Pubkey};

use crate::wots::SIG_BYTES;

pub const SEED_VAULT: &[u8] = b"pq-vault";
pub const SEED_TOKEN_VAULT: &[u8] = b"pq-token-vault";
pub const SEED_AUTHORITY: &[u8] = b"pq-vault-authority";
pub const SEED_SIG: &[u8] = b"pq-sig";

pub const DISCRIMINATOR_VAULT: [u8; 8] = *b"PQVAULT1";
pub const DISCRIMINATOR_SIG: [u8; 8] = *b"PQSIG001";

pub const STATE_ACTIVE: u8 = 1;
pub const STATE_CLOSED: u8 = 2;

/// Maximum one-time keys per commitment set. Deliberately small
/// in v1 — the audited surface stays small (the LaunchGuard
/// rule); the design note's XMSS Merkle-root extension is the
/// documented path past it.
pub const MAX_KEYS: usize = 16;

/// Largest signature chunk one WriteSigChunk carries. A whole
/// transaction is capped at 1,232 bytes; a chunk near this cap
/// plus the transaction's signatures and accounts still fits.
pub const MAX_CHUNK_BYTES: usize = 1_000;

/// Domain separator for the withdrawal digest, as specified in
/// the design note.
pub const DOMAIN_WITHDRAW: &[u8] = b"MACHINE-PQ-VAULT-V1";
/// Domain separator for the rotation digest. Rotation signs a
/// different shape than a withdrawal, so it gets its own domain:
/// a signature made for one can never be presented as the other.
pub const DOMAIN_ROTATE: &[u8] = b"MACHINE-PQ-VAULT-V1-ROTATE";

/// The vault account. Holds no funds itself; the funds sit in
/// `token_vault`, an SPL token account owned by the authority
/// PDA. The creating Ed25519 key (`owner`) is payer and creator
/// only — it appears nowhere in the withdrawal path, by design:
/// withdrawals are authorized by WOTS+ signatures or not at all.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub struct VaultAccount {
    pub discriminator: [u8; 8],
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub token_vault: Pubkey,
    /// Committed one-time keys in the current set, 1..=MAX_KEYS.
    pub keys_total: u8,
    /// The only key index the program will accept next. Spends
    /// are strictly in order; the index advances in the same
    /// transaction that spends, before any token moves, and no
    /// instruction decrements, resets (outside rotation, which
    /// replaces the whole set), or edits it.
    pub next_key_index: u32,
    pub state: u8,
    /// One 32-byte WOTS+ public-key commitment per key slot;
    /// unused slots are zero.
    pub commitments: [[u8; 32]; MAX_KEYS],
    pub vault_bump: u8,
    pub token_vault_bump: u8,
}

impl VaultAccount {
    pub const LEN: usize = 8 + 32 + 32 + 32 + 1 + 4 + 1 + (MAX_KEYS * 32) + 1 + 1;
}

/// The signature buffer account's header. The account is mostly
/// a data blob — a 2,144-byte WOTS+ signature uploaded in
/// chunks — so the header is a fixed byte layout in front of the
/// signature region rather than a borsh struct:
///
/// ```text
///  0..8    discriminator
///  8..40   vault pubkey
/// 40..44   key_index (u32, big-endian)
/// 44..76   digest the signature must authorize
/// 76..78   written_len (u16, big-endian)
/// 78..     the signature bytes, exactly SIG_BYTES long
/// ```
///
/// The digest binds the buffer to one exact operation at
/// creation: chunks uploaded for one withdrawal cannot be
/// executed against another, because execution re-derives the
/// digest from its own parameters and requires equality.
pub struct SigBuffer;

impl SigBuffer {
    pub const HEADER_LEN: usize = 8 + 32 + 4 + 32 + 2;
    pub const LEN: usize = Self::HEADER_LEN + SIG_BYTES;

    pub fn write_header(
        data: &mut [u8],
        vault: &Pubkey,
        key_index: u32,
        digest: &[u8; 32],
        written_len: u16,
    ) {
        data[0..8].copy_from_slice(&DISCRIMINATOR_SIG);
        data[8..40].copy_from_slice(vault.as_ref());
        data[40..44].copy_from_slice(&key_index.to_be_bytes());
        data[44..76].copy_from_slice(digest);
        data[76..78].copy_from_slice(&written_len.to_be_bytes());
    }

    pub fn vault(data: &[u8]) -> Pubkey {
        Pubkey::new_from_array(data[8..40].try_into().expect("buffer header"))
    }

    pub fn key_index(data: &[u8]) -> u32 {
        u32::from_be_bytes(data[40..44].try_into().expect("buffer header"))
    }

    pub fn digest(data: &[u8]) -> [u8; 32] {
        data[44..76].try_into().expect("buffer header")
    }

    pub fn written_len(data: &[u8]) -> u16 {
        u16::from_be_bytes(data[76..78].try_into().expect("buffer header"))
    }

    pub fn set_written_len(data: &mut [u8], written_len: u16) {
        data[76..78].copy_from_slice(&written_len.to_be_bytes());
    }

    pub fn signature(data: &[u8]) -> &[u8] {
        &data[Self::HEADER_LEN..Self::HEADER_LEN + SIG_BYTES]
    }

    pub fn signature_mut(data: &mut [u8]) -> &mut [u8] {
        &mut data[Self::HEADER_LEN..Self::HEADER_LEN + SIG_BYTES]
    }
}

/// The withdrawal digest, exactly as the design note specifies:
/// SHA-256 over the domain separator and every parameter of the
/// withdrawal, fixed-width, so changing the destination or the
/// amount by one lamport produces a digest the signature does
/// not authorize.
pub fn withdrawal_digest(
    vault: &Pubkey,
    key_index: u32,
    destination: &Pubkey,
    amount: u64,
    mint: &Pubkey,
) -> [u8; 32] {
    hashv(&[
        DOMAIN_WITHDRAW,
        vault.as_ref(),
        &key_index.to_be_bytes(),
        destination.as_ref(),
        &amount.to_be_bytes(),
        mint.as_ref(),
    ])
    .to_bytes()
}

/// Hash of a commitment set, as bound into the rotation digest:
/// the key count followed by every commitment in order. The
/// count is part of the hash, so a set and a prefix of it hash
/// differently.
pub fn commitment_set_hash(commitments: &[[u8; 32]]) -> [u8; 32] {
    let mut parts: Vec<&[u8]> = Vec::with_capacity(commitments.len() + 1);
    let count = [commitments.len() as u8];
    parts.push(&count);
    for c in commitments {
        parts.push(c);
    }
    hashv(&parts).to_bytes()
}

/// The rotation digest: binds the vault, the key authorizing
/// the rotation, and the exact new commitment set being
/// installed.
pub fn rotation_digest(vault: &Pubkey, key_index: u32, new_set_hash: &[u8; 32]) -> [u8; 32] {
    hashv(&[
        DOMAIN_ROTATE,
        vault.as_ref(),
        &key_index.to_be_bytes(),
        new_set_hash,
    ])
    .to_bytes()
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq)]
pub enum PqInstruction {
    /// Create the vault PDA and its token account, storing
    /// 1..=16 WOTS+ public-key commitments generated off-chain.
    /// The private seeds never touch the chain. Fails on an
    /// empty or oversized commitment set.
    CreateVault { commitments: Vec<[u8; 32]> },
    /// Create the signature buffer for the vault's next key,
    /// bound to the digest of the one operation it serves (a
    /// withdrawal or a rotation — the digest decides which).
    CreateSigBuffer { key_index: u32, digest: [u8; 32] },
    /// Upload signature bytes at `offset`. Chunks are appended
    /// strictly in order (offset must equal the bytes written
    /// so far): upload order is a client concern, and a single
    /// contiguous written length is what execution checks.
    /// (The design note allows chunks to arrive in any order;
    /// v1 narrows that to sequential appends — see PQ-VAULT.md.)
    WriteSigChunk { offset: u16, bytes: Vec<u8> },
    /// The only instruction that moves funds. Recomputes the
    /// withdrawal digest from the actual parameters, requires
    /// the buffer's key to be the vault's next key and the
    /// buffer complete, verifies the WOTS+ signature against
    /// the stored commitment, increments the key index BEFORE
    /// transferring, then transfers and closes the buffer.
    ExecuteWithdrawal { amount: u64 },
    /// Install a fresh commitment set, authorized by a WOTS+
    /// signature from the next unconsumed key over the rotation
    /// digest (delivered through the same buffer flow). Consumes
    /// that key. There is deliberately no Ed25519-authorized
    /// rotation: an Ed25519 path to new keys would be an
    /// Ed25519 path to the funds.
    RotateKeys { new_commitments: Vec<[u8; 32]> },
    /// Vault owner only: close a signature buffer without
    /// executing it and reclaim its rent. Moves no funds. The
    /// owner gate is also the anti-squatting rule: anyone may
    /// create a buffer for a vault (they pay its rent), but only
    /// the owner can clear one, so a squatted buffer can always
    /// be swept away by the person it was meant to block.
    AbortSigBuffer {},
    /// Vault owner only, and only with a zero token balance:
    /// close the token account (rent to the owner) and mark the
    /// vault Closed. Closing an empty vault moves nothing, so
    /// Ed25519 authorization is sufficient here — the one place
    /// it appears, and it can never reach funds.
    CloseVault {},
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u32)]
pub enum PqError {
    /// CreateVault or RotateKeys carried 0 or more than
    /// MAX_KEYS commitments.
    InvalidCommitmentCount = 1,
    /// The vault is Closed; every operation on it is over.
    VaultClosed = 2,
    /// Every committed key has been spent. Withdrawals and
    /// rotations are both impossible — rotation needed one
    /// unconsumed key to authorize it. The remaining balance is
    /// frozen, fail-closed, by design.
    KeysExhausted = 3,
    /// The buffer's key index is not the vault's next key index:
    /// keys are spent strictly in order, so a signature for any
    /// other key — earlier or later — is worthless here.
    WrongKeyIndex = 4,
    /// Execution was attempted before the full 2,144-byte
    /// signature was uploaded.
    BufferIncomplete = 5,
    /// The digest re-derived from the execution's actual
    /// parameters does not equal the digest the buffer (and the
    /// signature) was bound to.
    DigestMismatch = 6,
    /// The WOTS+ signature does not verify against the stored
    /// commitment for the next key.
    SignatureInvalid = 7,
    /// An account is not the PDA its position requires.
    InvalidPda = 8,
    /// An owner-only instruction was called by someone else.
    NotOwner = 9,
    /// A withdrawal of zero moves nothing and must not consume
    /// a one-time key.
    ZeroAmount = 10,
    /// The token vault holds less than the withdrawal amount.
    InsufficientBalance = 11,
    /// An account's mint is not the vault's mint.
    MintMismatch = 12,
    /// The withdrawal destination is the token vault itself.
    InvalidDestination = 13,
    /// A chunk's offset is not the number of bytes written so
    /// far: chunks append strictly in order.
    ChunkOutOfOrder = 14,
    /// A chunk is empty, larger than MAX_CHUNK_BYTES, or runs
    /// past the end of the signature region.
    ChunkOverflow = 15,
    /// The token program account (or an account's owner) is not
    /// the classic SPL Token program. Token-2022 is rejected in
    /// v1 for the same reason as in LaunchGuard: its extensions
    /// can reintroduce control this program exists to remove,
    /// and the classic layouts parsed here do not describe it.
    UnsupportedTokenProgram = 16,
    /// CloseVault was called while the token vault still holds
    /// funds. Withdraw them first (with WOTS+ signatures);
    /// there is no Ed25519 path to them, including this one.
    VaultNotEmpty = 17,
}

impl From<PqError> for ProgramError {
    fn from(e: PqError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
