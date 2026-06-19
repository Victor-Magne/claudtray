# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```powershell
# Debug build
cargo build

# Release build (static CRT, no MSVC Redist needed)
cargo build --release --target x86_64-pc-windows-msvc

# Run directly
.\target\release\claudtray.exe

# Debug snapshot (writes JSON to %TEMP%\claudtray_snapshot.json and exits)
$env:CLAUDTRAY_DUMP = "1"; .\target\release\claudtray.exe

# Build installer (requires Inno Setup 6)
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer.iss
```

There are no automated tests. Verification is done by running the app and inspecting the debug snapshot.

## Architecture

The app is a single-binary Windows system tray application with a WebView2 popover dashboard.

**Event model** (`main.rs`): A `tao` event loop drives everything on the UI thread. Background work is dispatched to OS threads via `std::thread::spawn` and results are sent back through `EventLoopProxy<UserEvent>`. Tokio is only used for the background timer ticker.

**Data flow**: `QuotaMonitor::refresh()` (`monitor.rs`) spawns one OS thread per provider in parallel, collects `ProviderSnapshot`s, and assembles a `Snapshot`. Failed providers are held in a 30-second stale cache to avoid flickering. The snapshot is sent to the UI as `UserEvent::Snapshot` and pushed to the WebView via `webview.evaluate_script("window.updateData(...)")`.

**Dashboard** (`window.rs`): A frameless, always-on-top `tao` window with Windows 11 Mica backdrop, hosting a `wry` WebView2. The HTML/CSS/JS in `src/ui/` are embedded at compile time with `include_str!` — no separate build step for the frontend. JS→Rust communication uses `window.__WRY_IPC_POST__` with JSON messages (`IpcMessage` enum); Rust→JS uses `evaluate_script`.

**Provider trait** (`providers/mod.rs`): Each provider implements `Provider::collect(&AppState) -> ProviderSnapshot`. Adding a provider means implementing the trait and registering it in `providers::all()`.

**Persisted state** (`state.rs`): Stored at `%APPDATA%\ClaudTray\state.json` — holds theme, Copilot token, and last snapshot for instant startup display.

**Status thresholds** (`model.rs`): >50% = Healthy (green), 20–49% = Warning (yellow), 1–19% = Critical (red), 0% or no data = Depleted (gray). The tray icon shows the worst status across all active providers.

## Key Details

- `.cargo/config.toml` sets `target-feature=+crt-static` for the MSVC target — the release binary has no runtime dependencies.
- The UI has Portuguese strings (tray menu labels, error notes) — keep that consistent.
- The dashboard is positioned bottom-right above the taskbar (`window.rs:position_bottom_right`). The 48px taskbar height is a fixed approximation.
- Single-click on the tray icon shows the context menu; double-click toggles the dashboard popover.
- Releases are triggered by pushing a `v*` tag; the CI workflow patches the version in `installer.iss` before building.
- After each release, update `packaging/winget/` and `packaging/scoop/claudtray.json` with the new version and SHA256 printed in the release notes.
