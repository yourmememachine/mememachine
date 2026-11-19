# PQ Vault — verification record

Design: `programs/PQ-VAULT.md`. Status: **local only — not
deployed, not audited** (in AUDIT.md Scope A; gates its own
mainnet use, since it custodies stored funds).

## Tests (2026-11-19)

```
$ cd programs && cargo test --release -p pq-vault
Total: 17 passed, 0 failed (4 unit + 13 integration).
```

Program-test against the real SPL token program: actual mint
creation, PDA vault and token vault, chunked signature upload
(700-byte chunks in the harness, so every upload exercises four
WriteSigChunk transactions), token movements, and the full
rotation flow are exercised end to end. The whole suite also
passes against the SBF build (`BPF_OUT_DIR` pointed at the
deploy directory) — 17/17 there too, which is where the
compute-budget requirement below was found.

The twelve tests the design note requires, and where they live:

| Design test | Test |
|---|---|
| 1. Create vault | `create_vault_stores_commitments` (also: 0 and 17 commitments rejected) |
| 2. Deposit is a plain transfer | `deposit_is_a_plain_transfer` |
| 3. Valid withdrawal | `valid_withdrawal_moves_funds_and_advances_index` (buffer closed afterwards) |
| 4. Replayed signature rejected | `replayed_signature_rejected` |
| 5. Forged signature rejected | `forged_signatures_rejected` (wrong seed, and a corrupted chain value; the honest signature still spends the key afterwards) |
| 6. Key-reuse rejected | `key_reuse_rejected` (a second, different valid signature from the consumed key) |
| 7. Digest binding | `digest_binding_amount_and_destination` (amount X+1 fails, a different destination fails, the exact parameters succeed on the same uploaded signature) |
| 8. Wrong index rejected | `wrong_index_rejected` (plus a forged buffer planted with a mismatched header, rejected as an invalid PDA) |
| 9. Rotation | `rotation_installs_new_set_and_resets_index` (a withdrawal under the new set succeeds afterwards) and `rotation_without_wots_signature_fails` (the owner's Ed25519 signature on the transaction buys nothing) |
| 10. Exhaustion freezes | `exhaustion_freezes_vault` (the remaining balance never moves) |
| 11. Buffer discipline | `buffer_discipline` (buffer for a non-next key, out-of-order chunk, incomplete buffer at execution, buffer bound to a different digest) |
| 12. Measured compute units | `compute_units_execute_withdrawal` + the section below |

Unit tests in `src/wots.rs`: `message_digits_known_values`
(zero digest → checksum digits [3, 12, 0]; 0xFF digest →
[0, 0, 0]; nibble order), `sign_verify_round_trip`,
`verify_rejects_tampered_and_foreign_signatures`,
`verify_binds_digest_and_key_index`.

## Measured compute units (2026-11-19)

Method: program-test against the SBF build below, the
measurement transaction requesting a 1,400,000 CU budget,
reading `compute_units_consumed` from the transaction metadata
— the same method as the bridge's SBF figures in
bridge/VERIFY.md. Native program-test figures are not used;
native execution meters almost nothing.

| Instruction | Measured CU (repeated runs) |
|---|---|
| `ExecuteWithdrawal` | 337,261 / 340,266 / 345,541 / 356,036 / 374,111 / 374,116 / 394,451 |
| `RotateKeys` | 380,328 / 393,103 |

The spread is the scheme, not noise: verification work is
Σ(15 − digit) chain steps over the signature's own digits, and
each step runs the hash three times (step key, bitmask, hash),
so a typical signature costs ~1,500 `sol_sha256` calls. The
design note's estimate (50k–120k CU) priced roughly one hash
per step; it is superseded, and the note now carries these
figures.

**Consequence:** verification exceeds the 200,000 CU default
transaction budget. Every test transaction that reaches
verification failed on budget against the SBF build until the
submitting helpers requested headroom — so the budget request
is part of how these instructions are submitted, in the suite
exactly as for a real client (the helpers request 1,000,000).
The extrapolated worst case (every digit zero, 1,005 steps) is
roughly 700k, inside the 1,400,000 transaction maximum, so no
valid signature is unexecutable.

One optimization was measured and rejected: assembling the
hash input into a single buffer and calling the single-slice
hash cost 402,454 CU against a 340,266 CU run of the
three-slice form under the same conditions — the syscall
iterates slices without copying; the buffer pays for its
copies. The three-slice form stays.

## SBF build (2026-11-19)

Same method as LaunchGuard (scratch tree, program-only
lockfile, see `bridge/deploy/SBF-BUILD-NOTES.md`):

| Artifact | Size (bytes) | sha256 |
|---|---|---|
| `pq_vault.so` | 148,944 | `b6a52b936b9af8c9fb7b60dcf1664a1bfde2bb18e62333ae00ad0048ea9c74d7` |

ELF 64-bit sBPF, stripped. Deterministic: two builds, identical
hash. Lockfile saved as `programs/sbf-Cargo.lock`, which now
covers launch-guard and pq-vault; the only change from the
previous saved lockfile is the added `pq-vault` package entry —
every dependency pin is untouched, so LaunchGuard's recorded
build is unaffected. Not deployed.
