# Security Changelog

Every security-relevant change to the bridge, LaunchGuard, and the
chain configuration, newest first. Maintained by hand: when a
commit changes verification, caps, authorities, replay protection,
or the audit surface, it gets a line here in the same commit or
the next one. Routine docs and test-count syncs are omitted unless
they record a security finding.

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
