# MemeMachine — Current Limitations

The one-page version, so nobody — reviewer, grantor, or user — has
to guess what is not yet true. The full evidence trail is
STATUS.md; the audit picture is AUDIT.md. Nothing on this page is
hidden anywhere else in the repo either.

- **Not audited.** No independent security audit has been
  performed. Quotes are being gathered (Accretion, Sec3, Oak
  Security) and grant applications are in (Areta Audit Subsidy,
  Solana Foundation). Until an audit completes, **no real asset may
  cross the bridge** — that gate is in BRIDGE.md §5 and binds every
  launch phase.
- **Not deployed.** The bridge programs and LaunchGuard exist as
  tested code and verified SBF builds only. Nothing has been
  bridged, ever.
- **Localnet only, one validator.** The chain has run as a
  single-node local testnet that votes and finalizes. Multi-
  validator consensus, signed user transfers, the faucet, and
  public RPC are **not yet proven** — they need a host where UDP
  send works and the file-descriptor limit can be raised to
  1,000,000; the development VM has neither (LAUNCH_CHECKLIST.md
  items 17–18). A public testnet is fully prepped
  (testnet/TESTNET.md) and waits on hosting.
- **Compute units are program-test numbers.** The CU figures in
  the docs (409–676 depending on the instruction) were measured
  in native program-test execution, not on-chain SBF execution.
  Real SBF CU gets measured on the Phase 1 testnet host.
- **Relayer transport is file-based.** Phase-1 drills exchange
  attestations as files. Authenticated relayer-to-relayer
  transport is a written production requirement (BRIDGE.md §3.3),
  not yet built — it is required before real value, not for drills.
- **The relayer set is a trust assumption.** 3-of-5 committee
  attestation is weaker than validator-set attestation. See
  THREAT-MODEL.md for exactly what that does and does not risk.
- **Bug bounty is scoped but unfunded.** SECURITY.md defines the
  scope; there is no money behind it yet, and the repo says so.
- **Patched validator source is unbuilt.** The chain runs stock
  prebuilt Agave 2.3.13 binaries. The three source patches in
  `patches/` (client id, banner, snapshot interval) are documented
  but their build has not been run end-to-end.
- **Numbers in this repo are testnet numbers.** Test $MACHINE on
  any future public testnet is a different genesis and is never
  convertible to anything. It has no value by design.
