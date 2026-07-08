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

**Data flow**: `QuotaMonitor::refresh()` (`monitor.rs`) spawns one OS thread per provider in parallel, collects `ProviderSnapshot`s, and assembles a `Snapshot`. Failed providers are held in a 30-second stale cache to avoid flickering. The snapshot is sent to the UI as `UserEvent::Snapshot` and pushed to the WebView via `webview.evaluate_script("window.updateData(...)")`.

**Dashboard** (`window.rs`): A frameless, always-on-top `tao` window with Windows 11 Mica backdrop, hosting a `wry` WebView2. The HTML/CSS/JS in `src/ui/` are embedded at compile time with `include_str!` — no separate build step for the frontend. JS→Rust communication uses `window.__WRY_IPC_POST__` with JSON messages (`IpcMessage` enum); Rust→JS uses `evaluate_script`.

**Provider trait** (`providers/mod.rs`): Each provider implements `Provider::collect(&AppState) -> ProviderSnapshot`. Adding a provider means implementing the trait and registering it in `providers::all()`.

**Persisted state** (`state.rs`): Stored at `%APPDATA%\ClaudTray\state.json` — holds theme, Copilot token, and last snapshot for instant startup display.

**Status thresholds** (`model.rs`): >50% = Healthy (green), 20–49% = Warning (yellow), 1–19% = Critical (red), 0% or no data = Depleted (gray). The tray icon shows the worst status across all active providers.

## Key Details

- `.cargo/config.toml` sets `target-feature=+crt-static` for the MSVC target — the Windows release binary has no runtime dependencies.
- The UI has Portuguese strings (tray menu labels, error notes) — keep that consistent.
- Windows: the dashboard is positioned bottom-right above the taskbar (`window.rs:position_bottom_right`). The 48px taskbar height is a fixed approximation.
- Linux/Wayland: the dashboard is a wlr-layer-shell surface (`window.rs:init_layer_shell`, namespace `claudtray`) anchored top-right below the bar — the compositor positions it; on X11 it falls back to a normal window using `position_bottom_right`. The webview must be built with `build_gtk(window.default_vbox())`, not `build(&window)`.
- Secrets in `state.json` are encrypted by `src/secret.rs`: DPAPI on Windows (`src/dpapi.rs`), ChaCha20-Poly1305 with a 0600 key file on Linux. Linux notifications go through D-Bus (`notify-rust`).
- Left-click on the tray icon toggles the dashboard popover; right-click shows the context menu. Windows uses `tray-icon` (`with_menu` + `with_menu_on_left_click(false)`); tray and menu events are forwarded into the event loop via `EventLoopProxy` (`TrayIconEvent`/`MenuEvent::set_event_handler`) so they're handled immediately and exactly once — do not poll the crate's global `receiver()` channels. Linux uses `ksni` (`src/tray_linux.rs`) because tray-icon's appindicator backend drops SNI `Activate` (left-click) entirely; ksni serves the full StatusNotifierItem + dbusmenu and sends the same `UserEvent`s through the proxy.
- One instance per session: `acquire_instance_lock()` (`main.rs`) holds an advisory lock on `claudtray.lock` in the runtime dir (`$XDG_RUNTIME_DIR`, or temp dir on Windows); a second launch prints a notice and exits.
- Releases are triggered by pushing a `v*` tag; the CI workflow patches the version in `installer.iss` before building.
- After each release, update `packaging/winget/` and `packaging/scoop/claudtray.json` with the new version and SHA256 printed in the release notes.
