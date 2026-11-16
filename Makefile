# Machine — one-command local checks.
#
# `make verify` runs everything that can be proven locally:
# the bridge workspace tests, the programs workspace tests
# (LaunchGuard), and the fork suite. SBF builds, fuzzing, and the
# testnet/livenet runbooks live in their own docs (bridge/deploy/,
# bridge/fuzz/, testnet/, livenet/) — see STATUS.md for what is and
# is not proven.

.PHONY: verify test-bridge test-programs fork-suite clippy

verify: test-bridge test-programs fork-suite
	@echo "All local checks passed (bridge + programs + fork suite)."

test-bridge:
	cd bridge && cargo test --release

test-programs:
	cd programs && cargo test --release

fork-suite:
	./tests/run.sh

clippy:
	cd bridge && cargo clippy --release --all-targets
	cd programs && cargo clippy --release --all-targets
