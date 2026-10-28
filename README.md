# Win-CodexBar + Mobile Connect

Windows AI usage monitoring with encrypted sharing to **Android, Windows and Mac**. An independently maintained fork of [nesszer/Win-CodexBar](https://github.com/nesszer/Win-CodexBar), originally based on [steipete/CodexBar](https://github.com/steipete/CodexBar).

[日本語](README.ja-JP.md) · [简体中文](README.zh-CN.md) · [繁體中文](README.zh-TW.md)

## Download

**[Download Windows EXE directly](https://github.com/lingmulongtai/Win-CodexBar-Mobile-Connect/releases/latest/download/Win-CodexBar-Mobile-Connect.exe)** · [Stable release and checksums](https://github.com/lingmulongtai/Win-CodexBar-Mobile-Connect/releases/latest) · **[Signed Android APK](https://github.com/lingmulongtai/CodexBar-android/releases/latest)**

Save the EXE in a permanent folder and run it. No ZIP extraction or Node.js installation is required. Windows 10/11 x64 and [Microsoft WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) are required. No separate VC++ Redistributable or WebView2 loader DLL is needed. The Universal CRT is included in Windows 10/11; the WebView2 Runtime itself is separate.

This fork's Windows EXE is **unsigned** and uses no upstream signing certificate or installer. Check `SHA256SUMS`. To update, quit the app and replace the EXE. The app's update screen opens this fork's releases. Android APKs retain the established production signing identity; install over the existing app to preserve connections.

## What this fork adds

v0.71.1 adds persistent Gemini sign-in and durable 7/30-day PC quota history. **Mobile Connect** has its own settings pane, shared PC history, Usage & Spend charts on Android/tablets and opt-in remote PC settings. **Use Android v0.10.0 or later for shared history, spend and PC controls.** [Changes, screenshots and verification](docs/releases/v0.71.1.md)

The following captures use the downloaded, published **v0.71.1 EXE** on Windows. They show WebView2 content captured through CDP with anonymous demo UI data; they are not native frame/tray captures or a month of live measurements.

| 7 days | 30 days |
| --- | --- |
| ![Published v0.71.1 seven-day archive, demo data](docs/images/mobile-connect/windows-v0.71.1-history-7days-ja.jpg) | ![Published v0.71.1 thirty-day archive, demo data](docs/images/mobile-connect/windows-v0.71.1-history-30days-ja.jpg) |

![Mobile Connect pane in the downloaded, published v0.71.0 EXE, using isolated demo settings](docs/images/mobile-connect/windows-v0.71.0-mobile-connect-ja.jpg)

| Feature | Behavior |
| --- | --- |
| Android sharing | Windows collects once; the phone receives sanitized encrypted snapshots. Provider credentials stay on the PC. |
| Tailscale selection | Detect/select the PC's Tailscale IPv4 address. Same tailnet required; no exit node needed. LAN is also supported. |
| Android acquisition modes | Phone collection or Windows collection, with optional per-service phone exceptions. Pairing, choices and separate histories survive switching. |
| PC token/cost totals | Cached local-log totals for today, 30 days and the selected history period, where available, with their own aggregation timestamp. USD costs are estimates. |
| Shared history and spend | The updated PC keeps account/connection-specific history and sends the same saved series and daily reference cost/token data to paired devices. |
| Opt-in PC controls | Android can change only the PC fixed refresh interval and Usage & Spend period after local permission. Concurrent changes return a conflict. |
| Owned distribution | Direct EXE, stable releases, correct support/update links and independent app identity. Existing Mobile Preview settings/pairings are retained. |
| Vivaldi browser import | Detects standard Windows Vivaldi profiles and offers a separate choice in Browser Cookies. Existing Chromium encryption limits apply. |

The original collectors, tray, history, notifications, themes and provider settings remain the foundation. This integration does not start a second collector.

[v0.70.3 Vivaldi changes](docs/releases/v0.70.3.md) · [Browser import setup and encryption limits](docs/COOKIES.md)

![Vivaldi selected in the published v0.70.3 Windows EXE](docs/images/mobile-connect/windows-v0.70.3-vivaldi-ja.jpg)

## Gemini Apps

Current/weekly quotas and reset times from Gemini Apps are separate from Gemini CLI and Antigravity. Android **v0.9.8 or later** displays the same sanitized quotas. v0.71.1 adds a dedicated persistent-browser connection and explicit browser/profile import. Imported desktop/browser cookies can still expire after a few hours. Indefinite authentication is not guaranteed; Google may require sign-in again. [Connection guide and verification status](docs/gemini-apps.md)

The following native Windows captures use the downloaded, published **v0.70.6 EXE** with an isolated demo profile.

![Published v0.70.6 Gemini Apps usage (demo data)](docs/images/mobile-connect/windows-v0.70.6-gemini-usage-ja.jpg)

**v0.71.1** keeps Gemini authentication in a dedicated persistent browser, restores an explicit browser/profile import option including Vivaldi, and adds 7/30-day PC quota history to the selected-provider view. The PC archive survives restarts and supplies the same observations to paired devices. [Release changes and validation](docs/releases/v0.71.1.md)

Dedicated-mode controls captured from the initial October 17 candidate's actual Windows EXE with an anonymous profile, before the browser-import controls were added:

![Native Windows dedicated Gemini connection controls, local v0.71.1 candidate](docs/images/mobile-connect/windows-gemini-browser-development-ja.png)

## Connect another desktop

In **Settings → Mobile Connect → Receive from another PC**, enter a PC name and the sharing PC’s connection code, then select **Save and connect**. Choose **This PC** to return to local usage. Windows↔Windows and Windows↔Mac use the same encrypted protocol; both PCs can share their own data while displaying each other. Received data is never forwarded as locally collected data.

History and spend use the selected PC’s saved observations, reporting period and time zone. Missing project/conversation details remain unavailable. Saved connections are separate from provider credentials and survive app updates. Tailscale works without an exit node.

[Mac app and desktop connection guide](https://github.com/lingmulongtai/CodexBar-Mac-Mobile-Connect) · [All three apps in one GitHub Project](https://github.com/users/lingmulongtai/projects/3)

Downloaded public Windows v0.72.0 EXE, isolated synthetic profiles; the images do not contain account data or connection keys. Mutual receiving, UI source switching, offline retention and restart restoration were checked against the released binary.

![Desktop receiving settings](docs/images/mobile-connect/windows-v0.72.0-peer-settings-ja.jpg)

![Received quota dashboard](docs/images/mobile-connect/windows-v0.72.0-peer-dashboard-ja.jpg)

## Connect

For an optional Discord status showing recent local Codex, Claude Code or Pi/OMP activity, see the [Discord Rich Presence guide](docs/discord-rich-presence.md). It is off by default and does not share conversations, project names or paths.

1. Connect and enable the desired providers on Windows.
2. In the updated app, open **Settings → Mobile Connect** (published v0.70.6 and older: **Advanced → Android sharing**). Select **Tailscale (no exit node)** or **LAN**, then enable sharing. Default TCP port: **43825**.
3. Copy the private pairing code into Android **Connections → Usage source & Windows connection**, then connect. Pairing selects Windows mode.
4. Choose **Collect on this phone** or **Collect on Windows**. Advanced phone exceptions select individual services to keep on their saved phone connection.

Both devices can use the same tailnet with the phone's exit node set to **None**. Select the PC's `100.x.x.x` address; ordinary internet traffic need not pass through home. Tailscale access rules and Windows Firewall must permit the app's port. The app does not change VPN or firewall settings. [Setup and troubleshooting](docs/android-sharing.md)

Windows must remain running. Android polls during dashboard/background refreshes; delivery is not instant push. Offline Windows never silently switches to another phone account. New known Windows services are discovered at refresh, including authentication-required providers. Legacy source selections migrate to phone exceptions; **Use Windows for all services** explicitly clears them.

## Actual screens

The Windows captures are from the downloaded, published **v0.70.2 EXE**, including a completed update check. Native Windows and Android emulator captures use synthetic data. Loopback/host addresses shown here are validation endpoints; use LAN or Tailscale for a physical phone.

![Published v0.70.2 Windows sharing settings](docs/images/mobile-connect/windows-release-v0.70.2-sharing-ja.jpg)

![Published v0.70.2 app information and completed update check](docs/images/mobile-connect/windows-release-v0.70.2-about-ja.jpg)

<p>
  <img src="docs/images/mobile-connect/android-windows-mode.png" width="280" alt="Android Windows acquisition mode" />
  <img src="docs/images/mobile-connect/android-pc-local-usage.png" width="280" alt="PC tokens and estimated costs on Android" />
</p>

## Data and limits

Quota windows, reset times, observation timestamps, optional budgets/balances and known Codex reset credits are shared. Local-log totals may cover multiple accounts and are separate from the current quota account and billed charges. Unknown numbers remain unknown. Quota observations over 35 minutes old are stale; over two hours old are unavailable. Local totals have their own PC calendar date and timestamp.

Pairing uses Windows DPAPI and Android Keystore. HMAC-SHA256 requests have clock/replay checks; responses use AES-256-GCM. Provider secrets, emails, account names, local paths, prompts and transcripts are excluded. Keep pairing codes private; **Revoke paired phones** invalidates old codes.

**Gemini Apps now has its own connection for current/weekly usage and reset times. Gemini CLI remains separate. Requires Android v0.9.8 for mobile display.** [Gemini Apps connection](docs/gemini-apps.md) There is no cloud relay or phone discovery. Physical-phone, live-account, launcher and Samsung Now Bar acceptance are separate from emulator tests. [Validation evidence](docs/mobile-validation.md) · [v0.70.2 changes](docs/releases/v0.70.2.md)

## Development

```powershell
git clone https://github.com/lingmulongtai/Win-CodexBar-Mobile-Connect.git
cd Win-CodexBar-Mobile-Connect
. .\scripts\worktree-env.ps1
pnpm --dir apps/desktop-tauri install --frozen-lockfile
pnpm --dir apps/desktop-tauri run tauri:dev
```

Development requires Rust, Windows build tools and the pinned pnpm version. Run `cargo test --locked --workspace`, `cargo fmt --all -- --check`, frontend `pnpm test`, `pnpm run lint` and `pnpm run build`. Build using `pnpm run tauri:build`; raw Cargo can embed the development URL. [Repository rules](AGENTS.md)

[MIT license](LICENSE). Original collector/UI work belongs to the Win-CodexBar/CodexBar contributors. This fork has its own [issues](https://github.com/lingmulongtai/Win-CodexBar-Mobile-Connect/issues) and releases.
