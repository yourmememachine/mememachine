# Security Changelog

Every security-relevant change to the bridge, LaunchGuard, PQ
Vault, and the chain configuration, newest first. Maintained by hand: when a
commit changes verification, caps, authorities, replay protection,
or the audit surface, it gets a line here in the same commit or
the next one. Routine docs and test-count syncs are omitted unless
they record a security finding.

- **2026-11-19 — `5b37d7b`** LaunchGuard dust floor
  (`programs/crates/launch-guard/`; spec
  `programs/LAUNCH-GUARD.md`). LaunchGuard's audit surface
  grows: a fifth instruction, `Initialize`, creates a config
  PDA carrying a minimum lock size in raw units, enforced by
  `RegisterAndLock` on top of the 10% share. Initialize is
  one-time and bound to the program's upgrade authority —
  proven against the ProgramData account by a check mirrored
  from bridge-common's `verify_upgrade_authority` (self-audit
  F-1 pattern); the mirror is now pinned by unit tests for
  every structural rejection (wrong address, wrong owner,
  truncated data, wrong variant, immutable program).
  Registration before Initialize fails closed
  (`ConfigNotInitialized`); no instruction changes the floor
  afterwards. **Breaking instruction-ABI change:**
  `RegisterAndLock` takes the config PDA as a tenth account
  (appended last); the pre-floor 9-account form fails with
  `NotEnoughAccountKeys`. The program is not deployed and
  has no live clients. Six-seat review before shipping;
  LaunchGuard suite 35 → 62; SBF rebuilt: launch_guard.so
  `8153dd4f…` (139,352 B, deterministic). Repo total
  283 → 310. Localnet only; not deployed.
- **2026-11-19 — `8ca345c`** PQ Vault prototype program added
  (`programs/crates/pq-vault/`; design `programs/PQ-VAULT.md`,
  record `programs/PQ-VERIFY.md`). A new custody program joins
  the audit surface: a cold-storage vault whose withdrawals
  are authorized only by RFC 8391 WOTS+ one-time signatures
  (SHA-256, w=16, len=67) — no Ed25519 key anywhere in the
  withdrawal or rotation path can move funds. One-time
  discipline is enforced by the program, not by user care: up
  to 16 commitments per vault, strict in-order spend via
  `next_key_index`, the index increments before the token
  transfer runs, rotation replaces the whole set only on a
  one-time signature from the next unconsumed key, and
  exhaustion without a chained rotation freezes the remainder
  fail-closed. The 2,144-byte signature is delivered in chunks
  through a buffer account bound to the exact withdrawal
  digest (domain-separated: vault, key index, destination,
  amount, mint), so a signature authorizes exactly one
  withdrawal of exactly one amount to exactly one destination.
  17 tests (4 unit + 13 integration), all twelve of the design
  note's required tests, green natively and against the SBF
  build. Measured verification cost: ExecuteWithdrawal
  337k–395k CU, RotateKeys 380k–393k CU (SBF) — verifying
  transactions must request a raised compute budget; the
  design estimate (50k–120k) did not survive measurement and
  is corrected in the design note. SBF built: pq_vault.so
  `b6a52b93…` (148,944 B, deterministic). Programs workspace
  46 → 63; repo total 266 → 283. Localnet only; not deployed.
- **2026-11-18 — `236257b`** Code-review debate round 4
  (bridge-common + proof-bundle; two independent reviews,
  every finding debated to a conclusion, every fix reproduced
  failing first). **MintWrapped source-mint binding (High):**
  `MachineConfig` had no source-mint field, so a quorum
  attestation for a deposit of a different asset sharing the
  relayer set minted this deployment's wrapped tokens unbacked
  (the Unlock side was bound in round 1; this was its leftover
  B2). `MachineConfig` gains `source_mint` (LEN 334 → 366),
  `Initialize` registers it, `MintWrapped` rejects a mismatched
  `message.source_mint`, and the relayer cross-checks the
  on-chain value at startup. **Bounded attestation lifetime:**
  `validate_message` now rejects `expiry_slot - source_slot >
  1,000,000` and expiry before the source slot (leftover B8) —
  a quorum-signed message can no longer be a permanent bearer
  instrument. **Zero-authority guards:** the shared verifiers
  reject quorum 0 / above the set size and admin threshold 0 /
  above the set size (previously an empty signature list
  verified). **ed25519 builder:** refuses more than
  `MAX_RELAYERS` signatures instead of silently truncating the
  count/offset fields. **Proof-bundle verifier:** a non-ASCII
  message hash is rejected instead of panicking; zero-amount
  bundles are rejected; export stamps the protocol quorum (a
  stronger 5-of-5 collection no longer fails its own
  verification); the report carries the relayer set and
  verification can pin it (`--expected-relayers`); CLI input
  files are capped at 1 MiB; the receipt and relayer-set
  claims are downgraded to what offline verification
  establishes. 9 new regression tests (bridge workspace
  188 → 197). SBF rebuilt: solana_lock.so `a22bccd8…`
  (211,744 B), machine_bridge.so `7d0ab41f…` (215,312 B).
- **2026-11-18 — `10840ba`** Code-review debate round 3
  (LaunchGuard; two independent reviews, every finding debated
  to a conclusion before any fix). One code fix: `ExtendLock`
  capped extensions against `clock.slot + MAX_LOCK_SLOTS`, a
  sliding window — a lock registered at the 5-year maximum
  could be extended again after any time passed, and repeated
  extensions made the total duration unbounded, contradicting
  the documented maximum lock period (reproduced before the
  fix). The cap is now `registered_slot + MAX_LOCK_SLOTS` with
  checked arithmetic. 1 new adversarial regression test
  (LaunchGuard 34 → 35). Three agreed findings were closed as
  documented limits in `programs/LAUNCH-GUARD.md` because no
  on-chain check can decide them: registration is
  permissionless and first-come among ≥10% holders, so the
  record's creator is the registrant, not a verified issuer;
  a holder account frozen before freeze-authority revocation
  stays frozen forever and is invisible to the program; the
  reputation counters are unweighted, so a dust-supply
  completion counts the same as a large launch. Killed in
  debate: the unchecked `clock.slot` additions are unreachable
  at any real slot height. SBF rebuilt: launch_guard.so
  `9aceb092…` (135,416 B).
- **2026-11-18 — `e0d0a24`** Receipt indexer (new off-chain
  tooling, no program change): a crossing receipt PDA can now be
  resolved to the single transaction that created it, with the
  receipt parsed strictly and the transaction cross-pinned to
  the slot the receipt itself records. Failed replay attempts
  against a nonce are skipped during resolution; extra
  successful signatures for a receipt PDA are flagged as an
  anomaly in the emitted record. This is the audit trail the
  Ankr-style recycled-receipt attack (Nov 2026) would have to
  survive. 15 new tests; bridge workspace 173 → 188.
- **2026-11-17 — `9cf00cf`** Code-review debate round 2
  (relayer; two independent reviews, every finding debated to a
  conclusion before any fix). Fixes: quorum collection counts
  only individually verified signatures (junk exchange files are
  skipped, verified disagreement still fails closed, attestation
  files cannot be silently overwritten with a different
  message); the source transaction is bound to the event
  record's slot and the source-transfer check requires the SPL
  Token program and a successful transaction; the audit log is
  verified before publishing/signing and before sending, its
  default path leaves the shared directory, and persistence
  writes are fsynced; the `run` loop validates its destination
  account at startup, scans all signed-but-unprocessed nonces,
  skips receipt-proven nonces, and verifies the destination
  token account's mint and owner; collection uses the on-chain
  quorum and refuses paused/killed destinations; config, state
  and keypair-file validation hardened; the watcher refuses an
  enrich result that differs from the finality-checked record.
  6 new regression tests (relayer 54 → 60). Killed in debate:
  the missing relayer-side cap check is intentional layering
  (caps are destination state, enforced on-chain). Recorded,
  not patched: `Submitted` is still logged on RPC acceptance
  without a confirmation poll, and there is no single-instance
  lock against two relayer processes sharing one config.
- **2026-11-17 — `0efccbc`** Code-review debate round 1
  (machine-bridge + solana-lock; two independent reviews, every
  finding debated to a conclusion before any fix). Six fixes:
  (1) `Unlock` never compared the attested message's
  `source_mint` to the config's — a quorum attestation for a
  different asset sharing the relayer set released this vault's
  tokens; now bound. (2) Machine `Initialize` discarded the
  parsed mint: a personal mint authority (unbacked minting
  outside caps/quorum, surviving program immutability) or freeze
  authority passed; now the mint authority must be the
  bridge-authority PDA and any freeze authority that PDA or
  none. (3) Lock `Initialize` trusted `TokenAccount::unpack`
  bytes with no token-program owner or account-state check; both
  now required. (4) `Unlock` accepted the vault itself as
  destination (a no-op transfer that still debited
  `total_locked`) and `MintWrapped` accepted the authority PDA
  as recipient (unburnable tokens, stranded backing); both
  rejected. (5) `Burn` did not pin its mint account; pinned.
  (6) The config PDA was re-derived in only the two attested
  paths; every handler in both programs now re-derives it.
  8 new regression tests; bridge SBF binaries rebuilt
  (`e0586b19…` / `56d23895…`). Recorded, not patched (design
  decisions): over-cap burns/deposits can strand a crossing,
  MintWrapped attestations cannot yet be bound to a source mint
  (no such field in the Machine config), attestation expiry has
  no program-side maximum, and direct transfers into the vault
  remain unrecoverable.
- **2026-11-17 — `8e84d18`** LaunchGuard CloseVault (external
  review triage). The review's stuck-funds point was real: the
  vault is a normal SPL token account, anyone can transfer extra
  tokens into it, and `Release` moves exactly `locked_amount`,
  so the excess was permanently stuck. New instruction
  `CloseVault` — creator only, gated on the record's Released
  state (set only after the full locked amount has left the
  vault) — sweeps the residual to the creator and closes the
  vault account, returning its rent. Locked or
  expired-but-unreleased launches always fail it (new error
  `NotReleased`); it has no path to any part of the lock, and it
  carries the same record-PDA re-derivation check as
  ExtendLock/Release. The review's second code claim — a missing
  record-PDA re-derivation check in ExtendLock and Release —
  was verified **not present in the code**: the assertion has
  been in both instructions since `93b5cd9` (2026-11-16), the
  reputation PDA is bound the same way in RegisterAndLock and
  Release, and dedicated wrong-address regression tests cover
  all of it (a forged Released record at a wrong address is now
  also pinned against CloseVault). The remaining four points
  are the standard's documented scope (10% floor vs the unlocked
  90%, no LP locking, per-keypair reputation, slot-based locks /
  unaudited) — LAUNCH-GUARD.md wording tightened, no economics
  changed. 5 new tests; launch_guard.so rebuilt
  (`915dc769…`, 135,488 B).
- **2026-11-16 — `7cd6f5b`** Self-audit fixes (REVIEW-1: the
  audit method turned on Machine's own code; 4 findings, each
  PoC-proven in a scratch copy first). (F-1, High) `Initialize`
  in both bridge programs was permissionless — the first caller
  after deployment set relayers/admins/caps, and machine-bridge
  accepted `wrapped_mint` from instruction data unvalidated.
  Initialize now requires the program's ProgramData account and
  the payer must equal the recorded upgrade authority
  (`bridge_common::verify_upgrade_authority`); machine-bridge
  validates the wrapped mint as a real mint account. (F-2,
  Medium) PDA creation in all three programs is create-or-adopt:
  a pre-funded empty system-owned PDA shell is topped up,
  allocated, and assigned instead of permanently blocking the
  crossing / registration / deployment it targets. (F-3, Low)
  one shared `bridge_common::validate_member_set` (default keys
  and duplicates rejected) now serves `Initialize` and
  `SetRelayers` in both bridge programs. (F-4, Low) LaunchGuard
  minimum lock uses ceiling division — supply 19 requires a
  2-token lock (the floor computation admitted 1 token = 5.26%).
  Same commit, defense in depth: token program pinned to
  `spl_token::id()` in Deposit / Unlock / MintWrapped / Burn,
  and the relayer's source-transfer verification is bound to the
  record's creating instruction (one transfer can no longer
  satisfy two same-amount records in one batched transaction).
  21 new tests; all three SBF binaries rebuilt.
- **2026-11-16 — `e14c539`** Relayer watcher trust audit (Coreum
  lesson, Aug 2026): the watcher's sender extraction only scanned
  top-level instructions, but the deposit's token transfer is a CPI
  of the Deposit instruction — on a real deposit transaction it
  found no transfer and refused to sign, and a batched transaction
  with an unrelated top-level transfer would have yielded the wrong
  sender. The watcher now verifies the source transfer behind each
  event (into the vault for a deposit, burning the wrapped mint
  for a burn, exactly the record's amount) before signing, and
  fails closed otherwise. Off-chain only; no program change.
- **2026-11-15 — `8369f99`** Three features, each changing the
  audit surface and recorded here per this file's rule: (1)
  **crossing receipts** — MintWrapped and Unlock now create a
  receipt PDA (message hash, source tx, amount, recipient, slot,
  timestamp) in the same transaction as the value move; the
  destination instruction account lists grew by one account on
  both programs and in the relayer's builders. (2) **Automatic
  cap tightening** — the lock program's Unpause now sets
  `tightened_until_slot` (new LockConfig field) one window out;
  unlocks inside the window use quarter-strength dynamic caps.
  (3) **LaunchGuard reputation PDA** — per-creator
  registered/completed counters, incremented only by
  RegisterAndLock and Release. Same round, test-only: sysvar
  parser adversarial tests, hash-binding coverage for every
  message field, cap-overflow boundaries, relayer-rotation
  end-to-end test, 45-crossing volume drill.
- **2026-11-15 — `b9d98d3`** Precompile parser adversarial tests:
  extra instructions, malformed offsets, cross-instruction
  references, duplicate signers, wrong-hash entries — the parser
  attack surface is now pinned by deterministic tests, not only
  fuzzing. AUDIT.md names the parser as the highest-risk component
  in plain language.
- **2026-11-15 — `4a83a7a`** Nonce-gate hardening: the four value
  paths' inline nonce-PDA checks moved into one shared, unit-tested
  helper (`check_nonce_unprocessed`), removing a class of
  copy-drift risk in replay protection.
- **2026-11-15 — `701c012`** Fuzzing found a real bug: upstream
  instructions-sysvar helpers sliced without length checks and
  panicked on malformed sysvar data. The sysvar is now parsed
  in-crate with every read bounds-checked; malformed input returns
  MissingAttestation, never a panic. cargo-fuzz targets, Makefile,
  and CI added in the same commit.
- **2026-11-15 — `f3e1f62`** Localnet checklist round: wrong-account
  and boundary adversarial tests (dynamic-cap exactly-1% passes /
  +1 fails, LaunchGuard freeze-authority and 9.99%-lock cases), and
  the full local end-to-end drill (deposit→mint, burn→unlock,
  pause mid-flow, relayer disagreement) added and passing.
- **2026-11-15 — `f4012ca`** Wrong-destination-account tests for
  the mint path (wrong owner → Unauthorized with the nonce left
  unconsumed and retryable; wrong mint → InvalidAccountData).
- **2026-11-14 — `e388aca`** Attestation verification switched from
  in-program ed25519-dalek to the ed25519 precompile + instructions
  sysvar (signatures no longer travel in program payloads). Dynamic
  percentage caps added on the Solana unlock path (1% per tx / 5%
  per window of the live locked total) on top of the absolute caps.
  LaunchGuard rejects Token-2022 mints. Adversarial tests: expiry
  boundary, reordered and duplicate signature sets, nonce gap.
- **2026-11-14 — `210df59`** LaunchGuard shipped: registration
  requires revoked mint + freeze authorities and a ≥10% / ≥30-day
  slot-based lock in a program vault; locks extend-only. Free
  security pass recorded in AUDIT.md (cargo audit dispositions,
  clippy).
- **2026-11-14 — `e04a8b7`** Bridge relayer built: finalized-only
  signing (+16 slots), deterministic message construction, 3-of-5
  quorum collection with disagreement blocking, dry-run unless
  explicitly enabled.
