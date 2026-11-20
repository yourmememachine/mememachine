"use strict";

// Tests for rugcall/app/predict-wallet.js: provider detection, the
// injected-provider adapter, the demo wallet, and network
// classification. Everything runs without a DOM.

const test = require("node:test");
const assert = require("node:assert/strict");

const wallet = require("../predict-wallet.js");
const tx = require("../predict-tx.js");

const USER = "CreatorWa11et1111111111111111111111111111111";
const RESOLVER = "Dm95oxQp8YghEsCads1SXWkGwy4BaYLjCYv4fieG7GtH";

test("wallet: detectProviders finds phantom, solflare and backpack in a fake root", () => {
  const phantomProvider = { isPhantom: true };
  const solflareProvider = { isSolflare: true };
  const backpackProvider = {};
  const found = wallet.detectProviders({
    solana: phantomProvider,
    solflare: solflareProvider,
    backpack: backpackProvider,
  });
  assert.deepEqual(found.map((f) => f.id), ["phantom", "solflare", "backpack"]);
  assert.deepEqual(found.map((f) => f.label), ["Phantom", "Solflare", "Backpack"]);
  assert.equal(found[0].provider, phantomProvider);
  assert.equal(found[1].provider, solflareProvider);
  assert.equal(found[2].provider, backpackProvider);
});

test("wallet: detectProviders prefers the phantom namespace and finds nothing in an empty root", () => {
  const nested = { isPhantom: true, via: "phantom.solana" };
  const found = wallet.detectProviders({
    phantom: { solana: nested },
    solana: { isPhantom: true, via: "window.solana" },
  });
  assert.equal(found.length, 1);
  assert.equal(found[0].provider, nested);
  assert.deepEqual(wallet.detectProviders({}), []);
  assert.deepEqual(wallet.detectProviders(), []);
});

test("wallet: demo wallet sign returns the identical descriptor bytes, simulated and unsigned", async () => {
  const demo = wallet.createDemoWallet({ demoUser: USER });
  assert.equal(demo.kind, "demo");
  assert.equal(demo.publicKey, USER);
  const connected = await demo.connect();
  assert.equal(connected.publicKey, USER);

  const descriptor = tx.buildClaimIx({
    programId: "6YMEjhBqVTMaSRWcmVkLrnHZ22FWEDJEpTeonAg8GKSy",
    user: USER,
    configAddress: RESOLVER,
    market: RESOLVER,
    vault: RESOLVER,
    userToken: RESOLVER,
    position: RESOLVER,
  });
  const result = await demo.signTransaction(descriptor);
  assert.equal(result.simulated, true);
  assert.equal(result.signed, false);
  assert.equal(result.submitted, false);
  assert.ok(!("signature" in result));
  assert.equal(result.descriptors, descriptor);
  assert.deepEqual(
    Buffer.from(result.descriptors.data), Buffer.from(descriptor.data));

  const all = await demo.signAllTransactions([descriptor]);
  assert.equal(all.simulated, true);
  assert.equal(all.signed, false);
  assert.equal(all.descriptors[0], descriptor);
});

test("wallet: demo wallet reads the demo user from the app fixture shape", () => {
  const demo = wallet.createDemoWallet({ users: { demo: USER } });
  assert.equal(demo.publicKey, USER);
});

test("wallet: a stub provider receives a ready transaction object unchanged", async () => {
  const received = [];
  const stubProvider = {
    publicKey: RESOLVER,
    connect: async () => ({ publicKey: RESOLVER }),
    disconnect: async () => {},
    signTransaction: async (t) => { received.push(t); return t; },
  };
  const adapter = wallet.createWalletAdapter(stubProvider);
  assert.equal(adapter.kind, "injected");
  const conn = await adapter.connect();
  assert.equal(conn.publicKey, RESOLVER);
  assert.equal(adapter.publicKey, RESOLVER);

  const readyTx = { fake: "already-assembled" };
  const signed = await adapter.signTransaction(readyTx);
  assert.equal(received.length, 1);
  assert.equal(received[0], readyTx);
  assert.equal(signed, readyTx);
});

test("wallet: signing a raw descriptor without live assembly rejects LIVE_UNAVAILABLE", async () => {
  const stubProvider = {
    publicKey: RESOLVER,
    signTransaction: async (t) => t,
  };
  const adapter = wallet.createWalletAdapter(stubProvider, { root: {} });
  const descriptor = tx.buildCancelMarketIx({
    programId: "6YMEjhBqVTMaSRWcmVkLrnHZ22FWEDJEpTeonAg8GKSy",
    caller: USER,
    configAddress: RESOLVER,
    market: RESOLVER,
    liveMarket: RESOLVER,
    creator: USER,
  });
  await assert.rejects(
    () => adapter.signTransaction(descriptor),
    (e) => e.code === "LIVE_UNAVAILABLE");
});

test("wallet: classifyNetwork names mainnet and devnet from exposed fields", () => {
  assert.equal(wallet.classifyNetwork({ cluster: "mainnet-beta" }, null), "mainnet");
  assert.equal(wallet.classifyNetwork({}, { rpcEndpoint: "https://api.devnet.solana.com" }),
    "devnet");
  assert.equal(wallet.classifyNetwork(null,
    { genesisHash: "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d" }), "mainnet");
  assert.equal(wallet.classifyNetwork(null,
    { genesisHash: "EtWTRABZaYq6iMfeYKouRu166VU2xqa1" }), "devnet");
});

test("wallet: classifyNetwork never guesses - absence is unknown, demo is devnet-fixture", () => {
  assert.equal(wallet.classifyNetwork({}, null), "unknown");
  assert.equal(wallet.classifyNetwork(null, null), "unknown");
  assert.equal(wallet.classifyNetwork({ isPhantom: true }, {}), "unknown");
  const demo = wallet.createDemoWallet({ demoUser: USER });
  assert.equal(wallet.classifyNetwork(demo, null), "devnet-fixture");
});
