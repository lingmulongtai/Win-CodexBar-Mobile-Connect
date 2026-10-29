# Local Codex usage and spend

Usage & Spend estimates API list-price cost from local token records. It is not
a subscription bill. Input includes cache reads; reasoning is part of output,
so neither is counted twice. Model and date-specific rates come from the existing
pricing catalog. Unknown models remain unpriced.

## Request pricing

Prices are summed from retained event rows, rather than applying a long-context
rate to an entire day's tokens. For a nonlinear rate, the event must also prove
that its delta is one request through matching `last_token_usage` counters.
A cumulative-only delta that could span several requests remains unpriced.
The verification reads at most 64 KiB at that event's recorded offset.

The parser retains model context and cumulative counter baselines outside the
selected reporting period; only the output records are filtered by date. This
prevents an in-range event from inheriting all earlier cumulative usage or losing
its recorded model.

## Incremental history and source changes

Default caches are scoped under the existing cache directory's `codex-scopes`
folder by configured history roots, bucket time zone and parser generation.
Older caches remain on disk. Switching folders uses a separate cache; switching
back can reuse the earlier scope. A scan fixes its destination before acquiring
the per-cache lock, and results from a changed scope are not published.

Completed files reuse their token rows. Growing files retain parser state and
read the suffix; changing the displayed period does not discard a broader
completed range. Normal native row caches rely on file identity, size and
modification time, like the incremental parser. Legacy content-hashed evidence
continues to validate its recorded prefix when recovering historical pricing.

Bounded scans resume their pending file ranges. A partial tail that stops making
progress pauses until its file metadata or source readability changes, or until
an explicit refresh. Missing files do not prevent other files from progressing.
An unreadable configured source does not become a known zero.

The settings page preserves the previous summary while refreshing and displays
a spinner and an updating label. Incomplete work retries with backoff from 10
seconds up to 2 minutes; completed and paused reports refresh every 5 minutes.
There is no fabricated completion percentage: discovery can add work and the
scanner does not expose a stable overall denominator. Errors remain visible and
the refresh indicator is cleared when a request finishes or fails.

## Approved backup restoration

An explicitly approved subset can be retained in the local cache directory's
`saved-codex-history.json`, with a source root, backup SHA-256, session IDs and
validated source rows. It is separate from live files and never creates fake
transcripts. The app includes it only for the matching available source, after
the live scan completes. A live session with the same ID suppresses its saved
copy. Restored input, cache, output and reasoning counters use the same merging
rules as live data. The summary identifies restored backup sessions and labels
the amount as an API list-price estimate.

Additional independently reconciled history may be stored in a separate
`saved-codex-history-extra.json` with its own backup digest. Duplicate IDs are
excluded across both files. Since deleted source files cannot verify whether a
long-context cumulative delta represents a single request, nonlinear prices for
such rows remain unpriced. Rows with unresolved identity or inconsistent
counters are excluded. No history or account data is committed to this repository.

## Account total and diagnostics

The settings page separately reads the signed-in default Codex CLI account's
profile via `account/usage/read`. The account lifetime token total is a server
value; it must not be combined with the local model or cost estimates. The server
response does not supply a lifetime model-by-model cost breakdown. Missing or
unsupported account data is shown as unavailable. The Usage & Spend diagnostic
panel shows the existing redacted log tail, including fixed stage names and elapsed
times, without recording local paths, token values, or account identifiers.

## Upstream review

On 2026-10-30 the read-only upstream default branch was fetched again at
`1d3c1305bfb643b6ea643d176bdc31c381ba27b1`, unchanged from the prior review.
No new mainline changes were available to incorporate. The separate OpenCode
history integration remains deferred for the fork-specific collector/settings
compatibility reasons recorded in `PORTING.md`. Mobile Connect, Discord settings,
update ownership and saved credentials are preserved.
