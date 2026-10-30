# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

The app is cross-platform: Windows (WebView2/Mica/DPAPI) and Linux (webkit2gtk/layer-shell/ChaCha20). All platform code is `cfg`-gated; a change to shared code must compile on both (`cargo check` natively + `cargo check --target x86_64-pc-windows-gnu` from Linux).

### Windows

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

### Linux (Arch)

```bash
# System deps
sudo pacman -S --needed gtk3 webkit2gtk-4.1 gtk-layer-shell

cargo build --release
./target/release/claudtray

# Debug snapshot (writes JSON to /tmp/claudtray_snapshot.json and exits)
CLAUDTRAY_DUMP=1 ./target/release/claudtray
```

### Feature flags

`--features auto-credentials`: personal/dev builds only — the Claude provider falls back to the token in `~/.claude/.credentials.json` when none was configured. Public builds (CI, winget, scoop) must NOT enable it; without it the fallback code is compiled out entirely (winget moderation requires the user to provide the token explicitly via Settings / `claude setup-token`).

`cargo test` runs the unit tests (secret round-trip, state serialization). Behaviour is verified by running the app and inspecting the debug snapshot.

## Architecture

The app is a single-binary system tray application with a webview popover dashboard (WebView2 on Windows, webkit2gtk on Linux).

**Event model** (`main.rs`): A `tao` event loop drives everything on the UI thread. Background work is dispatched to OS threads via `std::thread::spawn` and results are sent back through `EventLoopProxy<UserEvent>`. Tokio is only used for the background timer ticker.

**Data flow**: `QuotaMonitor::refresh()` (`monitor.rs`) spawns one OS thread per due provider in parallel and assembles a `Snapshot` each time one finishes (the others show their cached value), so the UI never waits for the slowest provider; a provider still running after `PROVIDER_DEADLINE` (8 s) counts as failed for that tick. Providers get one attempt per tick (no sleeping retries); failed providers are held in a stale cache to avoid flickering. `Collector::settle` schedules each provider's next attempt: success => `min_interval`; consecutive failures back off 30 s, 1, 2, 4, 8, 10 min (reset on success; skipped for local providers, `min_interval` < 30 s); a provider reports a server `Retry-After` (429/503, `Failure::Throttled`) via `ProviderSnapshot.retry_in_secs`, which wins if longer (cap 1 h) and which the monitor then refills with the time left for the UI; a 401 waits the normal `min_interval`; a manual refresh bypasses all of it (Claude's sticky 429 guard in `claude.rs` still applies to it). Each snapshot is sent to the UI as `UserEvent::Snapshot` and pushed to the WebView via `webview.evaluate_script("window.updateData(...)")`.

**Dashboard** (`window.rs`): A frameless, always-on-top `tao` window with Windows 11 Mica backdrop, hosting a `wry` WebView2. The HTML/CSS/JS in `src/ui/` are embedded at compile time with `include_str!` — no separate build step for the frontend. JS→Rust communication uses `window.__WRY_IPC_POST__` with JSON messages (`IpcMessage` enum); Rust→JS uses `evaluate_script`.

**Provider trait** (`providers/mod.rs`): Each provider implements `Provider::collect(&AppState) -> ProviderSnapshot`. Adding a provider means implementing the trait and registering it in `providers::all()`.

**Claude usage page** (`providers/claude_usage.rs`): one pass over `~/.claude/projects/**/*.jsonl` feeds the Claude page (today/week/month tokens and cost, tokens per weekday and per model, sessions, messages, first date). Claude Code writes the same message several times (streaming chunks, resumed sessions), so entries are de-duplicated by `message.id:requestId`. Parsed entries are cached per file by (path, mtime, size) in memory and in `~/.cache/ClaudTray/claude_usage.json` (`dirs::cache_dir()`); entries of transcripts Claude Code later deletes are kept on purpose so "Since" accumulates. Bump `CACHE_VERSION` when `Entry` changes meaning (cost is stored per entry). Prices live in `model_prices()` by model id; it is an estimate.

**Pace** (`monitor.rs:pace`): `WindowUsage.window_secs` (set by the provider when the window has a fixed length: Claude 5h / 7d) plus `reset_at` give elapsed share minus used share in percentage points, skipped in a window's first 3%. The dashboard shows it as text and a tick on the ring (`PACE_BAND` in `dashboard.js` is the "on pace" width).

**Signed-out alert**: a provider whose token was rejected (HTTP 401 only) returns `ProviderSnapshot::signed_out`, which sets `auth_failed`. `AlertKind::SignedOut` fires once per provider and again only after a recovery; `AlertState::prune` must keep its key while `auth_failed`, and the monitor's 30 s stale cache never masks it. A short-lived token read by `auto-credentials` never counts as signed out.

**Persisted state** (`state.rs`): Stored at `%APPDATA%\ClaudTray\state.json` — holds theme, Copilot token, and last snapshot for instant startup display.

**Status thresholds** (`model.rs`): >50% = Healthy (green), 20–49% = Warning (yellow), 1–19% = Critical (red), 0% or no data = Depleted (gray). The tray icon shows the worst status across all active providers.

## Key Details

- `.cargo/config.toml` sets `target-feature=+crt-static` for the MSVC target — the Windows release binary has no runtime dependencies.
- **i18n**: the app supports Portuguese and English. `src/i18n.rs` covers Rust-side strings (tray menu, notifications, tooltip) via a `Lang` enum + `Catalog` struct — add a language by adding a `Lang` variant and a `const` catalog. `src/ui/dashboard.js` has its own `STRINGS` dictionary (keyed the same way) for the webview UI, applied to `[data-i18n]`/`[data-i18n-title]`/`[data-i18n-placeholder]` elements in `dashboard.html` via `applyStaticStrings()`. The preference (`AppState.language`: `"system"|"pt"|"en"`) resolves against the OS locale when `"system"`, mirroring the `theme` field's pattern; the resolved value travels to the frontend as `Snapshot.resolved_language`. `installer.iss` is a separate, parallel translation (Inno Setup `[Languages]`/`[CustomMessages]`) — keep it in sync by hand, it does not read from the Rust catalog.
- Windows: the dashboard is positioned bottom-right above the taskbar (`window.rs:position_bottom_right`). The 48px taskbar height is a fixed approximation.
- Linux/Wayland: the dashboard is a wlr-layer-shell surface (`window.rs:init_layer_shell`, namespace `claudtray`) anchored top-right below the bar — the compositor positions it; on X11 it falls back to a normal window using `position_bottom_right`. The webview must be built with `build_gtk(window.default_vbox())`, not `build(&window)`.
- Close on pointer-leave: the 400 ms hysteresis and the Settings/press guards live in `dashboard.js` (`armPointerLeave`), which sends `pointerLeave` only for the final decision. WebView2 feeds it from DOM `mouseover`/`mouseout`; on Linux WebKitGTK never reports the leave on a layer-shell surface, so `window.rs:watch_pointer` forwards GTK enter/leave (`UserEvent::PointerInside` → `window.__setPointerInside`). A popover the pointer never entered stays open (by design).
- Secrets in `state.json` are encrypted by `src/secret.rs`: DPAPI on Windows (`src/dpapi.rs`), ChaCha20-Poly1305 with a 0600 key file on Linux. Linux notifications go through D-Bus (`notify-rust`).
- Left-click on the tray icon toggles the dashboard popover; right-click shows the context menu. Windows uses `tray-icon` (`with_menu` + `with_menu_on_left_click(false)`); tray and menu events are forwarded into the event loop via `EventLoopProxy` (`TrayIconEvent`/`MenuEvent::set_event_handler`) so they're handled immediately and exactly once — do not poll the crate's global `receiver()` channels. Linux uses `ksni` (`src/tray_linux.rs`) because tray-icon's appindicator backend drops SNI `Activate` (left-click) entirely; ksni serves the full StatusNotifierItem + dbusmenu and sends the same `UserEvent`s through the proxy.
- One instance per session: `acquire_instance_lock()` (`main.rs`) holds an advisory lock on `claudtray.lock` in the runtime dir (`$XDG_RUNTIME_DIR`, or temp dir on Windows); a second launch prints a notice and exits.
- Releases are triggered by pushing a `v*` tag; the CI workflow patches the version in `installer.iss` before building.
- After each release, update `packaging/winget/` and `packaging/scoop/claudtray.json` with the new version and SHA256 printed in the release notes.
