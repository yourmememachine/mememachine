#!/usr/bin/env bash
# Generate rugcall/app/test/fixtures/pda-vectors-deployed.json from
# the Rust program itself, for the DEPLOYED program id: a temporary
# integration test derives every fixture-bundle PDA (config, each
# market's market / live-market / vault, each position) with the
# program's own seed constructions, this script captures the JSON,
# and the trap removes the temporary test file so the crate is left
# unchanged. Sibling of gen-pda-vectors.sh (which covers the
# synthetic program id in pda-vectors.json).
#
# Derivation inputs come from the fixture bundle itself (subject
# mint + created_at per market; market index + user per position),
# but every address is derived in Rust from those inputs — vault
# and position derivations start from the Rust-derived market
# pubkey, never from a bundle-stored address — so a wrong stored
# address in the bundle cannot leak into the vectors; it fails the
# comparison tests instead.
#
# Usage: gen-pda-vectors-deployed.sh [--check]
#   --check regenerates into a temp file and diffs against the
#   committed vectors instead of overwriting them.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
TEST_NAME="gen_pda_vectors_deployed_tmp"
TEST_FILE="$REPO_ROOT/programs/crates/rug-call/tests/$TEST_NAME.rs"
OUT_FILE="$SCRIPT_DIR/fixtures/pda-vectors-deployed.json"
TMP_OUT="$(mktemp)"
CHECK_OUT=""

# With --check, the vectors are generated into a temp file and
# diffed against the committed file instead of overwriting it:
# regeneration must be byte-identical.
if [ "${1:-}" = "--check" ]; then
  CHECK_OUT="$(mktemp)"
fi

if [ -e "$TEST_FILE" ]; then
  echo "refusing to overwrite existing $TEST_FILE" >&2
  exit 1
fi

cleanup() {
  rm -f "$TEST_FILE" "$TMP_OUT" $CHECK_OUT
}
trap cleanup EXIT

mkdir -p "$SCRIPT_DIR/fixtures"

cat > "$TEST_FILE" <<'RUST'
// Temporary vector generator, written and removed by
// rugcall/app/test/gen-pda-vectors-deployed.sh. Not part of the suite.
use rug_call::state::*;
use solana_program::pubkey::Pubkey;
use std::path::PathBuf;
use std::str::FromStr;

const DEPLOYED_PROGRAM_ID: &str = "DcwvEDi4yCk16wQ2HVzc3dQiBFccj8VxzboGUYdUXseZ";

fn bundle_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../rugcall/app/test/fixtures/app-markets.json")
}

fn get<'a>(v: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    v.get(key).unwrap_or_else(|| panic!("missing key {key}"))
}

fn get_str(v: &serde_json::Value, key: &str) -> String {
    get(v, key).as_str().unwrap_or_else(|| panic!("key {key} is not a string")).to_string()
}

#[test]
fn print_pda_vectors_deployed() {
    let raw = std::fs::read_to_string(bundle_path()).expect("read fixture bundle");
    let bundle: serde_json::Value = serde_json::from_str(&raw).expect("parse fixture bundle");
    let program_id = Pubkey::from_str(DEPLOYED_PROGRAM_ID).unwrap();
    assert_eq!(
        get_str(&bundle, "program_id"),
        DEPLOYED_PROGRAM_ID,
        "the fixture bundle must carry the deployed program id before vectors are generated for it"
    );

    let (config, config_bump) = Pubkey::find_program_address(&[SEED_CONFIG], &program_id);

    let markets_in = get(&bundle, "markets").as_array().expect("markets array");
    let mut derived_markets: Vec<Pubkey> = Vec::new();
    let mut market_vectors = Vec::new();
    for m in markets_in {
        let mint = Pubkey::from_str(&get_str(m, "subject_mint")).unwrap();
        let created_at = get(m, "created_at").as_i64().expect("created_at i64");
        let (market, market_bump) = Pubkey::find_program_address(
            &[SEED_MARKET, mint.as_ref(), &created_at.to_le_bytes()],
            &program_id,
        );
        let (live, live_bump) =
            Pubkey::find_program_address(&[SEED_LIVE_MARKET, mint.as_ref()], &program_id);
        let (vault, vault_bump) =
            Pubkey::find_program_address(&[SEED_VAULT, market.as_ref()], &program_id);
        derived_markets.push(market);
        market_vectors.push(serde_json::json!({
            "subject_mint": mint.to_string(),
            "created_at": created_at,
            "market": { "address": market.to_string(), "bump": market_bump },
            "live_market": { "address": live.to_string(), "bump": live_bump },
            "vault": { "address": vault.to_string(), "bump": vault_bump },
        }));
    }

    let positions_in = get(&bundle, "positions").as_array().expect("positions array");
    let mut position_vectors = Vec::new();
    for p in positions_in {
        let stored_market = get_str(p, "market");
        let idx = markets_in
            .iter()
            .position(|m| get_str(m, "address") == stored_market)
            .unwrap_or_else(|| panic!("position references unknown market {stored_market}"));
        let market = derived_markets[idx];
        let user = Pubkey::from_str(&get_str(p, "user")).unwrap();
        let (position, position_bump) = Pubkey::find_program_address(
            &[SEED_POSITION, market.as_ref(), user.as_ref()],
            &program_id,
        );
        position_vectors.push(serde_json::json!({
            "market": market.to_string(),
            "user": user.to_string(),
            "address": position.to_string(),
            "bump": position_bump,
        }));
    }

    let out = serde_json::json!({
        "schema": "rugcall-pda-vectors-deployed/v1",
        "program_id": DEPLOYED_PROGRAM_ID,
        "generated_by": "rugcall/app/test/gen-pda-vectors-deployed.sh",
        "generator": "rust: rug_call::state SEED_* + Pubkey::find_program_address (processor.rs seed constructions)",
        "config": { "address": config.to_string(), "bump": config_bump },
        "markets": market_vectors,
        "positions": position_vectors,
    });
    println!("PDA_VECTORS_DEPLOYED_JSON:{out}");
}
RUST

export PATH="$HOME/.cargo/bin:$PATH"
cd "$REPO_ROOT/programs"
if ! cargo test --release -p rug-call --test "$TEST_NAME" -- --nocapture > "$TMP_OUT" 2>&1; then
  cat "$TMP_OUT" >&2
  echo "deployed vector generation failed" >&2
  exit 1
fi

python3 - "$TMP_OUT" "${CHECK_OUT:-$OUT_FILE}" <<'PY'
import json
import sys

src, dst = sys.argv[1], sys.argv[2]
payload = None
with open(src, "r", encoding="utf-8") as fh:
    for line in fh:
        if line.startswith("PDA_VECTORS_DEPLOYED_JSON:"):
            payload = line[len("PDA_VECTORS_DEPLOYED_JSON:"):].strip()
if payload is None:
    raise SystemExit("PDA_VECTORS_DEPLOYED_JSON line not found in cargo output")
data = json.loads(payload)
with open(dst, "w", encoding="utf-8") as fh:
    json.dump(data, fh, indent=2)
    fh.write("\n")
print(f"wrote {dst}: {len(data['markets'])} markets, {len(data['positions'])} positions")
PY

if [ -n "$CHECK_OUT" ]; then
  if diff -u "$OUT_FILE" "$CHECK_OUT" > /dev/null; then
    echo "check ok: $OUT_FILE regenerates byte-identically"
  else
    echo "check FAILED: regenerated vectors differ from $OUT_FILE" >&2
    diff -u "$OUT_FILE" "$CHECK_OUT" >&2 || true
    exit 1
  fi
fi
