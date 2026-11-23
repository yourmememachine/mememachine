#!/usr/bin/env python3
"""Generate Machine's static status page (status/index.html).

The page is a SNAPSHOT, not a monitor. Every number on it is
parsed out of the repository's own records at generation time:

  - test totals, last verified commit, genesis hash, and the
    local validator's stall, from STATUS.md;
  - the last bridge drill result, from bridge/drills/last-drill.md;
  - current SBF artifact sizes and hashes, from the records in
    bridge/VERIFY.md (its "Current state" summary, which
    supersedes the dated log and is newest-first),
    programs/VERIFY.md, and programs/PQ-VERIFY.md (chronological
    addenda — the last complete record for each binary wins).
    Hashes are reproduced exactly as recorded, in full or in the
    logs' truncated form.

If any value cannot be found, this script exits nonzero and
writes nothing — a status page that guesses is worse than none.
It never contacts a chain, an RPC, or the network: the local
validator's state is whatever STATUS.md records, and the page
says so, with the date that state was observed.

Usage: python3 status/generate.py
"""

import datetime
import html
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "status" / "index.html"


def fail(msg: str) -> "SystemExit":
    print(f"generate.py: REFUSING to generate: {msg}", file=sys.stderr)
    raise SystemExit(1)


def read(rel: str) -> str:
    path = ROOT / rel
    if not path.exists():
        fail(f"missing source file {rel}")
    return path.read_text(encoding="utf-8")


def need(pattern: str, text: str, what: str, flags: int = 0) -> "re.Match[str]":
    m = re.search(pattern, text, flags)
    if m is None:
        fail(f"could not find {what}")
    return m


def sbf_records(text: str, binary: str):
    """All complete (size, hash) records for `binary`, in file
    order. A record is the binary's name followed within a short
    window by a size and a hash. Hashes appear in the logs either
    in full (`<64 hex>`) or truncated (`<8 hex>…`) — both are the
    repository's own recorded form and are reproduced as-is."""
    found = []
    for m in re.finditer(re.escape(binary), text):
        window = text[m.start():m.start() + 220]
        hash_m = re.search(r"`([0-9a-f]{64})`| `([0-9a-f]{8})…`", window)
        size_m = re.search(r"\|\s*([\d,]+)\s*\||\(([\d,]+) B", window)
        if hash_m and size_m:
            size = (size_m.group(1) or size_m.group(2)).replace(",", "")
            hash_str = hash_m.group(1) or (hash_m.group(2) + "…")
            found.append((int(size), hash_str))
    if not found:
        fail(f"no complete SBF record for {binary}")
    return found


def current_section(text: str) -> str:
    """bridge/VERIFY.md leads with a 'Current state' summary that
    supersedes the dated log below it, newest entry first."""
    start = text.find("## Current state")
    if start < 0:
        fail("bridge/VERIFY.md has no Current state section")
    end = text.find("\n## ", start + 1)
    return text[start:] if end < 0 else text[start:end]


def main() -> None:
    status = read("STATUS.md")
    drill = read("bridge/drills/last-drill.md")
    bridge_verify = read("bridge/VERIFY.md")
    programs_verify = read("programs/VERIFY.md")
    pq_verify = read("programs/PQ-VERIFY.md")

    # --- Verification totals and pin, from the STATUS.md header ---
    # The header is prose and line-wraps freely; flatten whitespace
    # so the patterns below match the words, not the wrapping.
    header = re.sub(r"\s+", " ", status)
    totals = need(
        r"bridge workspace (\d+) passed, LaunchGuard (\d+) passed, PQ vault "
        r"(\d+) passed, registry export (\d+) passed, RugCall (\d+) passed, "
        r"fork suite (\d+) OK, resolver JS suite (\d+) passed, app JS suite "
        r"(\d+) passed — total (\d+), 0 failed",
        header,
        "test totals in STATUS.md header",
    )
    (
        bridge_t,
        launchguard_t,
        pq_t,
        registry_t,
        rugcall_t,
        fork_t,
        resolver_t,
        app_t,
        total_t,
    ) = (int(g) for g in totals.groups())
    # The suites and the total are parsed independently;
    # refuse to publish a page whose own numbers do not add up.
    suite_sum = (
        bridge_t + launchguard_t + pq_t + registry_t + rugcall_t
        + fork_t + resolver_t + app_t
    )
    if suite_sum != total_t:
        fail(
            "STATUS.md suite counts "
            f"({bridge_t}+{launchguard_t}+{pq_t}+{registry_t}+{rugcall_t}"
            f"+{fork_t}+{resolver_t}+{app_t}) "
            f"do not sum to the stated total ({total_t})"
        )
    pin = need(
        r"run on (\d{4}-\d{2}-\d{2}) against commit `([0-9a-f]+)`",
        header,
        "verification date / commit pin in STATUS.md header",
    )
    verify_date, verify_commit = pin.group(1), pin.group(2)

    genesis = need(
        r"Genesis hash:\s+`([0-9a-zA-Z]+)`", status, "genesis hash in STATUS.md"
    ).group(1)

    # --- The stall, stated exactly as STATUS.md states it ---
    stall = need(
        r"local validator is STALLED at slot\s+([\d,]+)",
        status,
        "stalled slot in STATUS.md",
    )
    stalled_slot = stall.group(1)
    stall_since = need(
        r"since ~([\d:]+ [A-Z]+ \d{4}-\d{2}-\d{2})",
        status,
        "stall start time in STATUS.md",
    ).group(1)

    # --- Last bridge drill ---
    drill_name = need(r"- drill: `([^`]+)`", drill, "drill name").group(1)
    drill_unix = int(need(r"- unix time: (\d+)", drill, "drill unix time").group(1))
    drill_when = datetime.datetime.fromtimestamp(
        drill_unix, tz=datetime.timezone.utc
    ).strftime("%Y-%m-%d %H:%M UTC")
    drill_ok = need(
        r"- crossings succeeded: (\d+) \((\d+) in, (\d+) back\)", drill, "drill crossings"
    )
    drill_failed = need(
        r"- crossings failed unexpectedly: (\d+)", drill, "drill failures"
    ).group(1)
    drill_final = need(r"- final state: (.+)", drill, "drill final state").group(1)

    # --- SBF artifacts (local builds; nothing here is deployed) ---
    # bridge/VERIFY.md: the Current state summary supersedes the
    # log and is newest-first, so the first record there is the
    # current one. The programs logs are chronological addenda, so
    # the last record is current.
    bridge_current = current_section(bridge_verify)
    artifacts = [
        ("solana_lock.so", sbf_records(bridge_current, "solana_lock.so")[0]),
        ("machine_bridge.so", sbf_records(bridge_current, "machine_bridge.so")[0]),
        ("launch_guard.so", sbf_records(programs_verify, "launch_guard.so")[-1]),
        ("pq_vault.so", sbf_records(pq_verify, "pq_vault.so")[-1]),
    ]
    artifact_rows = "\n".join(
        f"      <tr><td><code>{html.escape(name)}</code></td>"
        f"<td>{size:,} B</td><td><code>{html.escape(sha)}</code></td></tr>"
        for name, (size, sha) in artifacts
    )

    today = datetime.date.today().isoformat()

    page = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Machine — status snapshot</title>
<style>
  body {{ font-family: system-ui, sans-serif; margin: 2rem auto; max-width: 52rem;
         padding: 0 1rem; line-height: 1.5; color: #111; }}
  .stalled {{ border: 2px solid #b00020; padding: 1rem; border-radius: 6px; }}
  .stalled h2 {{ color: #b00020; margin-top: 0; }}
  section {{ border: 1px solid #ccc; padding: 1rem; border-radius: 6px; margin: 1rem 0; }}
  table {{ border-collapse: collapse; width: 100%; }}
  td, th {{ border-bottom: 1px solid #ddd; padding: 0.3rem 0.5rem; text-align: left;
           vertical-align: top; }}
  code {{ font-size: 0.85em; overflow-wrap: anywhere; }}
  .small {{ color: #555; font-size: 0.9em; }}
</style>
</head>
<body>
<h1>Machine — status snapshot</h1>
<p class="small">Static snapshot generated {today} by
<code>status/generate.py</code> from this repository's own records
(<code>STATUS.md</code>, <code>bridge/drills/last-drill.md</code>, the
VERIFY logs). It is not a live monitor: nothing here polls a chain,
and every figure is exactly as current as the file it was read from.
Regenerate with <code>python3 status/generate.py</code>.</p>

<section class="stalled">
  <h2>Chain: localnet only — validator STALLED</h2>
  <p>Machine runs as a <strong>single local validator</strong> on one
  machine. It is <strong>not a public chain</strong>: there is no public
  RPC, no second validator, and nothing is deployed anywhere.</p>
  <p>The local validator is <strong>stalled at slot {stalled_slot}</strong>
  (processed / confirmed / finalized), unchanged since ~{stall_since}.
  Its RPC health check still answers, but the slot does not advance;
  whether to restart fresh from the shipped genesis or diagnose the
  stall is an open decision, recorded in <code>STATUS.md</code>. Any
  page or post claiming this chain is live and advancing is describing
  some other day — the last day that was true was 2026-11-14.</p>
  <p>Genesis hash: <code>{html.escape(genesis)}</code></p>
</section>

<section>
  <h2>Verification (local test suites)</h2>
  <p>Full local <code>make verify</code> run on {verify_date} against
  commit <code>{verify_commit}</code>: <strong>{total_t} passed, 0 failed</strong>.</p>
  <table>
    <tr><th>Suite</th><th>Passed</th></tr>
    <tr><td>Bridge workspace</td><td>{bridge_t}</td></tr>
    <tr><td>LaunchGuard</td><td>{launchguard_t}</td></tr>
    <tr><td>PQ vault</td><td>{pq_t}</td></tr>
    <tr><td>Registry export</td><td>{registry_t}</td></tr>
    <tr><td>RugCall</td><td>{rugcall_t}</td></tr>
    <tr><td>Fork suite</td><td>{fork_t}</td></tr>
    <tr><td>Resolver JS suite</td><td>{resolver_t}</td></tr>
    <tr><td>App JS suite</td><td>{app_t}</td></tr>
  </table>
  <p class="small">Program-test results prove the programs behave as
  designed locally. They prove nothing about a live deployment, which
  does not exist.</p>
</section>

<section>
  <h2>Last bridge drill (local, program-test)</h2>
  <p><code>{html.escape(drill_name)}</code> — {drill_when}<br>
  Crossings succeeded: {drill_ok.group(1)} ({drill_ok.group(2)} in,
  {drill_ok.group(3)} back); failed unexpectedly: {drill_failed}.<br>
  Final state: {html.escape(drill_final)}</p>
  <p class="small">A local drill in the test harness — no asset has
  ever crossed the bridge, and the bridge is not deployed.</p>
</section>

<section>
  <h2>Program builds (SBF, local only)</h2>
  <table>
    <tr><th>Artifact</th><th>Size</th><th>sha256</th></tr>
{artifact_rows}
  </table>
  <p class="small">Deterministic local builds recorded in
  <code>bridge/VERIFY.md</code>, <code>programs/VERIFY.md</code>, and
  <code>programs/PQ-VERIFY.md</code>. A hash shown with an ellipsis
  is the truncated prefix exactly as the VERIFY log records it,
  not a full sha256. Built is not deployed: no program
  in this repository runs on any live network.</p>
</section>

<p class="small">Source of truth: <code>STATUS.md</code> in this
repository. If this page and STATUS.md ever disagree, STATUS.md is
right and this page is stale — regenerate it.</p>
</body>
</html>
"""
    OUT.write_text(page, encoding="utf-8")
    print(f"wrote {OUT.relative_to(ROOT)} (snapshot date {today})")


if __name__ == "__main__":
    main()
