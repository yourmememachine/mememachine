# Machine status page

`index.html` in this directory is a **static snapshot** of the
project's status, generated from the repository's own records by
`generate.py`:

```sh
python3 status/generate.py
```

It is not a live monitor and it must never look like one. Every
figure on the page is parsed from `STATUS.md`,
`bridge/drills/last-drill.md`, and the VERIFY logs at generation
time; the generator exits without writing anything if a value it
needs is missing, rather than emitting a page with a gap or a
guess. The page states the chain's real status as STATUS.md
records it — currently: localnet only, a single validator,
**stalled at slot 29,100** since 2026-11-14.

Regenerate it whenever the status docs are synced (test totals,
SBF hashes, or the verified-commit pin changed), and commit the
regenerated page with those doc updates. If this page and
`STATUS.md` ever disagree, STATUS.md is right and this page is
stale.
