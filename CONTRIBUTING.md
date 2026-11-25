# Contributing to NopBar

Thanks for helping. Bug reports, translations and small focused pull
requests are the most useful. For anything bigger (a new feature, a new
panel, a change in how the dock talks to the shell), please open an issue
first so we can agree on the shape before you spend time on it.

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
Security problems: see [SECURITY.md](SECURITY.md), not public issues.

## Build

Requirements: Windows 10 22H2 (19045) or Windows 11, the Rust stable
toolchain for MSVC (`rustup default stable-x86_64-pc-windows-msvc`) and the
Windows SDK (for `rc.exe`, which embeds the icon and version resource;
installed with the Visual Studio "Desktop development with C++" workload or
`winget install Microsoft.WindowsSDK.10.0.26100`).

```
cargo build -p dock -j 4
cargo test -p dock -j 4
```

The app is `target\debug\nopbar.exe` (`--release` for the optimized build).
`-j 4` keeps the build from saturating every core; use what suits your PC.

The whole build must stay at **zero warnings** and every test must pass.
CI (`.github/workflows/ci.yml`) checks both on each pull request.

### Running a dev build safely

NopBar hides the Windows taskbar while it runs and gives it back when it
quits. If a dev build crashes or you kill it, the guardian process restores
the taskbar; if the taskbar is still missing, run:

```
target\debug\nopbar.exe --restore-taskbar
```

Logs go to `%APPDATA%\NopBar\dock.log`. Settings live in
`%APPDATA%\NopBar\config.json`.

## Layout

| Path | What |
|---|---|
| `dock/` | The app (`nopbar.exe`): dock, top bar, All apps, search, settings window |
| `dock/locales/` | UI strings, one JSON catalog per language (42) |
| `spikes/` | Small experiments kept for reference; not shipped |
| `packaging/` | Inno Setup installer, winget and Scoop templates |
| `scripts/` | `package.ps1` (zip / installer), `notices.ps1` (third-party notices), dev helpers |

## Rules for changes

- **Every UI string goes through i18n.** Add each new key to **all 42
  catalogs** in `dock/locales/` (English in `en.json`, real translations in
  the others; keep `{placeholders}` identical). The test
  `every_catalog_parses_and_has_every_english_key` fails otherwise.
  Removing a key means removing it from all 42.
- **Dock window messages and timers**: a new message or timer id goes into
  `dock_window_messages_are_distinct` / `dock_window_timers_are_distinct`
  in `dock/src/dock/mod.rs`.
- **Never leave the user without a taskbar.** Anything that hides, moves or
  re-registers shell windows must undo itself on quit, crash and session
  end.
- Network use includes update checks and downloads, HTTPS requests to the
  ICS feeds users subscribe to, and optional AI sign-in and chat. The AI
  integrations can also use provider tools for chat, installation and
  updates. Explain any new network calls in the PR. No telemetry.
- **Dependencies**: prefer the `windows` crate and the standard library. A
  new crate needs a reason in the PR. After any `Cargo.lock` change run
  `.\scripts\notices.ps1` and commit the updated `THIRD_PARTY_NOTICES.md`.
- Keep comments short and about *why*. Match the surrounding style;
  `cargo fmt` is not enforced on the whole tree, so format only what you
  touch.

## Pull requests

1. Fork, branch from the default branch, keep the PR to one topic.
2. `cargo build -p dock` with no warnings, `cargo test -p dock` green.
3. Describe what changed, why, and how you checked it (Windows version,
   one or several monitors, DPI, light/dark).
4. Screenshots or a short clip for anything visible.

### Review

Every change is reviewed before it is merged, including the maintainer's
own: larger changes get a written review (format
`[severity] file:line | problem | repro | suggestion`), findings are fixed
or answered, then the branch is merged. Expect questions about edge cases:
multiple monitors, DPI changes, Explorer restarts, sleep/resume, and what
happens if NopBar is killed halfway.

## Translations

Fixes to existing translations are welcome as small PRs that touch only
`dock/locales/<lang>.json`. Keep strings short (they appear in menus and
narrow panels) and keep every `{placeholder}`.

## License

By contributing you license your contribution under [GPL-3.0-only](LICENSE)
with the [section 7 additional permission](assets/LICENSE) for NopBar design assets.
Certify the [Developer Certificate of Origin](https://developercertificate.org/)
by signing off each commit with `git commit -s` (adds `Signed-off-by`).
