"use strict";

// Contract tests for the resolver's two real network adapters,
// buildLiveDeps (mainnet evidence transport) and buildChainDeps
// (devnet chain seam). Both adapters are exercised here against a
// stubbed global fetch — no network — because the rest of the
// suite injects fakes at these seams, which is how a shape
// mismatch at the evidence seam survived every earlier test day:
// evidence.js documents deps.rpc(url, method, params) -> the full
// parsed JSON-RPC reply, while the adapter originally took
// (method, params) and returned reply.result only, so every live
// evidence fetch failed before the first family could load.
// Found on the first real-network exercise (2026-11-20).

const test = require("node:test");
const assert = require("node:assert/strict");

const { buildChainDeps, buildLiveDeps } = require("../resolver.js");
const ev = require("../../app/evidence.js");

const RPC_URL = "https://api.mainnet-beta.solana.com";
const MINT = "MintLiveDeps111111111111111111111111111111111";

function stubFetch(handler) {
  const calls = [];
  const original = globalThis.fetch;
  globalThis.fetch = async (url, init) => {
    const body = init && init.body ? JSON.parse(init.body) : null;
    calls.push({ url, body });
    const reply = handler(url, body);
    return { ok: true, status: 200, json: async () => reply };
  };
  return {
    calls,
    restore() {
      globalThis.fetch = original;
    },
  };
}

test("buildLiveDeps rpc follows the evidence.js transport contract", async () => {
  const replyBody = {
    jsonrpc: "2.0",
    id: 1,
    result: {
      context: { slot: 42 },
      value: { amount: "1000", decimals: 6 },
    },
  };
  const stub = stubFetch(() => replyBody);
  try {
    const deps = buildLiveDeps();
    const out = await deps.rpc(RPC_URL, "getTokenSupply", [MINT]);
    // The full parsed reply comes back: evidence.js reads
    // reply.result itself and hashes the whole reply.
    assert.deepEqual(out, replyBody);
    assert.equal(stub.calls.length, 1);
    assert.equal(stub.calls[0].url, RPC_URL);
    assert.equal(stub.calls[0].body.method, "getTokenSupply");
    assert.deepEqual(stub.calls[0].body.params, [MINT]);
  } finally {
    stub.restore();
  }
});

test("fetchEvidence loads the light family through the real buildLiveDeps", async () => {
  const stub = stubFetch((url, body) => {
    if (String(url).includes("api.dexscreener.com")) return { pairs: [] };
    switch (body && body.method) {
      case "getTokenLargestAccounts":
        return {
          jsonrpc: "2.0",
          id: 1,
          result: {
            context: { slot: 7 },
            value: [
              { address: "HolderA1111111111111111111111111111111111", amount: "600000000000000", decimals: 6, uiAmount: 600000000, uiAmountString: "600000000" },
              { address: "HolderB1111111111111111111111111111111111", amount: "100000000000000", decimals: 6, uiAmount: 100000000, uiAmountString: "100000000" },
            ],
          },
        };
      case "getTokenSupply":
        return {
          jsonrpc: "2.0",
          id: 1,
          result: {
            context: { slot: 7 },
            value: { amount: "1000000000000000", decimals: 6, uiAmount: 1000000000, uiAmountString: "1000000000" },
          },
        };
      default:
        return { jsonrpc: "2.0", id: 1, result: [] };
    }
  });
  try {
    const fetched = await ev.fetchEvidence(MINT, buildLiveDeps());
    assert.equal(fetched.fetchErrors.light, undefined);
    assert.ok(fetched.light, "light family must load through the real adapter");
    assert.equal(fetched.light.total, 1000000000000000);
    assert.equal(fetched.light.top1, 60);
  } finally {
    stub.restore();
  }
});

test("buildChainDeps sendRawTransaction uses the sendTransaction wire method", async () => {
  const stub = stubFetch((url, body) => {
    if (body && body.method === "sendTransaction") {
      return { jsonrpc: "2.0", id: 1, result: "SignatureResult111" };
    }
    return { jsonrpc: "2.0", id: 1, result: null };
  });
  try {
    const chain = buildChainDeps("https://api.devnet.solana.com");
    const sig = await chain.sendRawTransaction(Buffer.from([1, 2, 3]));
    assert.equal(sig, "SignatureResult111");
    const sendCall = stub.calls.find((c) => c.body && c.body.method);
    assert.ok(sendCall, "a JSON-RPC call must have been posted");
    assert.equal(sendCall.body.method, "sendTransaction");
    assert.equal(sendCall.body.params[0], Buffer.from([1, 2, 3]).toString("base64"));
  } finally {
    stub.restore();
  }
});
