#!/usr/bin/env bash
# Run the RugCall app data-layer test suite (Node built-in test
# runner, TAP output) and print the TAP summary lines so the test
# count is visible in make output.
set -e
set -o pipefail

cd "$(dirname "$0")"

TAP_FILE="$(mktemp)"
trap 'rm -f "$TAP_FILE"' EXIT

node --test --test-reporter=tap test/*.test.js | tee "$TAP_FILE"

grep -E '^# (tests|pass|fail)' "$TAP_FILE"
