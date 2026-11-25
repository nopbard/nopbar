## What and why

<!-- What does this change, and why? Link the issue: "Fixes #123". -->

## How I checked it

<!-- Windows version, monitors / DPI, light/dark, what you clicked. Screenshots or a clip for anything visible. -->

## Checklist

- [ ] `cargo build -p dock` with zero warnings
- [ ] `cargo test -p dock` passes
- [ ] New or changed UI strings are in **all 42** `dock/locales/*.json` catalogs (placeholders kept)
- [ ] New dock window messages/timers are added to the distinctness tests in `dock/src/dock/mod.rs`
- [ ] The taskbar still comes back on quit and after a crash
- [ ] `Cargo.lock` changed → ran `scripts/notices.ps1` and committed `THIRD_PARTY_NOTICES.md`
- [ ] No new network calls, telemetry or reserved assets (name, logo, icon, photos)
