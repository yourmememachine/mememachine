"use strict";

// Tests for rugcall/app/predict-tx.js: encoders, account lists,
// primitives, program derived addresses against the Rust-generated
// vectors, advisory preflight, and the fixture/live flows.

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const tx = require("../predict-tx.js");

const VECTORS = JSON.parse(
  fs.readFileSync(path.join(__dirname, "fixtures", "pda-vectors.json"), "utf8"));
const PROGRAM_ID = VECTORS.program_id;
const MINT = VECTORS.fixture_mint;
const USER = VECTORS.fixture_user;
const RESOLVER = "Dm95oxQp8YghEsCads1SXWkGwy4BaYLjCYv4fieG7GtH";
const COLLAT = "ConfigRugCa111111111111111111111111111111111";
const CREATED = 1799913600;
const EXPIRY = 1800000000;

function vector(name) {
  return VECTORS.vectors.find((v) => v.name === name);
}

function hex(bytes) {
  return Buffer.from(bytes).toString("hex");
}

// -------------------------------------------------------------------
// Encoders
// -------------------------------------------------------------------

test("encoder: create market is 33 bytes with the mint at [1..33)", () => {
  const data = tx.encodeCreateMarket(MINT);
  assert.equal(data.length, 33);
  assert.equal(data[0], 0x01);
  assert.deepEqual(Buffer.from(data.slice(1, 33)), Buffer.from(tx.base58Decode(MINT)));
});

test("encoder: create market rejects 31-byte and 33-byte mints", () => {
  assert.throws(() => tx.encodeCreateMarket(new Uint8Array(31)),
    (e) => e.code === "InvalidMintLength");
  assert.throws(() => tx.encodeCreateMarket(new Uint8Array(33)),
    (e) => e.code === "InvalidMintLength");
  // The corpus string that decodes to 33 bytes is rejected the same way.
  assert.throws(() => tx.encodeCreateMarket(VECTORS.requested_fixture_mint),
    (e) => e.code === "InvalidMintLength");
});

test("encoder: buy Flagged at min stake has the pinned byte layout", () => {
  const data = tx.encodeBuyPosition(1, "1000000");
  assert.equal(hex(data), "020140420f0000000000");
});

test("encoder: buy Survives at u64 max fills the amount with 0xff", () => {
  const data = tx.encodeBuyPosition(2, "18446744073709551615");
  assert.equal(data.length, 10);
  assert.equal(data[0], 0x02);
  assert.equal(data[1], 0x02);
  assert.ok(data.slice(2).every((b) => b === 0xff));
});

test("encoder: amount zero encodes (refusal is preflight's job)", () => {
  const data = tx.encodeBuyPosition(1, "0");
  assert.equal(hex(data), "02010000000000000000");
});

test("encoder: sides 0 and 3 are rejected", () => {
  assert.throws(() => tx.encodeBuyPosition(0, "1000000"), (e) => e.code === "InvalidSide");
  assert.throws(() => tx.encodeBuyPosition(3, "1000000"), (e) => e.code === "InvalidSide");
});

test("encoder: number, negative and out-of-range amounts are rejected", () => {
  assert.throws(() => tx.encodeBuyPosition(1, 1000000), (e) => e.code === "AmountType");
  assert.throws(() => tx.encodeBuyPosition(1, 1.5), (e) => e.code === "AmountType");
  assert.throws(() => tx.encodeBuyPosition(1, "-5"));
  assert.throws(() => tx.encodeBuyPosition(1, -5n));
  assert.throws(() => tx.encodeBuyPosition(1, "18446744073709551616"),
    (e) => e.code === "AmountOutOfRange");
});

test("encoder: claim is exactly [0x04]", () => {
  assert.deepEqual(Array.from(tx.encodeClaim()), [0x04]);
});

test("encoder: cancel is [0x05] and resolve matches the 35-byte layout", () => {
  assert.deepEqual(Array.from(tx.encodeCancelMarket()), [0x05]);
  const hash = new Uint8Array(32).fill(9);
  const data = tx.encodeResolveMarket(1, 3, hash);
  assert.equal(data.length, 35);
  const expected = Buffer.concat([Buffer.from([0x03, 1, 3]), Buffer.from(hash)]);
  assert.deepEqual(Buffer.from(data), expected);
});

// -------------------------------------------------------------------
// Account lists
// -------------------------------------------------------------------

const ADDRS = {
  programId: PROGRAM_ID,
  creator: USER,
  configAddress: vector("config").address,
  market: vector("market").address,
  liveMarket: vector("live-market").address,
  vault: vector("vault").address,
  position: vector("position").address,
  collateralMint: COLLAT,
  user: USER,
  userToken: "UserToken1111111111111111111111111111111111",
  caller: USER,
  subjectMint: MINT,
};

function K(pubkey, isSigner, isWritable) {
  return { pubkey, isSigner, isWritable };
}

test("metas: create market keys match the program test builder", () => {
  const ix = tx.buildCreateMarketIx(ADDRS);
  assert.equal(ix.programId, PROGRAM_ID);
  assert.deepEqual(ix.keys, [
    K(USER, true, true),
    K(ADDRS.configAddress, false, false),
    K(ADDRS.market, false, true),
    K(ADDRS.liveMarket, false, true),
    K(ADDRS.vault, false, true),
    K(COLLAT, false, false),
    K(tx.TOKEN_PROGRAM_ID, false, false),
    K(tx.SYSTEM_PROGRAM_ID, false, false),
  ]);
  assert.equal(ix.data.length, 33);
});

test("metas: buy position keys match the program test builder", () => {
  const ix = tx.buildBuyPositionIx({ ...ADDRS, side: 1, amount: "1000000" });
  assert.deepEqual(ix.keys, [
    K(USER, true, true),
    K(ADDRS.configAddress, false, false),
    K(ADDRS.market, false, true),
    K(ADDRS.vault, false, true),
    K(ADDRS.userToken, false, true),
    K(ADDRS.position, false, true),
    K(tx.TOKEN_PROGRAM_ID, false, false),
    K(tx.SYSTEM_PROGRAM_ID, false, false),
  ]);
  assert.equal(hex(ix.data), "020140420f0000000000");
});

test("metas: claim keys match the program test builder", () => {
  const ix = tx.buildClaimIx(ADDRS);
  assert.deepEqual(ix.keys, [
    K(USER, true, false),
    K(ADDRS.configAddress, false, false),
    K(ADDRS.market, false, false),
    K(ADDRS.vault, false, true),
    K(ADDRS.userToken, false, true),
    K(ADDRS.position, false, true),
    K(tx.TOKEN_PROGRAM_ID, false, false),
  ]);
  assert.deepEqual(Array.from(ix.data), [0x04]);
});

test("metas: cancel market keys match the program test builder", () => {
  const ix = tx.buildCancelMarketIx(ADDRS);
  assert.deepEqual(ix.keys, [
    K(USER, true, false),
    K(ADDRS.configAddress, false, false),
    K(ADDRS.market, false, true),
    K(ADDRS.liveMarket, false, true),
    K(USER, false, true),
  ]);
  assert.deepEqual(Array.from(ix.data), [0x05]);
});

test("metas: claim has no system program, its market is readonly, signer flags differ by instruction", () => {
  const claim = tx.buildClaimIx(ADDRS);
  assert.ok(!claim.keys.some((k) => k.pubkey === tx.SYSTEM_PROGRAM_ID));
  assert.equal(claim.keys[2].isWritable, false);
  const create = tx.buildCreateMarketIx(ADDRS);
  const buy = tx.buildBuyPositionIx({ ...ADDRS, side: 1, amount: "1000000" });
  const cancel = tx.buildCancelMarketIx(ADDRS);
  assert.deepEqual([create.keys[0].isSigner, create.keys[0].isWritable], [true, true]);
  assert.deepEqual([buy.keys[0].isSigner, buy.keys[0].isWritable], [true, true]);
  assert.deepEqual([claim.keys[0].isSigner, claim.keys[0].isWritable], [true, false]);
  assert.deepEqual([cancel.keys[0].isSigner, cancel.keys[0].isWritable], [true, false]);
});

// -------------------------------------------------------------------
// Primitives
// -------------------------------------------------------------------

test("primitives: base58 of the empty input is the empty string", () => {
  assert.equal(tx.base58Encode(new Uint8Array(0)), "");
  assert.deepEqual(Array.from(tx.base58Decode("")), []);
});

test("primitives: base58 of 32 zero bytes is 32 ones, and back", () => {
  const encoded = tx.base58Encode(new Uint8Array(32));
  assert.equal(encoded, "1".repeat(32));
  assert.deepEqual(Array.from(tx.base58Decode(encoded)), new Array(32).fill(0));
});

test("primitives: base58 roundtrips the resolver pubkey", () => {
  const bytes = tx.base58Decode(RESOLVER);
  assert.equal(bytes.length, 32);
  assert.equal(tx.base58Encode(bytes), RESOLVER);
});

test("primitives: base58 decode rejects characters outside the alphabet", () => {
  assert.throws(() => tx.base58Decode("0OIl"), (e) => e.code === "InvalidBase58");
  assert.throws(() => tx.base58Decode("abc!"), (e) => e.code === "InvalidBase58");
});

test("primitives: sha256 of the empty string matches the published digest", () => {
  assert.equal(tx.sha256Hex(""),
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
});

test("primitives: sha256 of abc matches the published digest", () => {
  assert.equal(tx.sha256Hex("abc"),
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
});

/* isOnCurve adversarial encodings, expected per Solana's curve
   rules (ed25519 decompression): y is read little-endian from the
   low 255 bits, the top bit is the sign of x, x^2 = (y^2-1)/(d y^2+1);
   an encoding is on-curve iff y < p and x^2 is a quadratic residue,
   or x^2 is zero with the sign bit CLEAR (a zero x with the sign
   bit set is rejected by decompression). */
function curveEncoding(y, signBit) {
  const bytes = new Uint8Array(32);
  let v = y;
  for (let i = 0; i < 32; i++) { bytes[i] = Number(v & 0xffn); v >>= 8n; }
  if (signBit) bytes[31] |= 0x80;
  return bytes;
}
const FIELD_P = (1n << 255n) - 19n;

test("curve: the identity point (y=1, x=0, sign clear) is on-curve", () => {
  assert.equal(tx.isOnCurve(curveEncoding(1n, 0)), true);
});

test("curve: the identity encoding with the sign bit set is rejected (zero x, negative sign)", () => {
  assert.equal(tx.isOnCurve(curveEncoding(1n, 1)), false);
});

test("curve: (0, -1) is on-curve with sign clear and rejected with the sign bit set", () => {
  assert.equal(tx.isOnCurve(curveEncoding(FIELD_P - 1n, 0)), true);
  assert.equal(tx.isOnCurve(curveEncoding(FIELD_P - 1n, 1)), false);
});

test("curve: y >= p encodings are not on-curve", () => {
  assert.equal(tx.isOnCurve(curveEncoding(FIELD_P, 0)), false);
  assert.equal(tx.isOnCurve(curveEncoding(FIELD_P + 1n, 0)), false);
  assert.equal(tx.isOnCurve(curveEncoding((1n << 255n) - 1n, 0)), false);
  assert.equal(tx.isOnCurve(curveEncoding((1n << 255n) - 1n, 1)), false);
});

test("primitives: sha256 of one million a bytes matches the published digest", () => {
  const input = new Uint8Array(1000000).fill(0x61);
  assert.equal(tx.sha256Hex(input),
    "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
});

// -------------------------------------------------------------------
// Program derived addresses (Rust vectors)
// -------------------------------------------------------------------

test("pda: config vector reproduces", () => {
  const got = tx.deriveConfig(PROGRAM_ID);
  assert.equal(got.address, vector("config").address);
  assert.equal(got.bump, vector("config").bump);
});

test("pda: live-market vector reproduces", () => {
  const got = tx.deriveLiveMarket(PROGRAM_ID, MINT);
  assert.equal(got.address, vector("live-market").address);
  assert.equal(got.bump, vector("live-market").bump);
});

test("pda: market vector reproduces", () => {
  const got = tx.deriveMarket(PROGRAM_ID, MINT, CREATED);
  assert.equal(got.address, vector("market").address);
  assert.equal(got.bump, vector("market").bump);
});

test("pda: vault vector reproduces", () => {
  const got = tx.deriveVault(PROGRAM_ID, vector("market").address);
  assert.equal(got.address, vector("vault").address);
  assert.equal(got.bump, vector("vault").bump);
});

test("pda: position vector reproduces", () => {
  const got = tx.derivePosition(PROGRAM_ID, vector("market").address, USER);
  assert.equal(got.address, vector("position").address);
  assert.equal(got.bump, vector("position").bump);
});

test("pda: associated token address vector reproduces", () => {
  const got = tx.deriveAssociatedTokenAddress(USER, MINT);
  assert.equal(got.address, vector("ata").address);
  assert.equal(got.bump, vector("ata").bump);
  assert.equal(tx.ATA_PROGRAM_ID, VECTORS.ata_program_id);
});

test("pda: one second later derives a different market", () => {
  const got = tx.deriveMarket(PROGRAM_ID, MINT, CREATED + 1);
  assert.notEqual(got.address, vector("market").address);
  assert.equal(got.address, vector("market-next-second").address);
  assert.equal(got.bump, vector("market-next-second").bump);
});

test("pda: derivation is deterministic, bumps are bytes, and the winning hash is off-curve", () => {
  const a = tx.deriveMarket(PROGRAM_ID, MINT, CREATED);
  const b = tx.deriveMarket(PROGRAM_ID, MINT, CREATED);
  assert.equal(a.address, b.address);
  assert.equal(a.bump, b.bump);
  for (const v of VECTORS.vectors) {
    assert.ok(Number.isInteger(v.bump) && v.bump >= 0 && v.bump <= 255, v.name);
  }
  // Rebuild the winning hash by hand: seeds, bump, program id, marker.
  const createdLe = new Uint8Array(8);
  new DataView(createdLe.buffer).setBigInt64(0, BigInt(CREATED), true);
  const seedParts = [
    Buffer.from("market", "utf8"),
    Buffer.from(tx.base58Decode(MINT)),
    Buffer.from(createdLe),
  ];
  const hash = tx.sha256Bytes(new Uint8Array(Buffer.concat([
    ...seedParts,
    Buffer.from([a.bump]),
    Buffer.from(tx.base58Decode(PROGRAM_ID)),
    Buffer.from("ProgramDerivedAddress", "utf8"),
  ])));
  assert.equal(tx.base58Encode(hash), a.address);
  assert.equal(tx.isOnCurve(hash), false);
});

// -------------------------------------------------------------------
// Preflight
// -------------------------------------------------------------------

const PF_CONFIG = { min_stake: "1000000", collateral_mint: COLLAT, resolver: RESOLVER };
const PF_WALLET = { publicKey: USER };
const PF_TOKEN = {
  address: "UserToken1111111111111111111111111111111111",
  owner: USER, mint: COLLAT, amount: "20000000",
};

function openMarket(overrides) {
  return {
    address: vector("market").address,
    subject_mint: MINT,
    creator: USER,
    created_at: CREATED,
    expiry: EXPIRY,
    flagged_pool: "3000000",
    survives_pool: "7000000",
    state: 1,
    outcome: 0,
    ...(overrides || {}),
  };
}

function buyCtx(overrides) {
  return {
    wallet: PF_WALLET, side: 1, amount: "1000000", config: PF_CONFIG,
    market: openMarket(), programId: PROGRAM_ID, userToken: PF_TOKEN,
    nowSec: CREATED + 3600,
    ...(overrides || {}),
  };
}

function firstError(result) {
  assert.equal(result.ok, false);
  assert.equal(result.errors.length, 1);
  return result.errors[0];
}

test("preflight: buy with side 0 fails with error 8", () => {
  const err = firstError(tx.preflightBuyPosition(buyCtx({ side: 0 })));
  assert.equal(err.code, "InvalidSide");
  assert.equal(err.rugCallError, 8);
});

test("preflight: buy below the config minimum, and buy of zero, fail with error 9", () => {
  const below = firstError(tx.preflightBuyPosition(buyCtx({ amount: "999999" })));
  assert.equal(below.code, "BelowMinStake");
  assert.equal(below.rugCallError, 9);
  const zero = firstError(tx.preflightBuyPosition(buyCtx({ amount: "0" })));
  assert.equal(zero.code, "BelowMinStake");
  assert.equal(zero.rugCallError, 9);
  // The minimum comes from the config object: a higher config minimum
  // rejects an amount the built-in constant would have accepted.
  const raised = firstError(tx.preflightBuyPosition(buyCtx({
    amount: "1500000", config: { ...PF_CONFIG, min_stake: "2000000" },
  })));
  assert.equal(raised.rugCallError, 9);
});

test("preflight: buy at exactly the minimum stake passes", () => {
  const result = tx.preflightBuyPosition(buyCtx({ amount: "1000000" }));
  assert.equal(result.ok, true);
  assert.deepEqual(result.errors, []);
});

test("preflight: buy on a resolved or cancelled market fails with error 10", () => {
  for (const state of [2, 3]) {
    const err = firstError(tx.preflightBuyPosition(
      buyCtx({ market: openMarket({ state }) })));
    assert.equal(err.code, "MarketNotOpen");
    assert.equal(err.rugCallError, 10);
  }
});

test("preflight: buy at expiry fails with error 11, one second before passes", () => {
  const at = firstError(tx.preflightBuyPosition(buyCtx({ nowSec: EXPIRY })));
  assert.equal(at.code, "BuyAfterExpiry");
  assert.equal(at.rugCallError, 11);
  const before = tx.preflightBuyPosition(buyCtx({ nowSec: EXPIRY - 1 }));
  assert.equal(before.ok, true);
});

test("preflight: buy with a wrong-mint token account fails with error 5", () => {
  const err = firstError(tx.preflightBuyPosition(buyCtx({
    userToken: { ...PF_TOKEN, mint: MINT },
  })));
  assert.equal(err.code, "MintMismatch");
  assert.equal(err.rugCallError, 5);
});

test("preflight: buy above the token balance is the runtime InsufficientFunds", () => {
  const err = firstError(tx.preflightBuyPosition(buyCtx({
    amount: "20000000",
    userToken: { ...PF_TOKEN, amount: "5000000" },
  })));
  assert.equal(err.code, "InsufficientFunds");
  assert.equal(err.rugCallError, null);
  assert.equal(err.runtimeError, "InsufficientFunds");
});

test("preflight: claim on an open market fails with error 17", () => {
  const err = firstError(tx.preflightClaim({
    market: openMarket(),
    position: { flagged_amount: "1000000", survives_amount: "0", claimed: false },
  }));
  assert.equal(err.code, "ClaimBeforeResolve");
  assert.equal(err.rugCallError, 17);
});

test("preflight: claim with no position is POSITION_NOT_FOUND", () => {
  const err = firstError(tx.preflightClaim({
    market: openMarket({ state: 2, outcome: 1 }),
    position: null,
  }));
  assert.equal(err.code, "POSITION_NOT_FOUND");
  assert.equal(err.rugCallError, null);
  assert.equal(err.onChainError, "InvalidAccountData");
});

test("preflight: claim on a claimed position fails with error 18", () => {
  const err = firstError(tx.preflightClaim({
    market: openMarket({ state: 2, outcome: 1 }),
    position: { flagged_amount: "1000000", survives_amount: "0", claimed: true },
  }));
  assert.equal(err.code, "AlreadyClaimed");
  assert.equal(err.rugCallError, 18);
});

test("preflight: claim branch precedence - cancelled refund, zero-pool refund, zero winning stake", () => {
  const cancelled = tx.preflightClaim({
    market: openMarket({ state: 3, outcome: 0 }),
    position: { flagged_amount: "1000000", survives_amount: "2500000", claimed: false },
  });
  assert.equal(cancelled.ok, true);
  assert.equal(cancelled.branch, "refund");
  assert.equal(cancelled.refundReason, "cancelled");
  assert.equal(cancelled.amount, "3500000");

  const zeroPool = tx.preflightClaim({
    market: openMarket({ state: 2, outcome: 1, flagged_pool: "0", survives_pool: "7000000" }),
    position: { flagged_amount: "0", survives_amount: "7000000", claimed: false },
  });
  assert.equal(zeroPool.ok, true);
  assert.equal(zeroPool.branch, "refund");
  assert.equal(zeroPool.refundReason, "zero-winning-pool");
  assert.equal(zeroPool.amount, "7000000");

  const noStake = firstError(tx.preflightClaim({
    market: openMarket({ state: 2, outcome: 1 }),
    position: { flagged_amount: "0", survives_amount: "7000000", claimed: false },
  }));
  assert.equal(noStake.code, "NoClaimableStake");
  assert.equal(noStake.rugCallError, 19);
});

test("preflight: claim payout preview for the canonical pools", () => {
  const result = tx.preflightClaim({
    market: openMarket({ state: 2, outcome: 1 }),
    position: { flagged_amount: "1000000", survives_amount: "0", claimed: false },
  });
  assert.equal(result.ok, true);
  assert.equal(result.branch, "payout");
  assert.equal(result.amount, "3333333");
  assert.equal(tx.parimutuelPayout("1000000", "10000000", "3000000"), "3333333");
});

test("preflight: cancel gates - stranger at the deadline, stranger after it, resolver early, resolved market", () => {
  const stranger = { callerAddress: USER, config: PF_CONFIG, market: openMarket() };
  const atDeadline = firstError(tx.preflightCancelMarket(
    { ...stranger, nowSec: EXPIRY + 86400 }));
  assert.equal(atDeadline.code, "CancelTooEarly");
  assert.equal(atDeadline.rugCallError, 20);

  const after = tx.preflightCancelMarket({ ...stranger, nowSec: EXPIRY + 86400 + 1 });
  assert.equal(after.ok, true);
  assert.equal(after.isResolver, false);

  const resolverEarly = tx.preflightCancelMarket({
    callerAddress: RESOLVER, config: PF_CONFIG, market: openMarket(), nowSec: CREATED,
  });
  assert.equal(resolverEarly.ok, true);
  assert.equal(resolverEarly.isResolver, true);

  const resolved = firstError(tx.preflightCancelMarket(
    { ...stranger, market: openMarket({ state: 2, outcome: 1 }), nowSec: EXPIRY + 86400 + 9999 }));
  assert.equal(resolved.code, "AlreadyResolved");
  assert.equal(resolved.rugCallError, 13);

  const cancelled = firstError(tx.preflightCancelMarket(
    { ...stranger, market: openMarket({ state: 3 }), nowSec: EXPIRY + 86400 + 9999 }));
  assert.equal(cancelled.code, "AlreadyCancelled");
  assert.equal(cancelled.rugCallError, 14);
});

// -------------------------------------------------------------------
// Flows
// -------------------------------------------------------------------

function flowClient(walletKey, data, nowMs) {
  return tx.createPredictClient({
    programId: PROGRAM_ID,
    config: {
      address: vector("config").address,
      resolver: RESOLVER,
      collateral_mint: COLLAT,
      min_stake: "1000000",
      default_duration_secs: 86400,
    },
    wallet: { kind: "demo", publicKey: walletKey },
    mode: "fixture",
    now: () => nowMs,
    data,
  });
}

function ataFor(user) {
  return tx.deriveAssociatedTokenAddress(user, COLLAT).address;
}

function assertSimulated(envelope) {
  assert.equal(envelope.mode, "fixture");
  assert.equal(envelope.signed, false);
  assert.equal(envelope.submitted, false);
  assert.equal(envelope.status, "simulated");
  assert.equal(envelope.signature, null);
  assert.equal(envelope.explorerUrl, null);
}

test("flows: fixture create, buys and claim keep the envelopes honest and the books balanced", () => {
  const userC = COLLAT; // a third distinct 32-byte address from the corpus
  const data = {
    markets: [],
    positions: [],
    tokens: [
      { address: ataFor(USER), owner: USER, mint: COLLAT, amount: "20000000" },
      { address: ataFor(RESOLVER), owner: RESOLVER, mint: COLLAT, amount: "20000000" },
      { address: ataFor(userC), owner: userC, mint: COLLAT, amount: "20000000" },
    ],
  };
  const nowMs = CREATED * 1000;
  const clientA = flowClient(USER, data, nowMs);
  const clientB = flowClient(RESOLVER, data, nowMs);
  const clientC = flowClient(userC, data, nowMs);

  const created = clientA.createMarket(MINT);
  assert.equal(created.ok, true);
  assertSimulated(created);
  assert.equal(created.instruction.name, "CreateMarket");
  assert.equal(created.derived.market.address, vector("market").address);
  const marketAddress = created.market.address;
  const market = data.markets.find((m) => m.address === marketAddress);
  assert.equal(market.state, 1);
  assert.equal(market.expiry, CREATED + 86400);
  assert.equal(data.liveMarkets.length, 1);

  // The vault's token record joins once its address is known.
  const vaultAddress = created.derived.vault.address;
  data.tokens.push({ address: vaultAddress, owner: marketAddress, mint: COLLAT, amount: "0" });
  const vaultToken = data.tokens[data.tokens.length - 1];

  function conservation() {
    const flagged = BigInt(market.flagged_pool);
    const survives = BigInt(market.survives_pool);
    let posFlagged = 0n, posSurvives = 0n;
    for (const p of data.positions) {
      posFlagged += BigInt(p.flagged_amount);
      posSurvives += BigInt(p.survives_amount);
    }
    assert.equal(posFlagged, flagged);
    assert.equal(posSurvives, survives);
    return flagged + survives;
  }

  const buyA = clientA.buyPosition(marketAddress, 1, "1000000");
  assert.equal(buyA.ok, true);
  assertSimulated(buyA);
  assert.equal(conservation(), 1000000n);
  assert.equal(vaultToken.amount, "1000000");

  const buyB = clientB.buyPosition({ market: marketAddress, side: 1, amount: "2000000" });
  assert.equal(buyB.ok, true);
  assertSimulated(buyB);
  assert.equal(conservation(), 3000000n);

  const buyC = clientC.buyPosition(marketAddress, "survives", "7000000");
  assert.equal(buyC.ok, true);
  assertSimulated(buyC);
  assert.equal(conservation(), 10000000n);
  assert.equal(vaultToken.amount, "10000000");
  assert.equal(market.flagged_pool, "3000000");
  assert.equal(market.survives_pool, "7000000");

  // The market resolves Flagged in the fixture state (the resolver's
  // own step, outside this layer), then the claim pays pro-rata.
  market.state = 2;
  market.outcome = 1;
  const tokenA = data.tokens[0];
  const balanceBefore = BigInt(tokenA.amount);
  const claimA = clientA.claim(marketAddress);
  assert.equal(claimA.ok, true);
  assertSimulated(claimA);
  assert.equal(claimA.branch, "payout");
  assert.equal(claimA.amount, "3333333");
  assert.equal(BigInt(tokenA.amount), balanceBefore + 3333333n);
  assert.equal(vaultToken.amount, "6666667");
  const positionA = data.positions.find((p) => p.user === USER);
  assert.equal(positionA.claimed, true);
  // Pools are frozen by the claim: conservation still holds.
  assert.equal(conservation(), 10000000n);
});

test("flows: live mode with no program id refuses every flow and builds nothing", () => {
  const data = { markets: [], positions: [] };
  const client = tx.createPredictClient({
    programId: null,
    config: { resolver: RESOLVER, collateral_mint: COLLAT, min_stake: "1000000" },
    wallet: { kind: "demo", publicKey: USER },
    mode: "live",
    now: () => CREATED * 1000,
    data,
  });
  for (const result of [
    client.createMarket(MINT),
    client.buyPosition(vector("market").address, 1, "1000000"),
    client.claim(vector("market").address),
    client.cancelMarket(vector("market").address),
  ]) {
    assert.equal(result.ok, false);
    assert.equal(result.code, "PROGRAM_NOT_DEPLOYED");
    assert.equal(result.instruction, null);
    assert.equal(result.signature, null);
    assert.equal(result.explorerUrl, null);
  }
  assert.equal(data.markets.length, 0);
  assert.equal(data.positions.length, 0);
});

test("flows: live mode refuses fixture-sourced state with MODE_MISMATCH", () => {
  const client = tx.createPredictClient({
    programId: PROGRAM_ID,
    config: { resolver: RESOLVER, collateral_mint: COLLAT, min_stake: "1000000" },
    wallet: { kind: "demo", publicKey: USER },
    mode: "live",
    now: () => CREATED * 1000,
    data: { source: "fixture", markets: [], positions: [] },
  });
  const result = client.createMarket(MINT);
  assert.equal(result.ok, false);
  assert.equal(result.code, "MODE_MISMATCH");
  assert.equal(result.instruction, null);
});
