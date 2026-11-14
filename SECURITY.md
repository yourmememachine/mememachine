# Machine — Security Policy and Bug Bounty

**Status:** Active. This document states how to report a security issue in
Machine, what is in scope, and what researchers can expect. It is a
disclosure policy, not a marketing document.

- **Chain:** Machine · **Token:** $MACHINE
- **Repository:** https://github.com/yourmememachine/mememachine
- **Chain identity:** Solana fork (Agave 2.3.13), custom genesis
  `32W8ouqrHbuhE3PM6ZsJrJcDbP7Tx9aC2zRvrEabkQSb`, native gas token
  $MACHINE, fixed supply 1,000,000,000.
- **Current state:** single-node localnet, voting and finalizing. The
  3-validator live net is the next step (see `livenet/` in the repo). No
  bridge exists yet — `BRIDGE.md` is a design document only. No audit has
  been performed yet.

---

## 1. Scope

### In scope

| Area | What it covers |
|---|---|
| Validator configuration and genesis | The genesis files in `ledger/`, `GENESIS.md`, `patch-vote-authority.py`, `install.sh`, `start-validator.sh`, and `run-in-ns.sh` — anything that affects how the chain boots, votes, confirms, or finalizes. |
| Livenet deployment package | Everything under `livenet/` — key generation, genesis generation, validator systemd units, cluster checks, port and firewall documentation. |
| Machine website | The public Machine website (the deployed site and its source artifact). Findings must be about the site itself, not about the hosting provider's infrastructure. |
| Rug-Farm Registry hosted API | The public read API at `rug-farm-registry.onrender.com` — read-only testing only. Findings about incorrect registry data are data-quality issues, not security issues, unless they demonstrate an integrity or access-control failure. |

### Out of scope

- **Bridge programs.** No bridge program is written or deployed. The
  design in `BRIDGE.md` is not code; reviewing the design document for
  logic errors is welcome and will be credited, but there is no running
  bridge to test.
- **Third-party wallets.** Phantom, Solflare, Backpack, and any other
  wallet software are the vendors' responsibility.
- **Social engineering, phishing, and physical attacks** against any
  person involved in the project.
- **Denial-of-service testing** against any hosted endpoint, including
  the Rug-Farm Registry API. Do not degrade a service other people use.
- **The localnet itself as a halt target.** The single-node localnet is
  a development environment. Testing that halts it, corrupts its ledger,
  or exhausts its resources is out of scope — report the underlying bug
  from the code instead.
- **Reports that require a live mainnet to reproduce.** There is no live
  mainnet yet. A finding must be demonstrable against the code and
  configuration in the repository.

---

## 2. Responsible disclosure

### How to report

Open a **private security advisory** on the Machine repository
(Repository → Security → Advisories → New draft security advisory).
Do not open a public issue for a security finding, and do not post
details publicly before a fix is available.

There is no dedicated security email address. The private advisory on
the repository is the only reporting channel.

### What to include

- A clear description of the issue and the component it affects.
- The impact: what an attacker could actually do (funds at risk, chain
  halt, consensus breakage, data exposure), with a concrete scenario.
- Steps to reproduce, against the code and configuration in the
  repository. Include the commit you tested against.
- Any suggested fix, if you have one. This is optional but appreciated.
- Your preferred name or handle for the Hall of Fame (§5).

### Response timeline

These are targets, not promises. The project is run by a small team and
response times vary with severity and available time.

- **Acknowledgement:** within 5 business days of the report.
- **Triage and severity assessment:** within 10 business days of
  acknowledgement.
- **Fix:** timing depends on severity. A Critical issue is worked on
  first; lower severities are scheduled alongside other development.
  You will be told the expected fix window once triage is complete.
- **Disclosure:** once a fix is merged and deployed, the advisory is
  published and the reporter is credited (unless they ask not to be).
  Do not disclose the issue publicly before the fix is available.

### Safe harbor

Good-faith security research conducted under this policy will not result
in legal action or retaliation from the project, provided the researcher:

- reports through the private advisory channel above;
- does not access, modify, or exfiltrate data belonging to anyone else;
- does not degrade or halt any service;
- does not disclose the issue publicly before a fix is available;
- follows the rules in §4.

This is a statement of intent by the project maintainers. It is not a
legal agreement and does not bind any third party.

---

## 3. Bug bounty structure

### Severity tiers

| Severity | Definition |
|---|---|
| **Critical** | Direct loss or theft of funds; infinite or unauthorized minting of $MACHINE or any bridged asset; a consensus breakage that lets an attacker finalize invalid state; remote compromise of a validator host through the chain software or the deployment scripts. |
| **High** | Chain halt or liveness failure caused by a single malformed transaction or message; bypass of a documented safety check (replay protection, finality rule, pause path); unauthorized access to validator keys or to the lock vault's authority path; a vulnerability that makes a Critical issue reachable with one additional step. |
| **Medium** | A weakness that requires unusual conditions or a second vulnerability to cause harm; information disclosure that meaningfully aids an attack (for example, leaking validator keys in logs); a configuration error in the livenet package that weakens security without directly exposing funds. |
| **Low** | Minor issues with limited impact: best-practice deviations, unclear or missing security documentation, low-severity information disclosure, or a bug that requires an already-compromised environment to exploit. |

Severity is assigned by the maintainers during triage. A report's impact
is judged on what an attacker could actually do, not on the theoretical
maximum. Duplicate reports are credited to the first reporter only.

### Rewards

**Cash rewards are not funded right now.** The project has no bounty
budget and no sponsor. Do not expect a cash payment for a report
submitted today.

What is offered now:

- **Public recognition** in the Hall of Fame (§5) and in the published
  security advisory, under the name or handle you choose.
- **A future $MACHINE allocation** for qualifying reports, to be defined
  when the live network launches. The exact terms — which severities
  qualify, how much, and how it is paid — will be published at that
  time. **No amount is promised here, and no allocation exists until
  those terms are published.**

Reports that do not qualify for a reward (out-of-scope findings, already
known issues, duplicates, or issues with no demonstrated impact) still
receive a thank-you and, where useful, a public credit for the report.

---

## 4. Rules

- **One report per issue.** If one root cause produces several symptoms,
  report it once. Multiple reports of the same issue are duplicates.
- **First reporter gets the credit.** Duplicates are closed with a
  reference to the original report.
- **Do not halt the localnet.** Do not test against the running localnet
  in any way that stops it, corrupts its ledger, or exhausts its
  resources. Reproduce against the code, or against a validator you run
  yourself.
- **Do not access other people's data.** No reading, modifying, or
  exfiltrating anything that belongs to anyone else.
- **No spam.** Do not submit bulk automated reports, low-effort scanner
  output, or reports with no reproduction steps. Automated scanner
  output without a demonstrated impact will be closed without review.
- **No social engineering.** No phishing, pretexting, or impersonation
  of maintainers, users, or third parties.
- **Do not test the bridge.** There is nothing to test; the bridge is a
  design document. Review the design on paper instead.
- **Follow the law.** Research must stay within applicable law in your
  jurisdiction.

Breaking these rules voids safe-harbor protection and may disqualify a
report from any reward.

---

## 5. Hall of Fame

Researchers who have responsibly disclosed a qualifying issue:

*No entries yet.*

---

*This policy may change as the project grows — notably when the live
network launches, when the bridge is built, and when an audit is
completed. The version in the repository is the current one.*
