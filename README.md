# ClaudTray

> Windows & Linux port of [ClaudeBar](https://github.com/tddworks/ClaudeBar) — a system tray app that monitors your AI assistant usage quotas in real time.

[![Release](https://img.shields.io/github/v/release/Victor-Magne/claudtray)](https://github.com/Victor-Magne/claudtray/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Platform: Windows | Linux](https://img.shields.io/badge/platform-Windows%2010%2B%20%7C%20Linux-blue)](https://github.com/Victor-Magne/claudtray/releases/latest)

ClaudTray lives in the system tray and gives you an at-a-glance coloured indicator of your remaining quota for Claude Code and other AI tools — no browser, no manual checking. Runs natively on Windows 10/11 and on Linux (Wayland & X11).

---

## What it monitors

| Provider | Data source | Windows tracked |
|---|---|---|
| **Claude** (claude.ai / Claude Code) | Anthropic OAuth API (`~/.claude/.credentials.json`) | Session (5 h), Weekly (7 d), Opus (7 d) |
| **GitHub Copilot** | Local rate-limit snapshots / API token | Monthly tokens |
| **Codex** | Local rate-limit snapshots | Monthly tokens |
| **Antigravity** | Local rate-limit snapshots | Monthly tokens |
| **OpenRouter** | API key (optional) | Remaining credits |
| **Gemini** (Google AI Studio) | API key (optional) | Available models |
| **Ollama** | Local API | Installed / loaded models |

Any provider can be hidden from the dashboard in Settings — hidden providers are not polled and don't affect the tray icon.

## Features

- **Colour-coded tray icon** — green (>50%), yellow (20–50%), red (<20%), grey (depleted / no data)
- **Tooltip** shows session and weekly percentages at a glance
- **Popover dashboard** — click the tray icon for a detailed panel with per-provider cards and reset countdowns
- **Choose your providers** — toggle which providers appear on the dashboard from Settings
- **Auto-refresh** — every 60 s in the background, every 5 s while the dashboard is open
- **Dark / Light / System theme** — follows the OS theme, switchable from the dashboard
- **Single instance** — a second launch exits immediately instead of duplicating the tray icon
- **Windows**: no dependencies — static CRT; WebView2 is bundled or auto-installed (Windows 11 always has it); per-user install, no admin rights required
- **Linux**: native Wayland support — the dashboard is a layer-shell popout (Hyprland, niri, KDE, GNOME…), the tray is a StatusNotifierItem, notifications go over D-Bus; falls back to a regular window on X11

---

## Installation

### Option 1 — winget (Windows Package Manager)

```powershell
winget install VictorMagne.ClaudTray
```

> The package is submitted to the [winget-pkgs](https://github.com/microsoft/winget-pkgs) community repository. Approval may take a few days after each release.

### Option 2 — Scoop

```powershell
scoop bucket add victor-magne https://github.com/Victor-Magne/scoop-bucket
scoop install claudtray
```

### Option 3 — Installer (direct download)

Download `ClaudTray_Setup_<version>.exe` from the [Releases page](https://github.com/Victor-Magne/claudtray/releases) and run it. The installer:

- Does **not** require administrator rights (installs to `%LocalAppData%\ClaudTray`)
- Optionally adds ClaudTray to Windows startup
- Automatically installs the WebView2 Runtime if it is missing (Windows 10 only — Windows 11 ships with it)

### Option 4 — Arch Linux (AUR)

```bash
yay -S claudtray   # or: paru -S claudtray
```

Runtime dependencies: `gtk3`, `webkit2gtk-4.1`, `gtk-layer-shell` (pulled in automatically). The tray icon needs a StatusNotifier host — KDE and most bars (Waybar, DankMaterialShell, …) have one; on GNOME install the AppIndicator extension.

### Option 5 — Build from source

See [Building from source](#building-from-source) below.

---

## How it works

### Claude usage

ClaudTray reads the OAuth access token stored by Claude Code at:

```
%USERPROFILE%\.claude\.credentials.json
```

It then calls Anthropic's usage endpoint and displays the `utilization` percentage for each rolling window. No credentials are stored or transmitted anywhere other than Anthropic's own API.

You can also set the token via environment variable (useful for testing):

```powershell
$env:CLAUDE_CODE_OAUTH_TOKEN = "sk-ant-..."
```

### Other providers

Copilot, Codex, and Antigravity usage is read from local rate-limit snapshot files that each tool writes to disk. ClaudTray inspects running processes and known file paths to find active sessions.

### Tray icon colours

| Colour | Meaning |
|---|---|
| Green | More than 50% remaining |
| Yellow | 20–50% remaining |
| Red | Less than 20% remaining |
| Grey | Depleted or no data found |

The icon shows the **worst** status across all active providers.

---

## Building from source

### Windows

Prerequisites: [Rust](https://rustup.rs/) (stable, MSVC toolchain), Windows 10/11, and [Inno Setup 6](https://jrsoftware.org/isdl.php) (only to build the installer).

```powershell
git clone https://github.com/Victor-Magne/claudtray.git
cd claudtray

# Build the release binary (static CRT, no MSVC Redist needed)
cargo build --release

# Run directly
.\target\release\claudtray.exe

# Build the installer (requires Inno Setup 6)
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer.iss
# Output: installer_output\ClaudTray_Setup_<version>.exe
```

### Linux (Arch)

```bash
sudo pacman -S --needed gtk3 webkit2gtk-4.1 gtk-layer-shell
git clone https://github.com/Victor-Magne/claudtray.git
cd claudtray
cargo build --release
./target/release/claudtray
```

On Debian/Ubuntu the equivalent packages are `libgtk-3-dev`, `libwebkit2gtk-4.1-dev` and `libgtk-layer-shell-dev`.

### Debug helper

Set `CLAUDTRAY_DUMP=1` to write a JSON snapshot to the temp dir (`%TEMP%\claudtray_snapshot.json` on Windows, `/tmp/claudtray_snapshot.json` on Linux) and exit without showing any UI — handy for checking what the app sees:

```powershell
$env:CLAUDTRAY_DUMP = "1"; .\target\release\claudtray.exe   # Windows
CLAUDTRAY_DUMP=1 ./target/release/claudtray                  # Linux
```

---

## Project structure

```
src/
  main.rs          — Event loop, tray wiring, single-instance lock
  model.rs         — Snapshot / ProviderSnapshot / WindowUsage types
  monitor.rs       — QuotaMonitor: orchestrates all providers
  state.rs         — Persisted app state (theme, tokens, hidden providers)
  secret.rs        — Credential encryption (DPAPI on Windows, ChaCha20 on Linux)
  renderer.rs      — Dynamic tray icon (coloured ring, RGBA)
  window.rs        — Webview dashboard popover (WebView2 / webkit2gtk, layer-shell)
  tray_linux.rs    — Linux tray (ksni StatusNotifierItem)
  providers/
    mod.rs         — Provider trait
    claude.rs      — Anthropic OAuth usage API
    copilot.rs     — GitHub Copilot local snapshots
    codex.rs       — Codex local snapshots
    antigravity.rs — Antigravity local snapshots
    openrouter.rs  — OpenRouter credits API
    gemini.rs      — Google AI Studio models API
    ollama.rs      — Ollama local API
    http.rs        — Shared HTTP agent (ureq)
assets/
  claudtray.ico
  MicrosoftEdgeWebview2Setup.exe   — WebView2 bootstrapper (~1.6 MB)
installer.iss      — Inno Setup script
packaging/
  winget/          — winget manifest templates
  scoop/           — Scoop manifest template
  arch/            — PKGBUILD + .desktop + icon (AUR)
```

---

## Package manager maintainers

After each release, update the version and SHA256 in:

- `packaging/winget/VictorMagne.ClaudTray.installer.yaml` → submit PR to [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs)
- `packaging/scoop/claudtray.json` → push to your Scoop bucket repo
- `packaging/arch/PKGBUILD` (`pkgver` + `sha256sums` of the tag tarball) → regenerate `.SRCINFO` (`makepkg --printsrcinfo > .SRCINFO`) and push to the AUR

The SHA256 of the Windows installers is printed in the GitHub release notes.

---

## Contributing

Pull requests are welcome. For significant changes, open an issue first.

This is a Windows & Linux port of the original macOS [ClaudeBar](https://github.com/tddworks/ClaudeBar) by [@tddworks](https://github.com/tddworks). The provider logic, colour thresholds, and dashboard design follow the original as closely as possible.

---

## Credits

- Original macOS app: [tddworks/ClaudeBar](https://github.com/tddworks/ClaudeBar)
- Built with [tao](https://github.com/tauri-apps/tao), [wry](https://github.com/tauri-apps/wry), [tray-icon](https://github.com/tauri-apps/tray-icon) (Windows), [ksni](https://github.com/iovxw/ksni) (Linux), and [ureq](https://github.com/algesten/ureq)

---

## License

MIT — see [LICENSE](LICENSE).
