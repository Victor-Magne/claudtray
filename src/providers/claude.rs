use super::http::agent;
use super::{reset_from_epoch, Provider};
use crate::i18n::{catalog, Lang};
use crate::model::{ActiveSession, ProviderSnapshot, WindowUsage};
use crate::state::AppState;
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Claude (claude.ai / Claude Code subscription). Reads the *real* usage that
/// Claude Desktop shows, from Anthropic's OAuth usage endpoint, using an OAuth
/// token the user provides explicitly — generated with `claude setup-token`
/// and pasted into the settings panel (or set via `CLAUDE_CODE_OAUTH_TOKEN`).
/// The response reports `utilization` (percent USED) per rolling window, so
/// the remaining percentage is `100 - utilization`.
pub struct ClaudeProvider;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Deserialize, Serialize)]
struct UsageResponse {
    five_hour: Option<Quota>,
    seven_day: Option<Quota>,
    seven_day_opus: Option<Quota>,
}

#[derive(Deserialize, Serialize)]
struct Quota {
    /// Percent of the window already consumed (0-100).
    utilization: Option<f64>,
    /// When the window resets — ISO8601 string or epoch seconds.
    resets_at: Option<serde_json::Value>,
}

impl Provider for ClaudeProvider {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn min_interval(&self) -> std::time::Duration {
        Duration::from_secs(SHARED_TTL_SECS)
    }

    fn name(&self) -> &'static str {
        "Claude"
    }

    fn collect(&self, state: &AppState) -> ProviderSnapshot {
        let Some(token) = load_token(state) else {
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Corre `claude setup-token` e cola o token nas definições",
            );
        };

        let lang = Lang::from_pref(&state.language);

        let now = now_secs();
        // The shared cache belongs to the account in `~/.claude/.credentials.json`.
        // A token the user pasted (Settings / env var) may be another account, so
        // then the file must not be trusted; it also keeps public builds, which
        // never fall back to that credentials file, from reading it at all.
        let shared = if explicit_token(state).is_none() {
            shared_cache_paths()
                .and_then(|(file, identity)| shared_usage(&std::fs::read(file).ok()?, &identity, now))
        } else {
            None
        };
        let own = own_cache_path();

        let result = usage_with_fallback(shared, own.as_deref().and_then(load_own), now, || {
            // Honour a previous 429's retry-after: don't touch the endpoint
            // again until the window has passed (the dashboard polls every 5 s
            // while open, which would otherwise keep extending the block).
            if let Some(wait) = rate_limited_for() {
                return Err(FetchError::RateLimited(Some(wait.as_secs())));
            }
            match fetch_usage(&token) {
                Ok(resp) => {
                    if let Some(p) = own.as_deref() {
                        save_own(p, &resp, now);
                    }
                    Ok(resp)
                }
                Err(FetchError::RateLimited(retry_after)) => {
                    let secs = retry_after.unwrap_or(60).min(3600);
                    set_rate_limited(Duration::from_secs(secs));
                    Err(FetchError::RateLimited(Some(secs)))
                }
                Err(e) => Err(e),
            }
        });

        match result {
            Ok((resp, stale_secs)) => {
                let mut snap = self.build(resp);
                snap.stale_secs = stale_secs;
                snap
            }
            Err(FetchError::Auth) => {
                let note = "Token inválido/expirado — gera um novo com `claude setup-token`";
                // Only a token the user supplied can be "signed out"; the
                // short-lived CLI token (auto-credentials) just ages out.
                if explicit_token(state).is_some() {
                    ProviderSnapshot::signed_out(self.id(), self.name(), note)
                } else {
                    ProviderSnapshot::unavailable(self.id(), self.name(), note)
                }
            }
            Err(FetchError::RateLimited(retry_after)) => {
                let mins = retry_after.unwrap_or(60).div_ceil(60).to_string();
                // The retry-after also tells the monitor when to try again (it
                // keeps its own sticky guard for forced refreshes).
                ProviderSnapshot::unavailable(
                    self.id(),
                    self.name(),
                    &catalog(lang).provider_rate_limited.replace("{mins}", &mins),
                )
                .retry_in(retry_after)
            }
            Err(FetchError::Other) => ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Não foi possível obter o uso",
            ),
        }
    }
}

/// Claude's cadence, and the TTL of the cache shared with the DMS shell plugin
/// (`~/.claude/usage-cache.json`): both sides refresh at most once per 90 s.
const SHARED_TTL_SECS: u64 = 90;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// (shared cache file, identity string). The plugin stores the path of the
/// credentials file as identity, so it only matches the same machine and user.
fn shared_cache_paths() -> Option<(PathBuf, String)> {
    let claude = dirs::home_dir()?.join(".claude");
    let identity = claude.join(".credentials.json").to_string_lossy().into_owned();
    Some((claude.join("usage-cache.json"), identity))
}

/// The plugin's file: `{cached_at (unix secs), identity, data}`. Read-only here.
#[derive(Deserialize)]
struct SharedCache {
    cached_at: u64,
    identity: String,
    data: UsageResponse,
}

/// `data` of the shared cache, if it is fresh, ours and well-formed. Anything
/// else (corrupt, foreign identity, stale, from the future) is ignored.
fn shared_usage(raw: &[u8], identity: &str, now: u64) -> Option<UsageResponse> {
    let c: SharedCache = serde_json::from_slice(raw).ok()?;
    let age = now.checked_sub(c.cached_at)?;
    (c.identity == identity && age < SHARED_TTL_SECS).then_some(c.data)
}

/// Our own last good API response, under `dirs::cache_dir()/ClaudTray`.
#[derive(Deserialize, Serialize)]
struct OwnCache {
    cached_at: u64,
    data: UsageResponse,
}

fn own_cache_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("ClaudTray").join("claude_usage_api.json"))
}

fn load_own(path: &Path) -> Option<OwnCache> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Atomic (tmp + rename), best effort.
fn save_own(path: &Path, data: &UsageResponse, now: u64) {
    #[derive(Serialize)]
    struct Out<'a> {
        cached_at: u64,
        data: &'a UsageResponse,
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec(&Out { cached_at: now, data }) {
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// A fresh shared cache wins with zero requests; otherwise `fetch`. A failed
/// fetch falls back to our last good response (second value = its age in
/// seconds, shown as "stale") — except `Auth`, which is never masked.
fn usage_with_fallback(
    shared: Option<UsageResponse>,
    own: Option<OwnCache>,
    now: u64,
    fetch: impl FnOnce() -> Result<UsageResponse, FetchError>,
) -> Result<(UsageResponse, Option<u64>), FetchError> {
    if let Some(resp) = shared {
        return Ok((resp, None));
    }
    match fetch() {
        Ok(resp) => Ok((resp, None)),
        Err(FetchError::Auth) => Err(FetchError::Auth),
        Err(e) => own
            .map(|c| (c.data, Some(now.saturating_sub(c.cached_at))))
            .ok_or(e),
    }
}

/// Sticky rate-limit window fed by the endpoint's `retry-after` header.
static RATE_LIMITED_UNTIL: std::sync::RwLock<Option<std::time::Instant>> =
    std::sync::RwLock::new(None);

fn rate_limited_for() -> Option<Duration> {
    let until = (*RATE_LIMITED_UNTIL.read().ok()?)?;
    until.checked_duration_since(std::time::Instant::now())
}

fn set_rate_limited(wait: Duration) {
    if let Ok(mut w) = RATE_LIMITED_UNTIL.write() {
        *w = Some(std::time::Instant::now() + wait);
    }
}

impl ClaudeProvider {
    fn build(&self, resp: UsageResponse) -> ProviderSnapshot {
        let mut windows = Vec::new();
        if let Some(q) = resp.five_hour {
            windows.push(window("session", "SESSION", q));
        }
        if let Some(q) = resp.seven_day {
            windows.push(window("weekly", "WEEKLY", q));
        }
        if let Some(q) = resp.seven_day_opus {
            windows.push(window("opus", "OPUS", q));
        }

        if windows.is_empty() {
            return ProviderSnapshot::unavailable(self.id(), self.name(), "Sem dados de uso");
        }

        let usage = super::claude_usage::collect();
        ProviderSnapshot {
            id: self.id().to_string(),
            name: self.name().to_string(),
            available: true,
            note: None,
            windows,
            total_tokens: usage.as_ref().map(|u| u.month.tokens),
            estimated_cost_usd: usage.as_ref().map(|u| u.month.cost),
            usage,
            local_models: Vec::new(),
            active_sessions: detect_ide_sessions(),
            stale_secs: None,
            auth_failed: false,
            retry_in_secs: None,
            fetched_at: None,
        }
    }
}

fn window(key: &str, label: &str, q: Quota) -> WindowUsage {
    let used = q.utilization.unwrap_or(0.0).clamp(0.0, 100.0);
    let remaining = (100.0 - used).round().clamp(0.0, 100.0) as u32;
    let reset = q.resets_at.as_ref().and_then(parse_reset);
    let mut w = WindowUsage::from_percent(key, label, remaining, reset);
    w.window_secs = Some(if key == "session" { 5 * 3600 } else { 7 * 24 * 3600 });
    w
}

fn parse_reset(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        if let Ok(dt) = s.parse::<DateTime<Utc>>() {
            return Some(dt.with_timezone(&Local).to_rfc3339());
        }
        if let Ok(n) = s.parse::<i64>() {
            return reset_from_epoch(n);
        }
        return None;
    }
    if let Some(n) = v.as_i64() {
        return reset_from_epoch(n);
    }
    v.as_f64().and_then(|f| reset_from_epoch(f as i64))
}

/// Resolve the Claude OAuth token from something the user provided explicitly:
/// the token pasted in the settings panel first, then the
/// `CLAUDE_CODE_OAUTH_TOKEN` env var. Claude Code's own credential store
/// (`~/.claude/.credentials.json`) is intentionally NOT read — except in
/// personal builds compiled with the `auto-credentials` feature, where it is
/// the last-resort fallback.
/// The token the user configured (Settings or `CLAUDE_CODE_OAUTH_TOKEN`).
fn explicit_token(state: &AppState) -> Option<String> {
    if let Some(t) = state.claude_token.as_ref() {
        if !t.trim().is_empty() {
            return Some(t.clone());
        }
    }
    std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
}

fn load_token(state: &AppState) -> Option<String> {
    if let Some(t) = explicit_token(state) {
        return Some(t);
    }
    #[cfg(feature = "auto-credentials")]
    if let Some(t) = load_token_from_claude_code() {
        return Some(t);
    }
    None
}

/// Personal builds only: read the access token Claude Code keeps in
/// `~/.claude/.credentials.json` (short-lived; Claude Code refreshes it).
#[cfg(feature = "auto-credentials")]
fn load_token_from_claude_code() -> Option<String> {
    #[derive(Deserialize)]
    struct Credentials {
        #[serde(rename = "claudeAiOauth")]
        oauth: Option<OAuth>,
    }
    #[derive(Deserialize)]
    struct OAuth {
        #[serde(rename = "accessToken")]
        access_token: Option<String>,
    }

    let path = dirs::home_dir()?.join(".claude").join(".credentials.json");
    let content = std::fs::read_to_string(path).ok()?;
    let creds: Credentials = serde_json::from_str(&content).ok()?;
    creds.oauth?.access_token.filter(|t| !t.trim().is_empty())
}

/// Scan `~/.claude/ide/*.lock` files for active Claude Code IDE sessions.
/// Each lock file is named `{pid}.lock` and contains JSON with ideName,
/// workspaceFolders, etc. We verify the PID is still running before reporting.
/// Scan `~/.claude/ide/*.lock` files for active Claude Code IDE sessions.
/// Each lock file is named `{pid}.lock` and contains JSON with ideName,
/// workspaceFolders, etc. We verify the PID is still running before reporting.
/// Executables we accept as genuine Claude Code IDE hosts. A lock file names a
/// PID; without this check any attacker with write access to `~/.claude/ide/`
/// could point a lock at a long-lived process (e.g. `explorer.exe`) and forge a
/// session. Matched case-insensitively against the process image file name.
const KNOWN_IDE_EXES: &[&str] = &[
    "code.exe",
    "code - insiders.exe",
    "cursor.exe",
    "windsurf.exe",
    "antigravity.exe",
    "node.exe",
    "codium.exe",
    "vscodium.exe",
];

fn detect_ide_sessions() -> Vec<ActiveSession> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let Some(ide_dir) = dirs::home_dir().map(|h| h.join(".claude").join("ide")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&ide_dir) else {
        return Vec::new();
    };

    let mut sys = System::new();
    let mut sessions = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue; };
        let Ok(pid_num) = stem.parse::<u32>() else { continue; };

        let Ok(content) = std::fs::read_to_string(&path) else { continue; };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) else { continue; };

        // Verify the PID is alive AND that its executable is a known IDE host,
        // so a forged lock file pointing at an arbitrary live PID is rejected.
        let pid = Pid::from_u32(pid_num);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        let Some(proc_) = sys.process(pid) else {
            continue;
        };
        let exe_name = proc_
            .exe()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !KNOWN_IDE_EXES.contains(&exe_name.as_str()) {
            continue;
        }

        let ide = val.get("ideName")
            .and_then(|v| v.as_str())
            .unwrap_or("IDE")
            .to_string();

        // Use the last path component of the first workspace folder.
        let workspace = val.get("workspaceFolders")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("—")
            .to_string();

        sessions.push(ActiveSession { ide, workspace });
    }
    sessions
}

enum FetchError {
    /// 401/403 — the token itself was rejected.
    Auth,
    /// 429 — with the `retry-after` value (seconds) when the API sent one.
    RateLimited(Option<u64>),
    Other,
}

fn fetch_usage(token: &str) -> Result<UsageResponse, FetchError> {
    let token = token.trim();
    // One attempt per tick: retrying would hold the refresh (and the UI) behind
    // a slow endpoint, and hammering it is what gets the token rate-limited.
    // The monitor's interval / `FAIL_RETRY` spacing re-tries later.
    let mut resp = agent(false)
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", "ClaudTray")
        .call()
        .map_err(|_| FetchError::Other)?;
    match resp.status().as_u16() {
        200 => {
            let text = resp
                .body_mut()
                .with_config()
                .limit(super::http::MAX_BODY_BYTES)
                .read_to_string()
                .map_err(|_| FetchError::Other)?;
            serde_json::from_str::<UsageResponse>(&text).map_err(|_| FetchError::Other)
        }
        401 | 403 => Err(FetchError::Auth),
        429 => {
            Err(FetchError::RateLimited(super::http::retry_after_secs(resp.headers())))
        }
        _ => Err(FetchError::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;

    #[test]
    fn load_token_uses_only_the_user_provided_state_token() {
        let mut state = AppState::default();
        assert_eq!(load_token(&state), None, "no token configured ⇒ none");

        state.claude_token = Some("   ".to_string());
        assert_eq!(load_token(&state), None, "blank token ⇒ none");

        state.claude_token = Some("sk-ant-oat01-test".to_string());
        assert_eq!(load_token(&state).as_deref(), Some("sk-ant-oat01-test"));
    }

    #[test]
    fn window_reports_remaining_as_100_minus_utilization() {
        let resp: UsageResponse =
            serde_json::from_str(include_str!("../../tests/fixtures/claude/usage.json")).unwrap();

        let session = window("session", "SESSION", resp.five_hour.unwrap());
        assert_eq!(session.remaining_pct, 58); // 100 - 42.5, rounded
        assert_eq!(session.status, Status::Healthy);
        assert!(session.reset_at.is_some());

        let opus = window("opus", "OPUS", resp.seven_day_opus.unwrap());
        assert_eq!(opus.remaining_pct, 12);
        assert_eq!(opus.status, Status::Critical);
    }

    #[test]
    fn window_handles_a_fully_consumed_quota() {
        let resp: UsageResponse =
            serde_json::from_str(include_str!("../../tests/fixtures/claude/usage_exhausted.json"))
                .unwrap();
        let session = window("session", "SESSION", resp.five_hour.unwrap());
        assert_eq!(session.remaining_pct, 0);
        assert_eq!(session.status, Status::Depleted);
    }

    const IDENTITY: &str = "/home/u/.claude/.credentials.json";
    const NOW: u64 = 1_800_000_000;

    fn usage() -> UsageResponse {
        serde_json::from_str(include_str!("../../tests/fixtures/claude/usage.json")).unwrap()
    }

    fn shared_file(cached_at: u64, identity: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "cached_at": cached_at,
            "identity": identity,
            "data": usage(),
        }))
        .unwrap()
    }

    fn session_used(r: &UsageResponse) -> Option<f64> {
        r.five_hour.as_ref()?.utilization
    }

    #[test]
    fn shared_cache_fresh_is_used_without_any_request() {
        let shared = shared_usage(&shared_file(NOW - 89, IDENTITY), IDENTITY, NOW);
        assert!(shared.is_some());
        let mut requests = 0;
        let r = usage_with_fallback(shared, None, NOW, || {
            requests += 1;
            Err(FetchError::Other)
        });
        assert!(matches!(r, Ok((_, None))));
        assert_eq!(requests, 0);
    }

    #[test]
    fn shared_cache_stale_foreign_future_or_corrupt_is_ignored() {
        assert!(shared_usage(&shared_file(NOW - 90, IDENTITY), IDENTITY, NOW).is_none());
        assert!(shared_usage(&shared_file(NOW, "/other/.credentials.json"), IDENTITY, NOW).is_none());
        assert!(shared_usage(&shared_file(NOW + 60, IDENTITY), IDENTITY, NOW).is_none());
        for junk in [&b""[..], b"{", b"[]", b"{\"cached_at\":1}", b"\xff\xfe"] {
            assert!(shared_usage(junk, IDENTITY, NOW).is_none());
        }

        // Stale => exactly one request.
        let mut requests = 0;
        let r = usage_with_fallback(
            shared_usage(&shared_file(NOW - 500, IDENTITY), IDENTITY, NOW),
            None,
            NOW,
            || {
                requests += 1;
                Ok(usage())
            },
        );
        assert!(matches!(r, Ok((_, None))));
        assert_eq!(requests, 1);
    }

    #[test]
    fn own_cache_roundtrips_atomically_and_bad_files_are_ignored() {
        let dir = std::env::temp_dir().join(format!("claudtray-test-{}", std::process::id()));
        let path = dir.join("nested").join("claude_usage_api.json");
        assert!(load_own(&path).is_none(), "missing file");

        save_own(&path, &usage(), NOW);
        let saved = load_own(&path).expect("roundtrip");
        assert_eq!(saved.cached_at, NOW);
        assert_eq!(session_used(&saved.data), Some(42.5));
        assert!(!path.with_extension("json.tmp").exists(), "tmp renamed away");

        std::fs::write(&path, b"{not json").unwrap();
        assert!(load_own(&path).is_none(), "corrupt file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn own_cache_is_used_when_the_request_fails_but_not_on_auth() {
        let own = || Some(OwnCache { cached_at: NOW - 300, data: usage() });
        for err in [FetchError::RateLimited(Some(60)), FetchError::Other] {
            let r = usage_with_fallback(None, own(), NOW, || Err(err));
            match r {
                Ok((resp, stale)) => {
                    assert_eq!(session_used(&resp), Some(42.5));
                    assert_eq!(stale, Some(300));
                }
                Err(_) => panic!("own cache should have been served"),
            }
        }
        // No own cache => the original error surfaces.
        let r = usage_with_fallback(None, None, NOW, || Err(FetchError::RateLimited(Some(60))));
        assert!(matches!(r, Err(FetchError::RateLimited(Some(60)))));
        // 401/403 is never masked by old numbers.
        let r = usage_with_fallback(None, own(), NOW, || Err(FetchError::Auth));
        assert!(matches!(r, Err(FetchError::Auth)));
    }

    #[test]
    fn parse_reset_accepts_iso_string_and_epoch() {
        assert!(parse_reset(&serde_json::json!("2026-01-15T18:00:00Z")).is_some());
        assert!(parse_reset(&serde_json::json!(1_768_500_000_i64)).is_some());
        assert!(parse_reset(&serde_json::json!("not-a-date")).is_none());
    }
}
