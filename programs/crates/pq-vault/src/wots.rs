//! WOTS+ one-time signatures — the RFC 8391 parameter set
//! (SHA-256, n = 32, w = 16, len = 67), implemented exactly as
//! specified there: PRF-derived chain secrets, randomized hash
//! chains driven by F with keyAndMask 0/1, base-w message digits
//! plus the RFC checksum digits.
//!
//! Only `verify` is called by the processor. `commitment` and
//! `sign` are the off-chain signer side — they live in the same
//! module so the construction the program verifies against is the
//! construction the signer uses, auditable byte-for-byte in one
//! place (and exercised by the test suite).
//!
//! Construction choices the design note (programs/PQ-VAULT.md)
//! leaves to this code:
//!
//! - ADRS is the RFC 8391 32-byte address: layer 0, tree 0,
//!   type 0 (OTS hash address). The OTS address is the vault's
//!   key index, so each of a vault's one-time keys gets its own
//!   chain randomization — the role the leaf index plays for a
//!   WOTS+ instance inside XMSS. Verification therefore takes
//!   the key index from the vault's state, never from the
//!   signature: a signature is only ever checked as the key the
//!   program is currently willing to spend.
//! - The WOTS+ public seed is a fixed, public protocol constant
//!   (derived below from its domain string), shared by all
//!   vaults — the same role PK.seed plays for a whole XMSS tree.
//!   It is not secret and protects nothing by itself; the
//!   randomization it drives is domain separation. Every key's
//!   secrets come from the owner's private seed.
//! - The commitment stored on-chain is SHA-256 over the 67 chain
//!   ends concatenated — the compression the design note
//!   specifies ("hashes the 67 ends into the commitment"), not
//!   the XMSS L-tree.

use solana_program::hash::{hash, hashv};

/// Hash output size in bytes (RFC 8391 `n`).
pub const N: usize = 32;
/// Winternitz parameter (RFC 8391 `w`): digits are 4 bits.
pub const W: u32 = 16;
/// Chains for the 256-bit message digest.
pub const LEN1: usize = 64;
/// Checksum chains.
pub const LEN2: usize = 3;
/// Total chains.
pub const LEN: usize = LEN1 + LEN2;
/// Signature size: one 32-byte value per chain — 2,144 bytes.
pub const SIG_BYTES: usize = LEN * N;

/// Domain string the shared public seed is derived from.
const PUB_SEED_DOMAIN: &[u8] = b"MACHINE-PQ-VAULT-V1-PUBSEED";

/// The WOTS+ public seed (RFC 8391 `PK.seed`): a fixed public
/// constant, SHA-256 of its domain string.
pub fn pub_seed() -> [u8; N] {
    hash(PUB_SEED_DOMAIN).to_bytes()
}

/// RFC 8391 ADRS: a 32-byte address. Layer (bytes 0..4), tree
/// (4..12) and type (12..16) are zero throughout this program —
/// a single flat key space, OTS hash addresses only. The fields
/// that move are the OTS address (the key index), the chain
/// address, the hash address (the step inside a chain) and
/// keyAndMask.
#[derive(Clone, Copy)]
struct Adrs([u8; 32]);

impl Adrs {
    fn new(ots_address: u32) -> Self {
        let mut adrs = Adrs([0u8; 32]);
        adrs.set_ots_address(ots_address);
        adrs
    }
    fn set_ots_address(&mut self, v: u32) {
        self.0[16..20].copy_from_slice(&v.to_be_bytes());
    }
    fn set_chain_address(&mut self, v: u32) {
        self.0[20..24].copy_from_slice(&v.to_be_bytes());
    }
    fn set_hash_address(&mut self, v: u32) {
        self.0[24..28].copy_from_slice(&v.to_be_bytes());
    }
    fn set_key_and_mask(&mut self, v: u32) {
        self.0[28..32].copy_from_slice(&v.to_be_bytes());
    }
}

/// RFC 8391 F: SHA-256(toByte(0, 32) || KEY || M). Passed to
/// the hash syscall as three slices — assembling one buffer
/// first was measured and is slower (the copies cost more
/// than the slice marshaling; see programs/PQ-VERIFY.md).
fn f(key: &[u8; N], msg: &[u8; N]) -> [u8; N] {
    const PAD: [u8; N] = [0u8; N];
    hashv(&[&PAD, key, msg]).to_bytes()
}

/// RFC 8391 PRF: SHA-256(toByte(3, 32) || KEY || M).
fn prf(key: &[u8; N], msg: &[u8; N]) -> [u8; N] {
    let mut pad = [0u8; N];
    pad[31] = 3;
    hashv(&[&pad, key, msg]).to_bytes()
}

/// One randomized hash chain (RFC 8391 `chain`): starting from
/// `x` at position `start`, take `steps` randomized steps. Each
/// step derives a fresh key and bitmask from the public seed and
/// the current address, so the same chain value at a different
/// position or in a different key hashes differently.
fn chain(
    mut x: [u8; N],
    start: u32,
    steps: u32,
    pub_seed: &[u8; N],
    adrs: &mut Adrs,
) -> [u8; N] {
    for j in start..start + steps {
        adrs.set_hash_address(j);
        adrs.set_key_and_mask(0);
        let key = f(pub_seed, &adrs.0);
        adrs.set_key_and_mask(1);
        let mask = f(pub_seed, &adrs.0);
        for i in 0..N {
            x[i] ^= mask[i];
        }
        x = f(&key, &x);
    }
    x
}

/// The secret element for one chain of one key:
/// PRF(seed, ADRS) with the chain address set and everything
/// else at its base value. The seed itself never leaves the
/// owner's machine; only chain intermediates derived from it
/// are ever revealed.
fn secret_element(seed: &[u8; N], key_index: u32, chain_index: u32) -> [u8; N] {
    let mut adrs = Adrs::new(key_index);
    adrs.set_chain_address(chain_index);
    adrs.set_hash_address(0);
    adrs.set_key_and_mask(0);
    prf(seed, &adrs.0)
}

/// The 67 base-w digits a signature reveals chain positions
/// for: the digest's 64 nibbles (most significant first), then
/// the 3 checksum digits. The checksum is
/// sum(15 - digit_i) over the message digits, left-shifted by
/// 8 - ((len_2 * lg(w)) mod 8) = 4 bits per RFC 8391, then read
/// as base-w digits. The checksum is what makes a second
/// signature with the same key forgeable only at the cost of
/// increasing some digit — which the checksum then forbids.
pub fn message_digits(digest: &[u8; N]) -> [u8; LEN] {
    let mut digits = [0u8; LEN];
    for i in 0..LEN1 {
        let byte = digest[i / 2];
        digits[i] = if i % 2 == 0 { byte >> 4 } else { byte & 0x0f };
    }
    let mut csum: u32 = 0;
    for d in digits.iter().take(LEN1) {
        csum += (W - 1) - (*d as u32);
    }
    csum <<= 4;
    let bytes = [(csum >> 8) as u8, (csum & 0xff) as u8];
    digits[LEN1] = bytes[0] >> 4;
    digits[LEN1 + 1] = bytes[0] & 0x0f;
    digits[LEN1 + 2] = bytes[1] >> 4;
    digits
}

/// Signer side: the public-key commitment for one key — every
/// chain run to its end (15 steps), the 67 ends hashed into the
/// single 32-byte value the vault stores on-chain.
pub fn commitment(seed: &[u8; N], key_index: u32) -> [u8; N] {
    let ps = pub_seed();
    let mut ends = [[0u8; N]; LEN];
    for i in 0..LEN {
        let sk = secret_element(seed, key_index, i as u32);
        let mut adrs = Adrs::new(key_index);
        adrs.set_chain_address(i as u32);
        ends[i] = chain(sk, 0, W - 1, &ps, &mut adrs);
    }
    let refs: Vec<&[u8]> = ends.iter().map(|e| &e[..]).collect();
    hashv(&refs).to_bytes()
}

/// Signer side: sign `digest` with key `key_index`. Reveals,
/// for each chain, the value at the position the corresponding
/// digit selects. This key must never sign again — the program
/// enforces that on-chain by consuming the index.
pub fn sign(seed: &[u8; N], key_index: u32, digest: &[u8; N]) -> [u8; SIG_BYTES] {
    let ps = pub_seed();
    let digits = message_digits(digest);
    let mut sig = [0u8; SIG_BYTES];
    for i in 0..LEN {
        let sk = secret_element(seed, key_index, i as u32);
        let mut adrs = Adrs::new(key_index);
        adrs.set_chain_address(i as u32);
        let revealed = chain(sk, 0, digits[i] as u32, &ps, &mut adrs);
        sig[i * N..(i + 1) * N].copy_from_slice(&revealed);
    }
    sig
}

/// Program side: verify `signature` over `digest` as key
/// `key_index` against the stored `commitment`. Completes every
/// chain from the revealed position to its end and re-hashes
/// the ends; a match means the signer held the chain secrets
/// for exactly this key index and this digest.
pub fn verify(
    signature: &[u8],
    digest: &[u8; N],
    key_index: u32,
    commitment: &[u8; N],
) -> bool {
    if signature.len() != SIG_BYTES {
        return false;
    }
    let ps = pub_seed();
    let digits = message_digits(digest);
    let mut ends = [[0u8; N]; LEN];
    for i in 0..LEN {
        let mut revealed = [0u8; N];
        revealed.copy_from_slice(&signature[i * N..(i + 1) * N]);
        let mut adrs = Adrs::new(key_index);
        adrs.set_chain_address(i as u32);
        ends[i] = chain(
            revealed,
            digits[i] as u32,
            (W - 1) - digits[i] as u32,
            &ps,
            &mut adrs,
        );
    }
    let refs: Vec<&[u8]> = ends.iter().map(|e| &e[..]).collect();
    hashv(&refs).to_bytes() == *commitment
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_digest() -> [u8; N] {
        hash(b"pq-vault wots test digest").to_bytes()
    }

    #[test]
    fn message_digits_known_values() {
        // All-zero digest: message digits all 0, checksum is
        // 64 * 15 = 960, shifted to 15360 = 0x3C00, whose base-w
        // digits are [3, 12, 0].
        let digits = message_digits(&[0u8; N]);
        assert!(digits[..LEN1].iter().all(|&d| d == 0));
        assert_eq!(&digits[LEN1..], &[3, 12, 0]);
        // All-ones digest: message digits all 15, checksum 0.
        let digits = message_digits(&[0xffu8; N]);
        assert!(digits[..LEN1].iter().all(|&d| d == 15));
        assert_eq!(&digits[LEN1..], &[0, 0, 0]);
        // Nibbles are read most-significant first.
        let mut digest = [0u8; N];
        digest[0] = 0xab;
        let digits = message_digits(&digest);
        assert_eq!(digits[0], 10);
        assert_eq!(digits[1], 11);
    }

    #[test]
    fn sign_verify_round_trip() {
        let seed = [7u8; N];
        let digest = test_digest();
        let c = commitment(&seed, 3);
        let sig = sign(&seed, 3, &digest);
        assert_eq!(sig.len(), SIG_BYTES);
        assert!(verify(&sig, &digest, 3, &c));
    }

    #[test]
    fn verify_rejects_tampered_and_foreign_signatures() {
        let seed = [7u8; N];
        let other_seed = [9u8; N];
        let digest = test_digest();
        let c = commitment(&seed, 0);
        let sig = sign(&seed, 0, &digest);

        // One corrupted chain value anywhere in the signature.
        let mut tampered = sig;
        tampered[10 * N + 5] ^= 0x01;
        assert!(!verify(&tampered, &digest, 0, &c));

        // A genuine signature, but from a different seed.
        let foreign = sign(&other_seed, 0, &digest);
        assert!(!verify(&foreign, &digest, 0, &c));

        // A truncated signature is not a signature.
        assert!(!verify(&sig[..SIG_BYTES - N], &digest, 0, &c));
    }

    #[test]
    fn verify_binds_digest_and_key_index() {
        let seed = [7u8; N];
        let digest = test_digest();
        let c0 = commitment(&seed, 0);
        let sig = sign(&seed, 0, &digest);

        // Same signature, different message.
        let other_digest = hash(b"a different withdrawal").to_bytes();
        assert!(!verify(&sig, &other_digest, 0, &c0));

        // Same signature presented as the next key: the chain
        // randomization is addressed by key index, so it does
        // not verify as key 1 even against key 0's commitment —
        // and key 1 has its own commitment, which it also fails.
        let c1 = commitment(&seed, 1);
        assert!(!verify(&sig, &digest, 1, &c0));
        assert!(!verify(&sig, &digest, 1, &c1));
    }
}
