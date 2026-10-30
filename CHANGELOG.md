# Changelog

All notable changes to ClaudTray are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases before 1.4.0 are only tagged in git and described in their
[GitHub release notes](https://github.com/Victor-Magne/claudtray/releases).

## [Unreleased]

### Added
- Claude page redesigned: a ring per quota window (filled with the share used,
  with a tick where the elapsed time says you should be), pace ("30% under
  pace", skipped in a window's first 3%), token consumption for today / week /
  month with estimated cost, daily activity, tokens per model and the date of
  your first record. Built from `~/.claude/projects`, de-duplicated by message
  and cached per file so a relaunch only re-reads what changed.
- Reset countdowns show days ("2d 3h 28m").
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

### Changed
- Each provider now refreshes on its own minimum interval (Claude 60 s,
  Copilot and OpenRouter 5 min, Gemini 10 min; local Codex, Antigravity and
  Ollama every tick) instead of every provider on every 5 s tick, which caused
  429s and timeouts while the popover was open. Skipped providers reuse their
  last snapshot without being marked stale; the manual refresh and saving a new
  credential still refresh everything at once.
- The dashboard no longer waits for the slowest provider: each provider's
  result is shown as soon as it arrives (the others keep their last value), and
  a provider that has not answered within 8 s keeps its previous value marked
  stale. Providers make one attempt per refresh instead of up to three with 1 s
  and 2 s sleeps between them; a failed one is retried by the next refresh
  rather than inside the same one.

### Fixed
- Token totals no longer skip transcripts over 10 MB or past the 300 newest
  files.
- Estimated cost uses current per-model prices (Opus 5.5, Sonnet 5, Haiku 5.5,
  Fable…) with each model's own cache-read price, instead of one old tier per
  family, and 1-hour cache writes at 2×. It stays an estimate.
- "Since" and the totals keep accumulating after Claude Code deletes old
  transcripts (the cache no longer forgets them); deleting
  `~/.cache/ClaudTray/claude_usage.json` starts over.

### Fixed
- The popover now closes when the pointer leaves it and stays out for about
  0.4 s (coming back in time cancels it). It does not close while Settings is
  open or a mouse button is held. Before, on Wayland layer-shell the popover
  stayed open after the pointer left it because keyboard focus never changed.

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
