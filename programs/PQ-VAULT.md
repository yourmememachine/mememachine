# PQ Vault — a quantum-safe cold-storage vault for Machine (design note)

**Status: design only. No code exists yet.** This note is step (a)
of backlog item #12: the specification for an opt-in vault program
whose withdrawals are authorized by hash-based one-time signatures
instead of Ed25519. Step (b) is a prototype program built to this
note. Nothing here is deployed, and nothing in this note should be
read as "Machine is quantum-resistant" — see the honest limits at
the end for exactly what this design does and does not protect.

## Why this exists

Machine's accounts, like Solana's, are Ed25519 keys — and on this
chain family an address **is** the public key, exposed from the
moment the address exists. Ed25519 falls to Shor's algorithm on a
cryptographically relevant quantum computer: from a public key, such
a machine derives the private key. Funds sitting still for years in
a known address are the textbook target — the key is already public,
there is nothing left to "harvest" later; the forgery can happen the
day the machine exists.

Cold storage is the one place this matters first, and the one place
the fix is cheap: cold funds move rarely, so they can afford a
signature scheme that is large, slow, and single-use — the three
things a hot wallet cannot tolerate.

Hash-based signatures are that scheme. Their security rests only on
the preimage resistance of a hash function. Quantum search
(Grover's algorithm) only halves a hash's effective strength, so
SHA-256 keeps roughly 128 bits of post-quantum preimage resistance.
No lattice assumption, no elliptic curve, nothing for Shor's
algorithm to bite on.

## The threat model, honestly

- **No quantum computer threatens Ed25519 today.** Nothing close to
  the required scale and error correction exists, and nobody
  credible publishes a date for one. This vault is
  **future-proofing for long-lived cold storage, not an emergency
  fix.**
- The planning clock that does exist is policy, not physics: NIST's
  transition report (IR 8547, final, October 2026) deprecates all
  quantum-vulnerable signature algorithms — Ed25519 included —
  after 2030 and disallows them after 2035 for systems that follow
  NIST guidance. That is a federal migration timetable, not a
  prediction of a break, and this note does not treat it as one.
- The design goal is narrow: **a vault whose stored funds stay put
  even if Ed25519 is broken outright** — i.e. there is no Ed25519
  key, anywhere in the withdrawal path, whose compromise moves the
  money.

## Why not just swap the chain's signatures

A chain-wide move to post-quantum signatures is not this program's
job, and at the protocol level it is mostly not possible yet:

| Scheme | Public key | Signature | vs. the 1,232-byte transaction limit |
|---|---|---|---|
| Ed25519 (today) | 32 B | 64 B | fits trivially |
| ML-DSA-44 (FIPS 204) | 1,312 B | 2,420 B | signature alone is ~2× the limit |
| ML-DSA-65 (FIPS 204) | 1,952 B | 3,309 B | ~2.7× the limit |
| SLH-DSA-SHA2-128s (FIPS 205) | 32 B | 7,856 B | ~6.4× the limit |
| FN-DSA-512 / Falcon-512 (FIPS 206, **draft**) | 897 B | ~666 B | fits — but the standard is not final |
| WOTS+ as specified below | 32 B (commitment) | 2,144 B | exceeds one transaction; delivered via a buffer account (see Design) |
| XMSS-SHA2_10_256 (RFC 8391) | 64 B | ~2,500 B | same buffer treatment |

Machine inherits Solana's transaction format from Agave 2.3.13:
a serialized legacy/v0 transaction is capped at **1,232 bytes**
(`PACKET_DATA_SIZE` = 1,280-byte IPv6 minimum MTU minus 48 bytes of
headers), and that budget must also hold the fee-payer signature,
accounts, and instruction data. Every standardized post-quantum
signature except Falcon's exceeds it by itself. (Upstream Agave has
since been developing a 4,096-byte v1 transaction format; even that
fits at most one ML-DSA-44 signature and no SLH-DSA signature, and
it is not in 2.3.13.)

Size is only the first blocker. A protocol swap also breaks every
wallet, explorer, SDK, and tool that assumes 32-byte Ed25519 keys —
the compatibility *is* the product of an Agave fork — and it would
orphan the bridge, whose relayer quorum is verified through the
**ed25519 precompile**. The protocol-layer decision belongs
upstream: the Solana Foundation's published preparation work has
both major client teams (Anza and Firedancer) converging on Falcon,
with early implementations and no migration timetable. If Agave
adopts a post-quantum scheme, Machine inherits it by rebasing. What
a single program can do today — without touching consensus, the
transaction format, or anyone else's keys — is put one vault's
spending authorization behind hashes. That is the whole scope here.

## What the ecosystem already did (prior art, stated plainly)

This design is not the first of its kind, and the note does not
pretend otherwise:

- **January 2025 — the Solana Winternitz Vault** (Dean Little /
  Blueshift): an opt-in Solana program holding funds behind
  Winternitz one-time signatures (truncated Keccak256), with a
  fresh keypair per spend and the vault closing on each spend. It
  proved the shape works on this runtime. Its author's own README
  is candid that the security figures are the author's claims with
  no independent audit. This design differs in construction
  (SHA-256, RFC 8391 parameters, a persistent vault with
  program-enforced key indices instead of close-and-reopen) but
  not in spirit.
- **December 2025 — Solana Foundation × Project Eleven**: a full
  quantum threat assessment of Solana and post-quantum signatures
  running on a Solana testnet. Industry coverage of the details
  conflicts and the scheme/benchmarks were not officially
  published, so this note cites the fact of the exercise, not its
  numbers.
- **April 2026 — Solana Foundation preparation plan** (as reported
  in industry research): Anza and Firedancer independently chose
  Falcon for the protocol layer; as of October 2026 the Foundation
  has said no large-scale migration is being enabled now.

None of this is in Agave 2.3.13, and none of it protects a Machine
vault today. It is evidence the direction is real, not a feature
we inherit.

## Chosen approach: WOTS+ one-time signatures, program-enforced state

The vault uses **WOTS+** — the Winternitz one-time signature from
RFC 8391 (the one-time scheme inside XMSS), with the RFC's
SHA-256 parameter set:

| Parameter | Value | Meaning |
|---|---|---|
| Hash | SHA-256 (`sol_sha256` syscall) | the only cryptographic primitive |
| `n` | 32 bytes | hash output size |
| `w` | 16 | Winternitz parameter (digits are 4 bits) |
| `len_1` | 64 | chains for the 256-bit message digest |
| `len_2` | 3 | checksum chains |
| `len` | 67 | total chains |
| Signature size | 67 × 32 = **2,144 bytes** | one 32-byte value per chain |
| Public key | 67 chain-end values, hashed to a single **32-byte commitment** | only the commitment is stored on-chain |
| Private key | a 32-byte seed (all chain secrets PRF-derived from it) | never leaves the owner's machine |

How a signature works, in one paragraph: the message digest's 64
four-bit digits (plus a 3-digit checksum) each select how far along
its hash chain the signer reveals. Verification completes each
chain to its end and hashes the 67 ends into the commitment; a
match means the signer knew the chain secrets. Revealing chain
intermediates is safe **exactly once** — a second signature with
the same key reveals different points on the same chains, and
between two signatures an attacker can interpolate chain values to
forge. That is why one-time discipline is not a usage guideline
here: **the program enforces it** (next section). Stateful
hash-based schemes are standardized practice — XMSS and LMS are
approved for exactly this use in NIST SP 800-208 — provided the
state can never roll back. On-chain, the state is the account data
itself, written by the program in the same transaction that spends,
which is the strongest place a stateful scheme can live.

Why WOTS+ and not the alternatives:

- **Not SLH-DSA (SPHINCS+).** Its statelessness is the answer to a
  problem this design does not have — the program *is* a state
  machine, so state is free and rollback-proof. In exchange,
  SLH-DSA-128s costs a 7,856-byte signature and an order of
  magnitude more hashes per verification.
- **Not ML-DSA.** A lattice assumption where a hash suffices, a
  2,420-byte signature that still exceeds one transaction, and no
  on-chain verifier short of hand-rolling the full ML-DSA
  verification in SBF — a large, subtle implementation to get
  wrong, for a cold-storage program whose whole virtue is being
  small.
- **Not FN-DSA/Falcon yet.** The only scheme whose signature fits
  a transaction (~666 B), and the upstream client teams' reported
  protocol choice — but FIPS 206 is still a draft (no initial
  public draft published as of late 2026), and Falcon's
  floating-point Gaussian sampling is famously easy to implement
  insecurely. It belongs at the protocol layer, upstream, not
  hand-built inside one vault program.
- **Full XMSS is the scaling path, not v1.** Committing a Merkle
  root over 2^h WOTS+ keys (h = 10 → 1,024 keys, a 320-byte
  authentication path per withdrawal) replaces the flat commitment
  list below when 16 keys per vault is not enough. The vault
  account layout leaves room for that change; v1 stays flat and
  auditable.

### Verification cost (estimate, to be measured in step (b))

Verification completes each chain: the number of SHA-256 calls is
the sum of the remaining chain steps — **~500 on average, at most
1,005** — plus the final hashing of the 67 chain ends into the
commitment. The runtime charges `sol_sha256` at an 85 CU base plus
a per-byte cost (Agave's `compute_budget.rs`), so a 32-byte chain
step costs on the order of 100 CU and a full verification lands
roughly in the **50k–120k CU** range — inside the 200,000 CU
default per-instruction budget, and far inside the 1,400,000 CU
transaction maximum. These are arithmetic estimates from the
syscall cost table, stated as such; step (b) replaces them with
measured figures, the same discipline as the bridge's CU numbers
in BRIDGE.md and bridge/VERIFY.md.

## Design

### Accounts

**Vault account** — PDA, seeds `["pq-vault", owner, mint]`.
Created once by the owner; holds no funds itself.

| Field | Size | Notes |
|---|---|---|
| discriminator | 8 B | program account tag |
| `owner` | 32 B | the creating Ed25519 key — payer and vault creator **only**; it cannot authorize withdrawals |
| `mint` | 32 B | the single SPL mint this vault holds (v1: one mint per vault) |
| `token_vault` | 32 B | the PDA token account holding the funds |
| `keys_total` | 1 B | number of committed one-time keys, 1–16 (`MAX_KEYS = 16`) |
| `next_key_index` | 4 B | the only key index the program will accept next |
| `state` | 1 B | `Active` / `Closed` |
| `commitments` | 16 × 32 B | one 32-byte WOTS+ public-key commitment per key slot (unused slots zeroed) |
| bumps | 2 B | vault + token-vault PDA bumps |

Total ≈ 625 bytes. At the rent parameters inherited from Agave
(rent-exempt minimum = (128 + data bytes) × 6,960 lamports), the
vault account costs roughly 5.2M lamports of rent-exempt balance —
a trivial, one-time amount of $MACHINE.

**Token vault** — a standard SPL token account for `mint`, owned
by the authority PDA (seeds `["pq-vault-authority", vault]`), the
same pattern LaunchGuard uses: only this program, signing as that
PDA, can move the funds, and the program moves them only via the
instructions below.

**Signature buffer account** — PDA, seeds `["pq-sig", vault,
key_index]`, created per withdrawal. A WOTS+ signature (2,144 B)
cannot ride in one 1,232-byte transaction, so it is uploaded in
chunks — the same reason large programs deploy through buffer
accounts. The buffer is bound at creation to the exact withdrawal
it serves:

| Field | Size | Notes |
|---|---|---|
| discriminator | 8 B | |
| `vault` | 32 B | the vault this signature belongs to |
| `key_index` | 4 B | must equal the vault's `next_key_index` at execution |
| `digest` | 32 B | the withdrawal digest (below); chunks cannot be mixed across withdrawals |
| `written_len` | 2 B | bytes uploaded so far; execution requires exactly 2,144 |
| `data` | 2,144 B | the signature, filled by chunk writes |

Rent ≈ 16.4M lamports, **returned in full** when the buffer closes
after execution or abort.

### The withdrawal digest

A WOTS+ signature authorizes exactly one withdrawal because it
signs a digest that binds every parameter of it:

```
digest = SHA-256(
    "MACHINE-PQ-VAULT-V1"   // domain separator
    || vault pubkey
    || key_index            // the one-time key being spent
    || destination          // token account receiving the funds
    || amount               // exact amount, no partial spends of a signature
    || mint
)
```

Change the destination or the amount by one lamport and the
signature does not verify. Replay is impossible by construction:
the program consumes `key_index` in the same transaction, so the
identical signature presented again names a key index the vault no
longer accepts.

### Instructions (v1)

| Instruction | What it does |
|---|---|
| `CreateVault { commitments }` | Owner (Ed25519) creates the vault PDA and its token account, storing 1–16 WOTS+ public-key commitments generated off-chain. The private seeds never touch the chain. Fails if `commitments` is empty or > 16. |
| *(deposit)* | **Not an instruction.** Anyone deposits by making an ordinary SPL transfer into the token vault. Receiving needs no signature scheme at all — deposits are exactly as safe in a post-quantum world as they are today. |
| `CreateSigBuffer { key_index }` | Creates the buffer account bound to this vault, `key_index`, and the digest of the intended withdrawal. |
| `WriteSigChunk { offset, bytes }` | Appends signature bytes at `offset` (≤ ~1,000 bytes per transaction). Chunks may arrive in any order; `written_len` tracks completion. |
| `ExecuteWithdrawal { amount, destination }` | The only instruction that moves funds. Recomputes the digest from the actual parameters and the buffer's stored binding; requires `key_index == vault.next_key_index`, a complete buffer, and a WOTS+ signature that verifies against `commitments[key_index]`. **Increments `next_key_index` before transferring** (state first, fail-closed), then transfers `amount` from the token vault to `destination` with the authority PDA as signer, and closes the buffer. |
| `RotateKeys { new_commitments }` | Installs a fresh commitment set, authorized by a WOTS+ signature (delivered through the same buffer flow) from the **next unconsumed key** over a rotation digest binding the new set's hash. Consumes that key. There is deliberately **no Ed25519-authorized rotation**: an Ed25519 path to new keys would be an Ed25519 path to the funds. |
| `AbortSigBuffer {}` | Closes an incomplete buffer and returns its rent. Moves no funds. |
| `CloseVault {}` | Owner (Ed25519), only when the token balance is zero. Closing an empty vault moves nothing, so Ed25519 authorization is sufficient here. |

**Key exhaustion is a hard stop, by design.** When
`next_key_index == keys_total`, no withdrawal can execute. The
escape hatch is `RotateKeys` *before* the last key is spent (the
rotation itself consumes one key and installs the next set). An
owner who spends the final key without chaining a rotation has
frozen the remainder permanently — fail-closed, the same way
LaunchGuard's locks cannot be shortened. For cold storage this is
the correct failure mode: the alternative (any recovery path) is a
second authorization path, and a second authorization path is the
vulnerability.

### One-time discipline, enforced by the program

The rule "a key signs exactly once" is not documentation — it is
the `next_key_index` field:

1. Keys are spent **strictly in order**. A withdrawal naming any
   index other than `next_key_index` fails, so a key cannot be
   skipped, reordered, or reused.
2. The index advances **in the same transaction** that verifies
   the signature, before any token moves. A failed transfer cannot
   leave a spent key unspent; a succeeded one cannot be replayed.
3. There is no instruction that decrements, resets, or edits the
   index or a stored commitment — rotation replaces the whole set
   atomically and only on a one-time signature's authority.
4. The signer side is off-chain and remains the owner's
   responsibility (generate keys from a proper seed, sign once,
   destroy or retire spent seeds). The program cannot stop an
   owner from *signing* twice off-chain — it can and does make the
   second signature worthless on-chain, because its index is
   already consumed.

## What PQ Vault does NOT do (honest limits)

- **It does not make Machine quantum-resistant.** Consensus, vote
  accounts, the bridge (whose quorum rides the ed25519 precompile),
  ordinary wallets, and every other program keep using Ed25519.
  Only funds inside this vault get hash-based withdrawal
  authorization.
- **It is not a replacement for Ed25519 accounts**, and it is
  useless for hot wallets: one signature per key, multi-transaction
  withdrawals, and off-chain seed custody are cold-storage
  trade-offs, not a wallet UX.
- **The fee payer is still an Ed25519 key.** A withdrawal
  transaction is submitted and paid for like any other. That key
  authorizes nothing — stealing it lets an attacker pay fees, not
  move funds — but the distinction matters and is stated rather
  than smoothed over.
- **The program's own upgrade path is part of the guarantee.** If
  the deployed program were left upgradeable under an ordinary
  Ed25519 key, that key could replace the code and the vault's
  guarantees with it. Deployment therefore follows the repo's
  existing gates (BRIDGE.md §5 pattern): the prototype is
  localnet-only, joins the audit scope before any real deployment
  is contemplated (an AUDIT.md scope note lands with the code in
  step (b), not before), and a production deployment must ship
  with the upgrade authority revoked or held to the same standard
  as the funds.
- **16 keys is a small number.** It is a deliberate v1 cap to keep
  the audited surface small (the LaunchGuard rule). The XMSS
  Merkle-root extension above is the documented path past it.
- **It is not audited and not deployed.** It is a design note.
  Step (b) is a localnet prototype. Nothing in this file is a
  security claim about running code, because there is no running
  code.

## Prototype plan — step (b)

A native Rust program at `programs/crates/pq-vault/`, built in the
repo's existing style (solana-program 2.3, program-test against the
real SPL token program, same conventions as LaunchGuard and the
bridge programs), plus an off-chain signer helper for tests
(WOTS+ key generation and signing from a seed, in the test crate).

Required tests — the program is not done until all of these pass:

1. **Create vault** — vault PDA and token account exist with the
   stored commitments, `keys_total`, and `next_key_index = 0`.
2. **Deposit** — an ordinary SPL transfer in increases the token
   vault balance; no PQ material involved.
3. **Valid withdrawal** — a correct WOTS+ signature for key 0,
   delivered through the buffer flow, moves the exact amount to
   the exact destination and advances `next_key_index` to 1.
4. **Replayed signature rejected** — the same signature and
   parameters presented again fail (index already consumed) and
   move no funds.
5. **Forged signature rejected** — a signature made with the wrong
   seed, and a signature with corrupted chain values, both fail
   verification and move no funds.
6. **Key-reuse withdrawal rejected** — a *second, different*
   valid signature produced by re-signing with key 0 (a different
   amount) fails: the key is consumed, whatever it signs.
7. **Digest binding** — a valid signature for amount X fails for
   amount X+1, and for a different destination.
8. **Wrong index rejected** — a valid signature for key 1 fails
   while `next_key_index` is 0.
9. **Rotation** — a rotation signed by the next key installs a new
   commitment set and resets the index; a rotation signed by the
   owner's Ed25519 key alone (no WOTS+ signature) fails.
10. **Exhaustion** — with all keys spent, further withdrawals
    fail; the vault's remaining balance does not move.
11. **Buffer discipline** — execution with an incomplete buffer
    fails; chunks from a buffer bound to a different digest cannot
    be executed against this withdrawal.
12. **Measured compute units** — the verification CU figure from
    the real SBF build replaces the estimate in this note.

When step (b) lands, it gets the AUDIT.md scope-update note per
the established pattern (new in-scope program; auditors quote
against current HEAD) — not before, because there is no code to
audit yet.

## How it fits the chain

- Design (this note): `programs/PQ-VAULT.md`.
- Prototype (step b): `programs/crates/pq-vault/` — the third
  native program alongside LaunchGuard (`programs/LAUNCH-GUARD.md`)
  and the bridge programs (BRIDGE.md), under the same localnet-only
  discipline and the same deployment gates.
- It composes with LaunchGuard rather than competing with it:
  LaunchGuard constrains *launches*; this vault constrains
  *withdrawal authorization for stored funds*. A project treasury
  could hold its locked or reserve funds in a PQ vault; neither
  program needs to know the other exists.

## References

Standards (the documents the parameters come from):

- NIST FIPS 204, *Module-Lattice-Based Digital Signature Standard*
  (ML-DSA), August 2024. doi:10.6028/NIST.FIPS.204
- NIST FIPS 205, *Stateless Hash-Based Digital Signature Standard*
  (SLH-DSA), August 2024. doi:10.6028/NIST.FIPS.205
- NIST SP 800-208, *Recommendation for Stateful Hash-Based
  Signature Schemes* (XMSS / LMS approval and state requirements).
- RFC 8391, *XMSS: eXtended Merkle Signature Scheme* (WOTS+
  parameters used here: n = 32, w = 16, len = 67).
- RFC 8554, *Leighton-Micali Hash-Based Signatures*.
- NIST IR 8547, *Transition to Post-Quantum Cryptography
  Standards* — final, 27 October 2026 (Ed25519 deprecated after
  2030, disallowed after 2035). doi:10.6028/NIST.IR.8547
- FN-DSA (Falcon), forthcoming FIPS 206 — selected, still in
  draft as of November 2026; Falcon-512 sizes are from the
  pre-standard Falcon specification.

Sources used for the figures and ecosystem claims above:

- PQC Algorithm Reference (sizes verified against the FIPS
  tables) — https://www.postquantumfield.com/blog/pqc-algorithm-reference/
- FIPS 205 (SLH-DSA) field guide (parameter sets, WOTS+/XMSS
  structure) — https://fieldguide.lamarrlabs.com/FIPS-205-(SLH-DSA)
- FIPS 206 (FN-DSA) field guide (draft status, Falcon-512 sizes) —
  https://fieldguide.lamarrlabs.com/FIPS-206-(FN-DSA)
- SLH-DSA overview, PQC Today (XMSS/LMS size ranges, SP 800-208) —
  https://www.pqctoday.com/learn/slh-dsa
- Post-quantum hash-based signatures, in depth (WOTS+ signature =
  67 × 32 = 2,144 bytes) —
  https://www.aicassindra.com/blogs/algorithms/post_quantum_hash_based.html
- NIST IR 8547 final coverage (2030/2035 dates, Ed25519 in scope) —
  https://cipherstance.com/knowledge/post-quantum-cryptography/post-quantum-standards-timeline/
- Solana transaction structure (1,232-byte limit) —
  https://solana.com/docs/core/transactions/transaction-structure
- Agave compute budget source (`sha256_base_cost: 85`,
  `sha256_byte_cost: 1`) —
  https://github.com/anza-xyz/agave/blob/v2.1.13/compute-budget/src/compute_budget.rs
- Ethereum and Solana post-quantum plans, QNT on SOL (Winternitz
  Vault, January 2025; Project Eleven testnet, December 2025 —
  including its caveats about unconfirmed details) —
  https://www.qntonsol.com/learn/ethereum-and-solana-post-quantum-plans.html
- Is Solana quantum safe?, qLABS (Winternitz Vault construction,
  ecosystem status as of October 2026) —
  https://www.qlabs.tech/blog/is-solana-quantum-safe-where-sol-and-its-accounts-stand
- Panoramic research report on quantum readiness (Solana
  Foundation preparation plan, April 2026; Anza and Firedancer
  Falcon choice) —
  https://www.weex.fit/news/detail/panoramic-research-report-is-the-crypto-world-ready-to-face-quantum-attacks-a8lngje4zmlw9fa32kbvrbp4
