# Machine — one-command local checks.
#
# `make verify` runs everything that can be proven locally:
# the bridge workspace tests, the programs workspace tests
# (LaunchGuard), and the fork suite. SBF builds, fuzzing, and the
# testnet/livenet runbooks live in their own docs (bridge/deploy/,
# bridge/fuzz/, testnet/, livenet/) — see STATUS.md for what is and
# is not proven.

.PHONY: verify test-bridge test-programs fork-suite test-resolver test-predict clippy

verify: test-bridge test-programs fork-suite test-resolver test-predict
	@echo "All local checks passed (bridge + programs + fork suite + resolver JS suite)."

test-bridge:
	cd bridge && cargo test --release

test-programs:
	cd programs && cargo test --release

fork-suite:
	./tests/run.sh

test-resolver:
	./rugcall/resolver/run-tests.sh

test-predict:
	./rugcall/app/run-tests.sh

clippy:
	cd bridge && cargo clippy --release --all-targets
	cd programs && cargo clippy --release --all-targets
