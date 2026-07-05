# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

This crate is **Windows-only** (winapi, DPAPI, WebView2, MSVC toolchain). It does not compile on Linux/macOS — in a non-Windows environment, verify changes by review only.

```powershell
# Debug build
cargo build

# Release build (static CRT, no MSVC Redist needed)
cargo build --release --target x86_64-pc-windows-msvc

# Unit tests (state secret round-trip, DPAPI)
cargo test

# Run directly
.\target\release\claudtray.exe

# Debug snapshot (writes JSON to %TEMP%\claudtray_snapshot.json and exits, no UI)
$env:CLAUDTRAY_DUMP = "1"; .\target\release\claudtray.exe

# Build installer (requires Inno Setup 6)
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer.iss
```

Unit tests are sparse (`state.rs`, `dpapi.rs`); most verification is done by running the app and inspecting the debug snapshot.

## Architecture

The app is a single-binary Windows system tray application with a WebView2 popover dashboard.

**Event model** (`main.rs`): A `tao` event loop (`ControlFlow::Wait` — never poll) drives everything on the UI thread. Background work is dispatched to OS threads via `std::thread::spawn` and results are sent back through `EventLoopProxy<UserEvent>`. Tokio is only used for the background timer ticker, which adapts its cadence: 5 s while the dashboard is open, 60 s while hidden.

**Data flow**: `QuotaMonitor::refresh()` (`monitor.rs`) runs every provider in parallel on scoped threads (borrowing `&AppState` so credentials aren't cloned per thread), collects `ProviderSnapshot`s, and assembles a `Snapshot`. Failed providers keep showing their last good value for `STALE_TTL` (5 minutes) to avoid flickering. A history point is recorded every 5 minutes (max 288 points = 24 h, persisted) and the last 48 points feed the dashboard sparklines. The snapshot is sent to the UI as `UserEvent::Snapshot` and pushed to the WebView via `webview.evaluate_script("window.updateData(...)")`.

**Dashboard** (`window.rs`): A frameless, always-on-top `tao` window with Windows 11 Mica backdrop, hosting a `wry` WebView2. The HTML/CSS/JS in `src/ui/` are embedded at compile time with `include_str!` — no separate build step for the frontend. A per-load CSP nonce allows only the embedded inline `<style>`/`<script>`. JS→Rust communication uses `window.__WRY_IPC_POST__` with JSON messages (`IpcMessage` enum, parsed in `parse_ipc`); Rust→JS uses `evaluate_script`. Adding an IPC message means touching `dashboard.js`, the `IpcMessage` enum + `parse_ipc` in `window.rs`, and the match in `main.rs`.

**Provider trait** (`providers/mod.rs`): Each provider implements `Provider::collect(&AppState) -> ProviderSnapshot`. Adding a provider means implementing the trait and registering it in `providers::all()` (display order). Current providers:

- `claude` — Anthropic OAuth usage API, token from `~/.claude/.credentials.json` (or `CLAUDE_CODE_OAUTH_TOKEN`)
- `antigravity` — probes the running language-server process (CSRF token from its command line, local Connect-RPC over self-signed localhost HTTPS)
- `codex` — parses local `~/.codex/sessions/**/*.jsonl` rate-limit snapshots, no network
- `copilot` — GitHub `/copilot_internal/user` endpoint, token supplied via dashboard Settings
- `openrouter` / `gemini` — API-key based, keys supplied via dashboard Settings
- `ollama` — local REST API on `127.0.0.1:11434`, lists installed/loaded models

**HTTP** (`providers/http.rs`): All providers use the shared `agent()` — 5 s global timeout, 1 MiB response body cap (`MAX_BODY_BYTES`), optional global proxy (validated, runtime-changeable). `agent(true)` disables TLS verification and exists only for Antigravity's self-signed localhost endpoint.

**Persisted state** (`state.rs`): Stored at `%APPDATA%\ClaudTray\state.json` — theme, observed peak usage, credentials, proxy, 24 h usage history, and last snapshot for instant startup display. Credential fields (`copilot_token`, `openrouter_key`, `gemini_key`, `http_proxy`) are encrypted with Windows DPAPI on disk (`dpapi.rs`, the `secret` serde module) and zeroized in memory on replace/drop. Never serialize a secret as plaintext; legacy plaintext values are still read and re-encrypted on next save.

**Status thresholds** (`model.rs`): >50% = Healthy (green), 20–49% = Warning (yellow), 1–19% = Critical (red), 0% or no data = Depleted (gray). The tray icon shows the worst status across all active providers (`Status::rank` — Critical ranks worse than Depleted). A balloon notification (`notification.rs`) fires when any window transitions into Critical/Depleted, with a 5-minute cooldown.

## Key Details

- `.cargo/config.toml` sets `target-feature=+crt-static` for the MSVC target — the release binary has no runtime dependencies.
- The UI has Portuguese strings (tray menu labels, error notes, notification titles) — keep that consistent.
- The dashboard is positioned bottom-right above the taskbar (`window.rs:position_bottom_right`). The 48px taskbar height is a fixed approximation.
- Left-click on the tray icon toggles the dashboard popover; right-click shows the context menu, rendered natively by `tray-icon` (`with_menu` + `with_menu_on_left_click(false)`). Tray and menu events are forwarded into the event loop via `EventLoopProxy` (`TrayIconEvent`/`MenuEvent::set_event_handler`) so they're handled immediately and exactly once — do not poll the crate's global `receiver()` channels.
- External URLs opened from the dashboard go through a whitelist (`main.rs:open_url`) — never pass a raw URL from JS to the shell.
- Releases are triggered by pushing a `v*` tag; the CI workflow (`.github/workflows/release.yml`) patches the version in `installer.iss`, builds the installer plus a portable exe, optionally signs via SignPath (repo variable `SIGNPATH_ENABLED`), and prints both SHA256 hashes in the release notes.
- After each release, update `packaging/winget/` (Setup hash → winget-pkgs PR) and `packaging/scoop/claudtray.json` (portable hash → Scoop bucket repo) with the new version and SHA256 from the release notes.
