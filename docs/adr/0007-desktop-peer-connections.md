# Desktop connection upstream review

Checked `nesszer/Win-CodexBar` default branch on 2026-10-20 at `fef5d1102afad43954d20ffa8355b00f8e1d6f7e`. The fork starts at `e38ac2dadcefc43eee8c35ef4d23d33dce39b6a8`.

## Settings preservation

Ported `60b589ec85b1dce780ed1df19317b52efad91901` (#917) before adding saved desktop connections. If an existing settings file cannot be read, decrypted or parsed, its in-memory fallback refuses to overwrite it. Preferences import retains the save block; one-time migrations wait for a readable file; the locale-change event follows a successful save. This preserves this fork's DPAPI/atomic writes and owned settings namespace.

Validation: all 164 matching core settings tests passed on Windows, including the seven upstream unreadable-file regressions. `cargo fmt --all` completed. Full shell/native platform checks are recorded with the desktop feature validation when run.

## Mac and Unix compilation

Ported `9c1f8290df2505e61ca29fea9917ea0ffb909adc` (#909). Muse's file identity now selects Windows and Unix implementations correctly instead of importing Windows APIs unconditionally. The same port scopes Windows-only imports and fixtures to their platform without removing portable test coverage.

Validation on Windows: the full core suite passed, **4,860 passed / 2 ignored / 0 failures**. Formatting passed. This establishes the Windows regression baseline; native Mac compilation and runtime verification remain pending.

## Scope of the remaining upstream updates

The new upstream `235ba9ecc679764d6b35fbc4e8933774ec7a511d` reorganizes 54 shell files, including provider/cache integration points customized for Mobile Connect and Gemini session renewal. This feature uses narrow ports of required fixes rather than replacing those integration points. Provider-specific updates unrelated to desktop transport remain outside this change; no upstream repository is modified. The existing Android protocol, account isolation, saved connections, history and release ownership must remain intact.

## Encrypted desktop receiver

The shared core can read the existing v1 sharing protocol using numeric LAN/Tailscale addresses. Requests and responses authenticate the pairing, nonce and clock; reads have a total deadline and a size bound. Received provider/history/spend fields follow the Android validation limits. This client does not write the local collector cache or archive, so adding a receiver cannot create a forwarding loop.

Validation: all 21 mobile-bridge tests passed, including two concurrent receivers, revoked/wrong pairing, tampered envelopes, bounded reads and invalid provider/history/spend fields. Core Clippy with tests and `-D warnings` passed; formatting passed.

## Desktop display source

Desktop receiving has a separate protected connection store and generation-checked in-memory snapshot. Switching or deleting a source discards in-flight results. Local collectors, sharing and archive writers continue to use only local data, even while the desktop displays a peer. Remote history checks the connection generation and opaque account scope; cost queries use only transmitted aggregates and retain the sender's period/time zone and unknown values. Provider credentials and local account actions are never used as receiver fallbacks.

Native tray updates resolve their current source inside a UI-thread task. Source-selection commands run synchronously on that thread, so queued background refreshes cannot publish captured values from a previous PC. No receiver lock is held while calling native tray APIs.

Validation on Windows: shell check and all-target Clippy with `-D warnings` passed. Four focused tests cover delayed publication after switching/disconnecting, retaining last good data with offline status, private status payloads, missing quotas and remote spend/local-usage projection. Native Mac CI and desktop UI acceptance are recorded separately when complete.

## Protected storage on Mac

Mac uses the existing keyring dependency's native Apple backend. Secret-bearing files use AES-256-GCM with a random nonce and authenticated format header; the 32-byte master key stays in the login Keychain. A process mutex and file lock serialize initial key creation across CLI/desktop processes. A durable initialization marker prevents replacing a lost key. Keychain errors never fall back to plaintext, and reads of existing ciphertext require no config-directory writes. Windows DPAPI behavior and its storage namespace are unchanged; Mac has its own stable namespace.

Validation on Windows: all 11 secure-file tests and core all-target Clippy passed. Unit fixtures use an in-process test key on Mac; the normal-build `secure_file_probe` explicitly exercises real Keychain persistence in native CI, including concurrent first use and reading while config storage is read-only. Native execution remains pending until that CI runs.

## Native Mac test isolation

Native run [39325148398](https://github.com/lingmulongtai/CodexBar-Mac-Mobile-Connect/actions/runs/39325148398) passed real Keychain concurrency/read-only probes and 4,810 core tests (2 ignored) on both architectures. It was cancelled after the shell's account-persistence test stopped making progress. That test uses the core as a normal dependency, so the core's `cfg(test)` key fixture did not apply and it reached the same real Keychain item created by the separate probe executable. Four further shell failures exposed Windows-only watcher/path assumptions.

The Mac shell now activates `test-secure-storage` only through its dev-dependency. Its process-random key never touches Keychain. Non-debug builds reject that feature at compile time, and the native probe rejects it at runtime. Cargo's Mac dependency graph confirms the feature is absent from normal/build edges and present only with dev edges. Released bundles still compile and exercise the real backend. Windows-specific fixtures are gated; a non-Windows test verifies that unavailable auto-resume does not create a watcher. Native CI separates compilation from test execution, each with a bounded timeout.

## Native Windows receiver verification

At source `5aac54214af09568a1ff720838812e744b5a99a2`, the Windows workspace passed 4,872 core tests (2 ignored), 1 CLI test and 788 shell tests. Frontend passed 1,357/1,357 tests, lint and production build; after the native layout adjustment its 65 affected tests and build passed again. Workspace all-target Clippy with `-D warnings` passed before that frontend-only adjustment. All 24 locale tests passed, including completeness and placeholder parity for every supported language.

Two newly built desktop processes ran with separate settings and WebView profiles, seeded Codex/Claude fixtures, loopback-only sharing ports and no provider credentials. Actual UI code entry saved a source and cleared its masked code field. Both directions continued showing the opposite PC's own fixture (Codex 18% / Claude 37%) without relaying received data. Local enabled-provider lists were unchanged. An offline sender retained its last value with offline status; the UI source selector returned to the receiver's local data. Replacing/restarting both proof binaries retained saved receiver selection and sharing configuration. The remove control stayed enabled offline; removal itself is covered by the connection-store and UI tests, not a native delete action.

Real-pixel Windows screenshots were captured through the available Computer Use (Sky) runtime; the isolated app's local CDP endpoint controlled and asserted its WebView. No CuaDriver-specific DWM/tray-menu proof is claimed. Screenshots show synthetic data and development binaries; published-artifact confirmation is recorded separately when available. Proof processes were stopped afterward; the installed resident and Gemini profile were not changed. Physical cross-PC and Mac/Tailscale acceptance remain separate from this single-host test.
