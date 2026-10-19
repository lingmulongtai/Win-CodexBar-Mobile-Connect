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
