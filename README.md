# CloudTray

> Windows port of [ClaudeBar](https://github.com/tddworks/ClaudeBar) — a system tray app that monitors your AI assistant usage quotas in real time.

[![Release](https://img.shields.io/github/v/release/Victor-Magne/cloudtray)](https://github.com/Victor-Magne/cloudtray/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%2B-blue)](https://github.com/Victor-Magne/cloudtray/releases/latest)

CloudTray lives in the Windows system tray and gives you an at-a-glance coloured indicator of your remaining quota for Claude Code and other AI tools — no browser, no manual checking.

---

## What it monitors

| Provider | Data source | Windows tracked |
|---|---|---|
| **Claude** (claude.ai / Claude Code) | Anthropic OAuth API (`~/.claude/.credentials.json`) | Session (5 h), Weekly (7 d), Opus (7 d) |
| **GitHub Copilot** | Local rate-limit snapshots | Monthly tokens |
| **Codex** | Local rate-limit snapshots | Monthly tokens |
| **Antigravity** | Local rate-limit snapshots | Monthly tokens |

## Features

- **Colour-coded tray icon** — green (>50%), yellow (20–50%), red (<20%), grey (depleted / no data)
- **Tooltip** shows session and weekly percentages at a glance
- **Popover dashboard** — click the tray icon for a detailed panel with per-provider cards and reset countdowns
- **Auto-refresh** — every 60 s in the background, every 5 s while the dashboard is open
- **Dark / Light / System theme** — follows Windows accent, switchable from the dashboard
- **No dependencies** — static CRT, WebView2 is bundled or auto-installed (Windows 11 always has it)
- **Per-user install** — no admin rights required

---

## Installation

### Option 1 — winget (Windows Package Manager)

```powershell
winget install VictorMagne.CloudTray
```

> The package is submitted to the [winget-pkgs](https://github.com/microsoft/winget-pkgs) community repository. Approval may take a few days after each release.

### Option 2 — Scoop

```powershell
scoop bucket add victor-magne https://github.com/Victor-Magne/scoop-bucket
scoop install cloudtray
```

### Option 3 — Installer (direct download)

Download `CloudTray_Setup_<version>.exe` from the [Releases page](https://github.com/Victor-Magne/claudtray/releases) and run it. The installer:

- Does **not** require administrator rights (installs to `%LocalAppData%\CloudTray`)
- Optionally adds CloudTray to Windows startup
- Automatically installs the WebView2 Runtime if it is missing (Windows 10 only — Windows 11 ships with it)

### Option 4 — Build from source

See [Building from source](#building-from-source) below.

---

## How it works

### Claude usage

CloudTray reads the OAuth access token stored by Claude Code at:

```
%USERPROFILE%\.claude\.credentials.json
```

It then calls Anthropic's usage endpoint and displays the `utilization` percentage for each rolling window. No credentials are stored or transmitted anywhere other than Anthropic's own API.

You can also set the token via environment variable (useful for testing):

```powershell
$env:CLAUDE_CODE_OAUTH_TOKEN = "sk-ant-..."
```

### Other providers

Copilot, Codex, and Antigravity usage is read from local rate-limit snapshot files that each tool writes to disk. CloudTray inspects running processes and known file paths to find active sessions.

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

### Prerequisites

- [Rust](https://rustup.rs/) (stable, MSVC toolchain)
- [Inno Setup 6](https://jrsoftware.org/isdl.php) (only needed to build the installer)
- Windows 10 or 11

### Steps

```powershell
git clone https://github.com/Victor-Magne/cloudtray.git
cd cloudtray

# Build the release binary (static CRT, no MSVC Redist needed)
cargo build --release

# Run directly
.\target\release\cloudtray.exe

# Build the installer (requires Inno Setup 6)
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer.iss
# Output: installer_output\CloudTray_Setup_<version>.exe
```

### Debug helper

Set `CLOUDTRAY_DUMP=1` to write a JSON snapshot to `%TEMP%\cloudtray_snapshot.json` and exit without showing any UI — handy for checking what the app sees:

```powershell
$env:CLOUDTRAY_DUMP = "1"; .\target\release\cloudtray.exe
```

---

## Project structure

```
src/
  main.rs          — Event loop, tray icon, dashboard wiring
  model.rs         — Snapshot / ProviderSnapshot / WindowUsage types
  monitor.rs       — QuotaMonitor: orchestrates all providers
  state.rs         — Persisted app state (theme, Copilot token)
  renderer.rs      — Dynamic tray icon (coloured ring, RGBA)
  window.rs        — WebView2 dashboard popover (IPC bridge)
  providers/
    mod.rs         — Provider trait
    claude.rs      — Anthropic OAuth usage API
    copilot.rs     — GitHub Copilot local snapshots
    codex.rs       — Codex local snapshots
    antigravity.rs — Antigravity local snapshots
    http.rs        — Shared HTTP agent (ureq)
assets/
  cloudtray.ico
  MicrosoftEdgeWebview2Setup.exe   — WebView2 bootstrapper (~1.6 MB)
installer.iss      — Inno Setup script
packaging/
  winget/          — winget manifest templates
  scoop/           — Scoop manifest template
```

---

## Package manager maintainers

After each release, update the version and SHA256 in:

- `packaging/winget/VictorMagne.CloudTray.installer.yaml` → submit PR to [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs)
- `packaging/scoop/cloudtray.json` → push to your Scoop bucket repo

The SHA256 of the installer is printed in the GitHub release notes.

---

## Contributing

Pull requests are welcome. For significant changes, open an issue first.

This is a Windows port of the original macOS [ClaudeBar](https://github.com/tddworks/ClaudeBar) by [@tddworks](https://github.com/tddworks). The provider logic, colour thresholds, and dashboard design follow the original as closely as possible.

---

## Credits

- Original macOS app: [tddworks/ClaudeBar](https://github.com/tddworks/ClaudeBar)
- Built with [tao](https://github.com/tauri-apps/tao), [tray-icon](https://github.com/tauri-apps/tray-icon), [wry](https://github.com/tauri-apps/wry), and [ureq](https://github.com/algesten/ureq)

---

## License

MIT — see [LICENSE](LICENSE).
