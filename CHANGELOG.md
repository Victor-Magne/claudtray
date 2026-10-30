# Changelog

All notable changes to ClaudTray are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases before 1.4.0 are only tagged in git and described in their
[GitHub release notes](https://github.com/Victor-Magne/claudtray/releases).

## [Unreleased]

### Added
- A "signed out" notification when Claude, Copilot or OpenRouter rejects your
  token (HTTP 401). It fires once per provider and again only after the
  provider has worked and then failed anew. A short-lived Claude Code CLI token
  picked up automatically (`auto-credentials` builds) never triggers it.
- The tray icon now carries a mark in the ring's gap for states the colour
  cannot tell: a solid disc when a provider is signed out, a bar when a
  provider is showing stale data.
- The tray tooltip lists one row per provider (worst first) with its lowest
  window, remaining percentage and reset countdown, instead of only the single
  worst window. Signed-out and stale providers are flagged in it. On Windows,
  rows that do not fit its 127-character limit are dropped whole.

### Security
- Provider HTTP requests no longer follow redirects, so a custom secret header
  (e.g. the Antigravity CSRF token) can never be forwarded to another address.

## [1.4.0] - 2026-08-31

### Added
- Predictive quota alerts: a notification now also fires when a window is
  projected to run out within ~30 minutes, or resets within ~20 minutes — not
  just when it crosses a status threshold.
- The tray tooltip shows the worst window across *all* providers with its
  remaining percentage and reset countdown (it was Claude-only before).
- The dashboard flags stale provider data ("no response from provider — Xm ago")
  instead of presenting a cached snapshot as if it were current.

### Fixed
- A window dropping straight to 0% (Depleted) now triggers a notification. It was
  previously silent because the alert gate only caught the Critical status rank.

### Internal
- Offline test coverage for every provider parser, driven by captured JSON/JSONL
  fixtures.
- Parsing split from I/O in the Codex, Gemini and Ollama providers so it can be
  unit-tested without network or filesystem.

[1.4.0]: https://github.com/Victor-Magne/claudtray/releases/tag/v1.4.0
