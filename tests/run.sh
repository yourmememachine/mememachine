#!/bin/bash
# run.sh — run the fork-script test suite.
# Usage: ./tests/run.sh   (from the repo root)
# Never starts a validator, never uses --fresh, never touches
# ledger/, ledger-test/ or keys/. See test_fork_scripts.py header.
set -e
cd "$(dirname "$0")/.."
python3 -m unittest tests.test_fork_scripts tests.test_ops_scripts -v
