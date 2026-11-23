# Fuzzing bridge-common

cargo-fuzz (libFuzzer) targets for the parsers an attacker can
influence: the ed25519 precompile / instructions-sysvar parser
(AUDIT.md Scope A's priority target), BridgeMessage decoding,
the DailyUsage cap arithmetic, and the CrossingReceipt
serialization / receipt PDA derivation.

```
rustup toolchain install nightly --profile minimal --component rust-src
cargo install cargo-fuzz

cd bridge/fuzz
cargo +nightly fuzz run precompile_parser -- -max_total_time=300
cargo +nightly fuzz run message_decode -- -max_total_time=300
cargo +nightly fuzz run daily_usage -- -max_total_time=300
cargo +nightly fuzz run crossing_receipt -- -max_total_time=300
```

A scheduled GitHub Actions workflow (`.github/workflows/fuzz.yml`,
Mondays 06:00 UTC, also manually dispatchable) runs the same four
bounded sessions weekly, so fuzzing is a standing control rather
than a one-off event; a crash fails the run and uploads the
reproducing artifact.

The crate is its own workspace (a cargo-fuzz requirement) and is
not part of the bridge workspace build. Corpora and crash
artifacts are generated locally and not committed — a crash
becomes a unit test in bridge-common instead (see
`malformed_sysvar_is_missing_attestation_never_a_panic`).

History: the first session (2026-11-14) found a panic on empty
sysvar data inside an upstream helper the verifier delegated to;
the verifier now parses the sysvar itself with bounds checks.
Full record in `bridge/VERIFY.md`.
