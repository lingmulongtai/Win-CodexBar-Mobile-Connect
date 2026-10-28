# Discord Rich Presence

The Windows desktop app can optionally show recent local Codex, Claude Code and Pi/OMP activity in Discord. Open **Settings → Menu → Discord Rich Presence** and turn it on. It is off by default and uses the Discord desktop client; Discord's activity-sharing setting must also permit the status to be displayed.

The activity content contains only the service label and a fixed public icon URL. Discord proxies the icon from the fork's public GitHub repository; the local RPC protocol also includes the application ID, process ID and a request nonce. Project names, paths, prompts, transcripts, account details and usage limits are not sent. The feature scans only this PC, independently of remote session settings and Mobile Connect. It excludes its own child processes and the known Claude usage-probe session. It uses recent session-file timestamps as an approximation, so a completed session may remain visible for a few minutes, and unsupported or unusual installations may not be detected. When several providers are active, the most recently updated eligible session is shown. Disabling the setting or exiting the app closes the local Discord connection. Discord may prioritize another application's activity.

The Discord application ID is `[REMOVED]` and belongs to this fork's Discord application. Discord must be running locally; the app retries when it becomes available. No bot token, OAuth login or extra process is required.

## Upstream review

On 2026-10-28, fetched the read-only `nesszer/Win-CodexBar` default branch at `31b10ce42` against fork main `9ea38f9b6`. The branches diverged by 133 fork and 131 upstream commits. The upstream local-only session work (`7cc8dc4d5`, `429bcfa25`) changes remote discovery and is unnecessary for this feature, which calls the existing local scanner directly; the remote decoding changes are likewise outside its path. The Codex token-event timestamp change `48ce07d57` affects the separate Usage & Spend indexer, not the local session scanner, and is deferred with the remaining broad upstream divergence rather than merging over fork-specific Mobile Connect and release behavior. No upstream changes were incorporated for this feature.

## Validation

The local Windows test run passed 34 agent-session tests, four activity-mapping tests, four desktop IPC/settings tests, all 1,359 frontend tests, the frontend build and locale check. A live Discord desktop IPC handshake accepted the supplied application ID, activity and icon URL, and acknowledged a clear request. This is protocol evidence; it does not establish visual display or end-to-end detection of every Claude installation.

References: [Codex Discord Rich Presence](https://github.com/xt0n1-t3ch/Codex-Discord-Rich-Presence), [claude-rpc](https://github.com/rar-file/claude-rpc), and [Discord RPC documentation](https://docs.discord.com/developers/topics/rpc).
