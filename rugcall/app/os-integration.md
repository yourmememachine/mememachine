# RugCall in Solana OS — integration notes

How the RugCall app wires into Solana OS, and what is deliberately
not wired yet. This document describes the integration contract; it
does not modify the live OS artifact.

## App registry entry

Solana OS keeps an `APPS` registry and opens app windows through its
existing `openApp(id)` machinery (same chrome, drag, and taskbar
behaviour as every other app). RugCall adds one entry:

```js
{ id: "predict", name: "RugCall", sub: "will it get flagged?", tag: "New" }
```

The icon key follows the registry's existing icon naming when the
RugCall-enabled build is assembled; the entry above is the binding
part — id `predict`, name `RugCall`, the sub-line, and the `New` tag.

## Window mounting

`openApp("predict")` creates the app window and mounts the same UI
module the standalone judge page uses:

- `predict-ui.js` renders into the window body through
  `RugCallUI.mountApp(root, deps)` — the market list, market detail
  with the evidence panel, buy/claim forms, create flow, and the
  portfolio strip are the same code in both surfaces.
- `deps.dataSource` comes from `predict.js`
  (`RugCall.createDataSource`), `deps.client` from `predict-tx.js`
  (`RugCallTx.createPredictClient`), and `deps.wallet` from
  `predict-wallet.js` (a provider adapter or the demo wallet).
- The evidence panel evaluates recorded evidence through
  `evidence.js` (`evaluateEvidence` with an explicit crew store).

Entry points from neighbouring apps, for the RugCall-enabled build:
Pump Radar rows gain an "Open market" action (and a chip linking to
an existing live market for the mint), and the check surfaces gain a
link to the mint's market. Those hooks live in the OS build, not in
this module set.

## Dual-RPC configuration

Two clusters are in play at all times, and the app header states
both:

- **Evidence reads stay on mainnet.** The evidence module reads the
  subject token from Solana mainnet (the public mainnet endpoint, or
  the hackathon RPC endpoint when configured). Subject tokens are
  real mainnet launches.
- **Markets live on devnet.** Market accounts, positions, the
  collateral vault, and every transaction target a single
  `DEVNET_RPC` / markets-program configuration constant. The subject
  mint is stored in the market account as data; no devnet account
  for the subject token is created.

The split is user-visible furniture, not a footnote: the persistent
banner reads "Evidence: Mainnet · Markets: Devnet", collateral is
labelled devnet play money (dUSDC), and a wallet connected on the
wrong network gets the amber "Markets live on Devnet — set your
wallet to Devnet" banner until `classifyNetwork` reports devnet.

## Wallet patterns reused

`predict-wallet.js` follows the wallet patterns the OS already uses:

- Provider detection (`detectProviders`) covers the providers the OS
  already supports — Phantom, Solflare, and Backpack — and the app
  offers a provider chooser next to the demo wallet button.
- `createWalletAdapter(provider)` wraps a detected provider behind
  the sign interface the predict client consumes.
- `createDemoWallet(fixtureState)` is the built-in demo identity:
  pre-funded devnet play money with no faucet step. In fixture mode
  it is simulated end to end — its sign methods return descriptors
  marked simulated, and nothing is signed for real or sent.
- Network classification (`classifyNetwork`) drives the
  wrong-network banner described above.

## What ships when

- The standalone judge page (`index.html`, generated into the
  self-contained `judge.html` by `build-judge-page.js`) ships with
  the hackathon submission. It runs the full app against fixture
  markets through the real resolver artifacts.
- The RugCall-enabled OS build is sequenced **after** the Dev3pack
  submission (Nov 27–29), because the current OS build is the
  Dev3pack demo surface. The live OS artifact is **not** modified by
  this work: no registry entry, window, or hook described here exists
  in the live OS yet.
- The OS does **not** consume `evidence.js` today. The module in
  this repo and the verdict code inside the current OS build are two
  implementations kept in agreement by the comparison tests in
  `rugcall/resolver/test/`; the extraction that would let the OS load
  the shared module directly happens on a copy, with the
  RugCall-enabled build.

## Trust disclosures (v1)

The trust limits of the v1 design, stated as plainly in the
submission package and recorded here:

- **One resolver key resolves in v1.** A single named resolver key
  posts resolutions. The mitigation — not a fix — is that every
  resolution is hash-bound to a published evidence bundle that
  anyone can recompute and re-derive with `verifyBundle`. A 3-of-5
  resolver quorum (the bridge's pattern) is roadmap, not built.
- **F3 — the resolver can cancel any Open market at any time,
  on-chain.** The 24-hour grace the resolver observes after expiry
  before cancelling an unresolved market is resolver policy, not
  program enforcement. (Anyone else can cancel only after expiry
  plus the 24-hour grace, and that part is program-enforced.)
- **F4 — late evidence resolves; it does not refund.** If complete
  evidence first becomes available after the grace window, the
  market resolves on it rather than cancelling. The grace window is
  not a deadline after which every unresolved market refunds.
- **F5 — the event-time rule is resolver-enforced, not
  program-enforced.** A trigger counts only if its *event*
  timestamp is ≤ the market's expiry. The program sees only the
  outcome and the evidence hash; the bundle records both the event
  and detection timestamps so the rule stays auditable.
- **Completeness is global.** A market resolves only on a complete
  evidence snapshot; a proven trigger can still refund if an
  unrelated evidence family stays incomplete past grace.
- **No dispute or appeal in v1.** A posted resolution is final in
  the program; the check on a bad one is the published bundle.

## Honest remaining pieces

- **Live send path.** Signing and sending real devnet transactions
  from the OS needs the vendored web3.js UMD decision (which build,
  vendored into the artifact or loaded from a CDN) before the live
  mode of `predict-tx.js` can run inside the OS window. Until then,
  the OS surface runs the same fixture/simulated path as the judge
  page, labelled as such.
- **Demo wallet funding.** The demo wallet is simulated until a
  server-side treasury faucet exists to fund it with real devnet
  dUSDC and rate-limit top-ups per wallet. The "Get dUSDC" flow
  waits on that service; nothing about the current demo wallet
  implies a funded on-chain account.
- **Program id.** The program id is in the app config (the single
  `programId` binding the judge page and the OS window both read):
  the program was deployed to devnet on 2026-11-20 as
  `DcwvEDi4yCk16wQ2HVzc3dQiBFccj8VxzboGUYdUXseZ`. The judge page
  binds that real id in fixture mode — its markets are labelled
  fixtures and its transactions are simulated; live devnet reads
  go through the app's data layer.
