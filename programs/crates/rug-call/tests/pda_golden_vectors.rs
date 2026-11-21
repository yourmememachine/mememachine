//! Cross-language PDA golden vectors for the deployed program.
//!
//! The judge page's fixture bundle
//! (`rugcall/app/test/fixtures/app-markets.json`) stores derived
//! PDA addresses for its markets and positions. Those stored
//! addresses are only trustworthy if something other than the
//! JavaScript implementation that consumes them has derived them.
//! `rugcall/app/test/fixtures/pda-vectors-deployed.json` is that
//! independent derivation: it is generated from this crate's own
//! seed constructions by `rugcall/app/test/gen-pda-vectors-deployed.sh`.
//!
//! These tests pin the triangle shut from the Rust side:
//!   program seeds -> golden file (this file's first test),
//!   golden file -> fixture bundle stored addresses (second test),
//!   golden config -> the known on-chain config account (third).
//!
//! Exactly what the first test's tripwire covers, stated narrowly:
//! the `SEED_*` constants are imported from `state.rs`, so a
//! constant change fails it directly; a change to the seed
//! compositions as restated below (or to the golden file, or to
//! the bundle inputs they are derived from) fails it until the
//! vectors are regenerated. A composition change made ONLY inside
//! `processor.rs`'s private helpers is not caught here — it is
//! caught end-to-end by the integration suite in `rug_call.rs`,
//! which pre-derives accounts with the same restatement and
//! submits them to the real processor, which recomputes with its
//! own helpers and rejects mismatches. That suite runs under a
//! synthetic program id, which is sound because PDA derivation is
//! program-id-parametric; the deployed-id instantiation is
//! anchored to reality by the third test for the config seed, and
//! for the remaining seeds rests on that parametricity.
//!
//! Scope of the proof: given the bundle's derivation inputs
//! (subject mint, created_at, user), the stored addresses equal
//! the program's derivation of them. The inputs themselves are
//! fixture authorship, and a position's assignment to a market is
//! checked by the semantic suites, not by these tests.
//!
//! The seed constructions below restate `processor.rs`'s private
//! `*_pda` helpers line for line (the same restatement the
//! integration suite in `rug_call.rs` already relies on); the
//! helpers themselves are exercised end-to-end there.
//!
//! Run with:
//!     cd programs && cargo test --release --test pda_golden_vectors

use rug_call::state::{SEED_CONFIG, SEED_LIVE_MARKET, SEED_MARKET, SEED_POSITION, SEED_VAULT};
use solana_program::pubkey::Pubkey;
use std::path::PathBuf;
use std::str::FromStr;

const DEPLOYED_PROGRAM_ID: &str = "DcwvEDi4yCk16wQ2HVzc3dQiBFccj8VxzboGUYdUXseZ";
const KNOWN_ONCHAIN_CONFIG: &str = "D8upBvHaUh7pX8PT3AZWFFWa78PkDJAJ5xPoumBSwANp";
const REGENERATE: &str =
    "re-run rugcall/app/test/gen-pda-vectors-deployed.sh to regenerate the golden vectors";

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../rugcall/app/test/fixtures")
        .join(name)
}

fn load_json(name: &str) -> serde_json::Value {
    let path = fixture_path(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn field<'a>(v: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    v.get(key)
        .unwrap_or_else(|| panic!("missing key {key} in {v}"))
}

fn field_str<'a>(v: &'a serde_json::Value, key: &str) -> &'a str {
    field(v, key)
        .as_str()
        .unwrap_or_else(|| panic!("key {key} is not a string in {v}"))
}

fn field_i64(v: &serde_json::Value, key: &str) -> i64 {
    field(v, key)
        .as_i64()
        .unwrap_or_else(|| panic!("key {key} is not an integer in {v}"))
}

fn field_u64(v: &serde_json::Value, key: &str) -> u64 {
    field(v, key)
        .as_u64()
        .unwrap_or_else(|| panic!("key {key} is not an unsigned integer in {v}"))
}

fn pubkey(s: &str) -> Pubkey {
    Pubkey::from_str(s).unwrap_or_else(|e| panic!("bad pubkey {s}: {e}"))
}

fn assert_vector(
    what: &str,
    derived: (Pubkey, u8),
    stored: &serde_json::Value,
    stored_address_key: &str,
    stored_bump_key: &str,
) {
    assert_eq!(
        derived.0.to_string(),
        field_str(stored, stored_address_key),
        "{what} address mismatch — {REGENERATE}"
    );
    assert_eq!(
        u64::from(derived.1),
        field_u64(stored, stored_bump_key),
        "{what} bump mismatch — {REGENERATE}"
    );
}

#[test]
fn deployed_golden_vectors_rederive_from_program_seeds() {
    let golden = load_json("pda-vectors-deployed.json");
    assert_eq!(field_str(&golden, "schema"), "rugcall-pda-vectors-deployed/v1");
    let program_id = pubkey(field_str(&golden, "program_id"));
    assert_eq!(
        program_id,
        pubkey(DEPLOYED_PROGRAM_ID),
        "golden vectors must be keyed to the pinned deployed program id"
    );

    let config = Pubkey::find_program_address(&[SEED_CONFIG], &program_id);
    assert_vector("config", config, field(&golden, "config"), "address", "bump");

    let golden_markets = field(&golden, "markets")
        .as_array()
        .expect("golden markets is an array");
    assert_eq!(golden_markets.len(), 8, "golden market count — {REGENERATE}");
    // Derived market pubkeys, reused so position derivations start
    // from the re-derived market, never the file's stored address.
    let mut derived_markets: Vec<Pubkey> = Vec::new();
    for gm in golden_markets {
        let mint = pubkey(field_str(gm, "subject_mint"));
        let created_at = field_i64(gm, "created_at");
        let market = Pubkey::find_program_address(
            &[SEED_MARKET, mint.as_ref(), &created_at.to_le_bytes()],
            &program_id,
        );
        assert_vector("market", market, field(gm, "market"), "address", "bump");
        let live = Pubkey::find_program_address(&[SEED_LIVE_MARKET, mint.as_ref()], &program_id);
        assert_vector("live-market", live, field(gm, "live_market"), "address", "bump");
        let vault = Pubkey::find_program_address(&[SEED_VAULT, market.0.as_ref()], &program_id);
        assert_vector("vault", vault, field(gm, "vault"), "address", "bump");
        derived_markets.push(market.0);
    }

    let golden_positions = field(&golden, "positions")
        .as_array()
        .expect("golden positions is an array");
    assert_eq!(golden_positions.len(), 22, "golden position count — {REGENERATE}");
    for gp in golden_positions {
        let market_addr = field_str(gp, "market");
        let market = derived_markets
            .iter()
            .find(|m| m.to_string() == market_addr)
            .unwrap_or_else(|| panic!("golden position references underived market {market_addr}"));
        let user = pubkey(field_str(gp, "user"));
        let position = Pubkey::find_program_address(
            &[SEED_POSITION, market.as_ref(), user.as_ref()],
            &program_id,
        );
        assert_vector("position", position, gp, "address", "bump");
    }
}

#[test]
fn deployed_golden_vectors_match_fixture_bundle_stored_addresses() {
    // This comparison zips the two files by index, so it is
    // order-sensitive by design: the generator emits the golden
    // vectors in bundle order, and a reorder of either file fails
    // here (closed) until the vectors are regenerated. The
    // JavaScript counterpart in fixtures.test.js is key-based and
    // order-insensitive; key fields are asserted before addresses
    // below, so a one-sided reorder cannot pass silently.
    let golden = load_json("pda-vectors-deployed.json");
    let bundle = load_json("app-markets.json");
    assert_eq!(field_str(&bundle, "program_id"), DEPLOYED_PROGRAM_ID);

    let bundle_markets = field(&bundle, "markets")
        .as_array()
        .expect("bundle markets is an array");
    let golden_markets = field(&golden, "markets")
        .as_array()
        .expect("golden markets is an array");
    assert_eq!(bundle_markets.len(), golden_markets.len());
    for (bm, gm) in bundle_markets.iter().zip(golden_markets.iter()) {
        assert_eq!(field_str(bm, "subject_mint"), field_str(gm, "subject_mint"));
        assert_eq!(field_i64(bm, "created_at"), field_i64(gm, "created_at"));
        assert_eq!(
            field_str(bm, "address"),
            field_str(field(gm, "market"), "address"),
            "bundle market address is not the Rust-derived PDA — {REGENERATE}"
        );
        assert_eq!(
            field_i64(bm, "bump"),
            field(field(gm, "market"), "bump").as_i64().expect("golden market bump"),
            "bundle market bump mismatch — {REGENERATE}"
        );
        assert_eq!(
            field_str(bm, "live_market"),
            field_str(field(gm, "live_market"), "address"),
            "bundle live-market address is not the Rust-derived PDA — {REGENERATE}"
        );
        assert_eq!(
            field_i64(bm, "vault_bump"),
            field(field(gm, "vault"), "bump").as_i64().expect("golden vault bump"),
            "bundle vault bump mismatch — {REGENERATE}"
        );
    }

    let bundle_positions = field(&bundle, "positions")
        .as_array()
        .expect("bundle positions is an array");
    let golden_positions = field(&golden, "positions")
        .as_array()
        .expect("golden positions is an array");
    assert_eq!(bundle_positions.len(), golden_positions.len());
    for (bp, gp) in bundle_positions.iter().zip(golden_positions.iter()) {
        assert_eq!(field_str(bp, "market"), field_str(gp, "market"));
        assert_eq!(field_str(bp, "user"), field_str(gp, "user"));
        assert_eq!(
            field_str(bp, "address"),
            field_str(gp, "address"),
            "bundle position address is not the Rust-derived PDA — {REGENERATE}"
        );
        assert_eq!(
            field_i64(bp, "bump"),
            field_i64(gp, "bump"),
            "bundle position bump mismatch — {REGENERATE}"
        );
    }
}

#[test]
fn deployed_golden_config_matches_known_onchain_config() {
    let golden = load_json("pda-vectors-deployed.json");
    let config = field(&golden, "config");
    assert_eq!(field_str(config, "address"), KNOWN_ONCHAIN_CONFIG);
    assert_eq!(field_u64(config, "bump"), 252);
}
