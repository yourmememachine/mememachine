#!/usr/bin/env bash
# Generate rugcall/app/test/fixtures/pda-vectors.json from the Rust
# program itself: a temporary integration test prints the derived
# addresses as JSON, this script captures that JSON, and the trap
# removes the temporary test file so the crate is left unchanged.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
TEST_NAME="gen_pda_vectors_tmp"
TEST_FILE="$REPO_ROOT/programs/crates/rug-call/tests/$TEST_NAME.rs"
OUT_FILE="$SCRIPT_DIR/fixtures/pda-vectors.json"
TMP_OUT="$(mktemp)"

cleanup() {
  rm -f "$TEST_FILE" "$TMP_OUT"
}
trap cleanup EXIT

if [ -e "$TEST_FILE" ]; then
  echo "refusing to overwrite existing $TEST_FILE" >&2
  exit 1
fi

mkdir -p "$SCRIPT_DIR/fixtures"

cat > "$TEST_FILE" <<'RUST'
// Temporary vector generator, written and removed by
// rugcall/app/test/gen-pda-vectors.sh. Not part of the suite.
use rug_call::state::*;
use solana_program::pubkey::Pubkey;
use std::str::FromStr;

#[test]
fn print_pda_vectors() {
    let program_id = Pubkey::new_from_array([82; 32]);
    // The resolver corpus mint string named in the build brief
    // ("MintC1ean111...") decodes to 33 bytes, so it cannot be a
    // subject mint. The substitute below is also from the resolver
    // corpus (crew-store fixtures) and decodes to exactly 32 bytes;
    // the JSON records both strings.
    let mint = Pubkey::from_str("CountedMintA11111111111111111111111111111111").unwrap();
    let user = Pubkey::from_str("CreatorWa11et1111111111111111111111111111111").unwrap();
    assert_eq!(mint.to_bytes().len(), 32);
    assert_eq!(user.to_bytes().len(), 32);
    let created_at: i64 = 1799913600;

    let (config, config_bump) = Pubkey::find_program_address(&[SEED_CONFIG], &program_id);
    let (live, live_bump) =
        Pubkey::find_program_address(&[SEED_LIVE_MARKET, mint.as_ref()], &program_id);
    let (market, market_bump) = Pubkey::find_program_address(
        &[SEED_MARKET, mint.as_ref(), &created_at.to_le_bytes()],
        &program_id,
    );
    let created_next = created_at + 1;
    let (market_next, market_next_bump) = Pubkey::find_program_address(
        &[SEED_MARKET, mint.as_ref(), &created_next.to_le_bytes()],
        &program_id,
    );
    let (vault, vault_bump) =
        Pubkey::find_program_address(&[SEED_VAULT, market.as_ref()], &program_id);
    let (position, position_bump) = Pubkey::find_program_address(
        &[SEED_POSITION, market.as_ref(), user.as_ref()],
        &program_id,
    );
    let token_program =
        Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap();
    let ata_program =
        Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap();
    let (ata, ata_bump) = Pubkey::find_program_address(
        &[user.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    );

    let json = format!(
        concat!(
            "{{\"schema\":\"rugcall-pda-vectors/v1\",",
            "\"program_id\":\"{program}\",",
            "\"requested_fixture_mint\":\"MintC1ean11111111111111111111111111111111111\",",
            "\"fixture_mint\":\"{mint}\",",
            "\"fixture_mint_note\":\"the requested corpus mint decodes to 33 bytes, ",
            "so the corpus address CountedMintA11111111111111111111111111111111 ",
            "(exactly 32 bytes) is used as the fixture mint instead\",",
            "\"fixture_user\":\"{user}\",",
            "\"token_program_id\":\"{token}\",",
            "\"ata_program_id\":\"{ata_program}\",",
            "\"vectors\":[",
            "{{\"name\":\"config\",\"inputs\":{{}},\"address\":\"{config}\",\"bump\":{config_bump}}},",
            "{{\"name\":\"live-market\",\"inputs\":{{\"subject_mint\":\"{mint}\"}},\"address\":\"{live}\",\"bump\":{live_bump}}},",
            "{{\"name\":\"market\",\"inputs\":{{\"subject_mint\":\"{mint}\",\"created_at\":{created}}},\"address\":\"{market}\",\"bump\":{market_bump}}},",
            "{{\"name\":\"market-next-second\",\"inputs\":{{\"subject_mint\":\"{mint}\",\"created_at\":{created_next}}},\"address\":\"{market_next}\",\"bump\":{market_next_bump}}},",
            "{{\"name\":\"vault\",\"inputs\":{{\"market\":\"{market}\"}},\"address\":\"{vault}\",\"bump\":{vault_bump}}},",
            "{{\"name\":\"position\",\"inputs\":{{\"market\":\"{market}\",\"user\":\"{user}\"}},\"address\":\"{position}\",\"bump\":{position_bump}}},",
            "{{\"name\":\"ata\",\"inputs\":{{\"user\":\"{user}\",\"mint\":\"{mint}\",\"token_program\":\"{token}\",\"ata_program\":\"{ata_program}\"}},\"address\":\"{ata}\",\"bump\":{ata_bump}}}",
            "]}}"
        ),
        program = program_id,
        mint = mint,
        user = user,
        token = token_program,
        ata_program = ata_program,
        created = created_at,
        created_next = created_next,
        config = config,
        config_bump = config_bump,
        live = live,
        live_bump = live_bump,
        market = market,
        market_bump = market_bump,
        market_next = market_next,
        market_next_bump = market_next_bump,
        vault = vault,
        vault_bump = vault_bump,
        position = position,
        position_bump = position_bump,
        ata = ata,
        ata_bump = ata_bump,
    );
    println!("PDA_VECTORS_JSON:{json}");
}
RUST

export PATH="$HOME/.cargo/bin:$PATH"
cd "$REPO_ROOT/programs"
if ! cargo test --release -p rug-call --test "$TEST_NAME" -- --nocapture > "$TMP_OUT" 2>&1; then
  cat "$TMP_OUT" >&2
  echo "vector generation failed" >&2
  exit 1
fi

python3 - "$TMP_OUT" "$OUT_FILE" <<'PY'
import json
import sys

src, dst = sys.argv[1], sys.argv[2]
payload = None
with open(src, "r", encoding="utf-8") as fh:
    for line in fh:
        if line.startswith("PDA_VECTORS_JSON:"):
            payload = line[len("PDA_VECTORS_JSON:"):].strip()
if payload is None:
    raise SystemExit("PDA_VECTORS_JSON line not found in cargo output")
data = json.loads(payload)
with open(dst, "w", encoding="utf-8") as fh:
    json.dump(data, fh, indent=2)
    fh.write("\n")
print(f"wrote {dst}: {len(data['vectors'])} vectors")
PY
