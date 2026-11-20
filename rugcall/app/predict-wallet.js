"use strict";

// RugCall wallet layer.
//
// Two kinds of wallet live here:
// - an adapter over an injected browser provider (Phantom, Solflare
//   or Backpack), which delegates signing to that provider and never
//   pretends a signature happened when the live assembly path is not
//   available;
// - a demo wallet for fixture mode, which performs no cryptography
//   at all: its signing methods hand the descriptors back wrapped in
//   a result that says simulated, not signed, not submitted. It
//   never fabricates a signature string.
//
// The provider detection and network classification helpers are pure
// and can be exercised without a DOM: every window access is guarded
// and callers may pass an explicit root object instead.

var RugCallWallet = (function () {
  var DEVNET_GENESIS_HASH = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1";
  var MAINNET_GENESIS_HASH = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";

  function fail(code, message) {
    var err = new Error(message || code);
    err.code = code;
    return err;
  }

  function globalRoot() {
    if (typeof window !== "undefined") return window;
    if (typeof globalThis !== "undefined") return globalThis;
    return {};
  }

  function publicKeyString(pk) {
    if (pk === undefined || pk === null) return null;
    if (typeof pk === "string") return pk;
    if (typeof pk.toString === "function") {
      var text = pk.toString();
      return text && text !== "[object Object]" ? text : null;
    }
    return null;
  }

  // ------------------------------------------------------------------
  // Provider detection
  // ------------------------------------------------------------------

  function detectProviders(root) {
    var scope = root && typeof root === "object" ? root : globalRoot();
    var found = [];
    var phantom = null;
    if (scope.phantom && scope.phantom.solana) phantom = scope.phantom.solana;
    else if (scope.solana && scope.solana.isPhantom) phantom = scope.solana;
    if (phantom) found.push({ id: "phantom", provider: phantom, label: "Phantom" });
    var solflare = null;
    if (scope.solflare && scope.solflare.isSolflare) solflare = scope.solflare;
    if (solflare) found.push({ id: "solflare", provider: solflare, label: "Solflare" });
    if (scope.backpack) {
      found.push({ id: "backpack", provider: scope.backpack, label: "Backpack" });
    }
    return found;
  }

  // ------------------------------------------------------------------
  // Network classification
  //
  // A cluster is reported only when a field actually names one. The
  // absence of evidence is "unknown" — never a guessed default.
  // ------------------------------------------------------------------

  function classifyString(value) {
    if (typeof value !== "string" || !value) return null;
    var text = value.toLowerCase();
    if (text === DEVNET_GENESIS_HASH.toLowerCase()) return "devnet";
    if (text === MAINNET_GENESIS_HASH.toLowerCase()) return "mainnet";
    if (text.indexOf("devnet") !== -1) return "devnet";
    if (text.indexOf("mainnet") !== -1) return "mainnet";
    return null;
  }

  function classifyObject(obj) {
    if (!obj || typeof obj !== "object") return null;
    var fields = ["cluster", "network", "chain", "genesisHash", "genesis_hash",
      "rpcEndpoint", "rpc_endpoint", "endpoint", "url"];
    for (var i = 0; i < fields.length; i++) {
      var hit = classifyString(obj[fields[i]]);
      if (hit) return hit;
    }
    if (obj.connection && typeof obj.connection === "object") {
      return classifyString(obj.connection.rpcEndpoint) ||
        classifyString(obj.connection._rpcEndpoint);
    }
    return null;
  }

  function classifyNetwork(provider, probeResult) {
    if (provider && typeof provider === "object" && provider.kind === "demo") {
      return "devnet-fixture";
    }
    if (provider === "demo") return "devnet-fixture";
    var fromProbe = classifyString(probeResult) || classifyObject(probeResult);
    if (fromProbe) return fromProbe;
    var fromProvider = classifyObject(provider);
    if (fromProvider) return fromProvider;
    return "unknown";
  }

  // ------------------------------------------------------------------
  // Injected provider adapter
  // ------------------------------------------------------------------

  function isDescriptor(value) {
    return !!value && typeof value === "object" &&
      typeof value.programId === "string" && Array.isArray(value.keys) &&
      value.data instanceof Uint8Array;
  }

  function assembleTransaction(descriptor, root) {
    var web3 = root && root.solanaWeb3;
    if (!web3 && typeof window !== "undefined") web3 = window.solanaWeb3;
    if (!web3 || typeof web3.Transaction !== "function" ||
        typeof web3.TransactionInstruction !== "function" ||
        typeof web3.PublicKey !== "function") {
      return null;
    }
    var keys = descriptor.keys.map(function (k) {
      return {
        pubkey: new web3.PublicKey(k.pubkey),
        isSigner: !!k.isSigner,
        isWritable: !!k.isWritable,
      };
    });
    var instruction = new web3.TransactionInstruction({
      programId: new web3.PublicKey(descriptor.programId),
      keys: keys,
      data: typeof Buffer !== "undefined" ? Buffer.from(descriptor.data) : descriptor.data,
    });
    var tx = new web3.Transaction();
    tx.add(instruction);
    return tx;
  }

  function createWalletAdapter(provider, options) {
    if (!provider || typeof provider !== "object") {
      throw fail("NO_PROVIDER", "createWalletAdapter requires a provider object");
    }
    var opts = options || {};
    var adapter = {
      kind: "injected",
      id: opts.id || null,
      provider: provider,
      publicKey: publicKeyString(provider.publicKey),

      connect: async function () {
        if (typeof provider.connect !== "function") {
          throw fail("LIVE_UNAVAILABLE", "the provider has no connect method");
        }
        var result = await provider.connect();
        var pk = publicKeyString(provider.publicKey) ||
          (result && publicKeyString(result.publicKey));
        adapter.publicKey = pk;
        return { publicKey: pk };
      },

      disconnect: async function () {
        if (typeof provider.disconnect === "function") {
          await provider.disconnect();
        }
        adapter.publicKey = null;
        return { publicKey: null };
      },

      signTransaction: async function (tx) {
        if (isDescriptor(tx)) {
          var assembled = assembleTransaction(tx, opts.root || globalRoot());
          if (!assembled) {
            throw fail("LIVE_UNAVAILABLE",
              "live transaction assembly is not available in this environment");
          }
          if (typeof provider.signTransaction !== "function") {
            throw fail("LIVE_UNAVAILABLE", "the provider cannot sign transactions");
          }
          return provider.signTransaction(assembled);
        }
        if (typeof provider.signTransaction !== "function") {
          throw fail("LIVE_UNAVAILABLE", "the provider cannot sign transactions");
        }
        return provider.signTransaction(tx);
      },

      signAllTransactions: async function (txs) {
        var list = Array.isArray(txs) ? txs : [txs];
        var assembledList = [];
        for (var i = 0; i < list.length; i++) {
          if (isDescriptor(list[i])) {
            var assembled = assembleTransaction(list[i], opts.root || globalRoot());
            if (!assembled) {
              throw fail("LIVE_UNAVAILABLE",
                "live transaction assembly is not available in this environment");
            }
            assembledList.push(assembled);
          } else {
            assembledList.push(list[i]);
          }
        }
        if (typeof provider.signAllTransactions === "function") {
          return provider.signAllTransactions(assembledList);
        }
        if (typeof provider.signTransaction === "function") {
          var out = [];
          for (var j = 0; j < assembledList.length; j++) {
            out.push(await provider.signTransaction(assembledList[j]));
          }
          return out;
        }
        throw fail("LIVE_UNAVAILABLE", "the provider cannot sign transactions");
      },
    };
    return adapter;
  }

  // ------------------------------------------------------------------
  // Demo wallet (fixture mode)
  //
  // The demo wallet's public key is the fixture state's demo user.
  // Its signing methods are deliberately not signing: they return
  // the descriptors unchanged, wrapped in a result that states
  // simulated:true, signed:false, submitted:false.
  // ------------------------------------------------------------------

  function demoUserOf(fixtureState) {
    if (!fixtureState || typeof fixtureState !== "object") return null;
    if (typeof fixtureState.demoUser === "string") return fixtureState.demoUser;
    if (typeof fixtureState.demo_user === "string") return fixtureState.demo_user;
    if (typeof fixtureState.demo === "string") return fixtureState.demo;
    if (fixtureState.users && typeof fixtureState.users.demo === "string") {
      return fixtureState.users.demo;
    }
    if (typeof fixtureState.user === "string") return fixtureState.user;
    return null;
  }

  function createDemoWallet(fixtureState) {
    var demoUser = demoUserOf(fixtureState);
    var wallet = {
      kind: "demo",
      publicKey: demoUser,

      connect: async function () {
        return { publicKey: wallet.publicKey };
      },

      disconnect: async function () {
        return { publicKey: wallet.publicKey };
      },

      signTransaction: async function (descriptor) {
        return {
          simulated: true, signed: false, submitted: false,
          descriptors: descriptor,
        };
      },

      signAllTransactions: async function (descriptors) {
        return {
          simulated: true, signed: false, submitted: false,
          descriptors: Array.isArray(descriptors) ? descriptors : [descriptors],
        };
      },
    };
    return wallet;
  }

  return {
    DEVNET_GENESIS_HASH: DEVNET_GENESIS_HASH,
    MAINNET_GENESIS_HASH: MAINNET_GENESIS_HASH,
    detectProviders: detectProviders,
    createWalletAdapter: createWalletAdapter,
    createDemoWallet: createDemoWallet,
    classifyNetwork: classifyNetwork,
  };
})();

if (typeof module !== "undefined" && module.exports) {
  module.exports = RugCallWallet;
}
if (typeof window !== "undefined") {
  window.RugCallWallet = RugCallWallet;
}
if (typeof globalThis !== "undefined") {
  globalThis.RugCallWallet = RugCallWallet;
}
