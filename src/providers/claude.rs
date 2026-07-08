use super::http::agent;
use super::{reset_from_epoch, Provider};
use crate::i18n::{catalog, Lang};
use crate::model::{ActiveSession, ProviderSnapshot, WindowUsage};
use crate::state::AppState;
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use std::time::Duration;

/// Claude (claude.ai / Claude Code subscription). Reads the *real* usage that
/// Claude Desktop shows, from Anthropic's OAuth usage endpoint, using an OAuth
/// token the user provides explicitly — generated with `claude setup-token`
/// and pasted into the settings panel (or set via `CLAUDE_CODE_OAUTH_TOKEN`).
/// The response reports `utilization` (percent USED) per rolling window, so
/// the remaining percentage is `100 - utilization`.
pub struct ClaudeProvider;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<Quota>,
    seven_day: Option<Quota>,
    seven_day_opus: Option<Quota>,
}

#[derive(Deserialize)]
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

        // Honour a previous 429's retry-after: don't touch the endpoint again
        // until the window has passed (the dashboard polls every 5 s while
        // open, which would otherwise keep extending the block).
        if let Some(wait) = rate_limited_for() {
            let mins = wait.as_secs().div_ceil(60).to_string();
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                &catalog(lang).provider_rate_limited.replace("{mins}", &mins),
            );
        }

        match fetch_usage(&token) {
            Ok(resp) => self.build(resp),
            Err(FetchError::Auth) => ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Token inválido/expirado — gera um novo com `claude setup-token`",
            ),
            Err(FetchError::RateLimited(retry_after)) => {
                let secs = retry_after.unwrap_or(60).min(3600);
                set_rate_limited(Duration::from_secs(secs));
                let mins = secs.div_ceil(60).to_string();
                ProviderSnapshot::unavailable(
                    self.id(),
                    self.name(),
                    &catalog(lang).provider_rate_limited.replace("{mins}", &mins),
                )
            }
            Err(FetchError::Other) => ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Não foi possível obter o uso",
            ),
        }
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

        let (total_tokens, estimated_cost_usd) = count_tokens_from_logs();
        ProviderSnapshot {
            id: self.id().to_string(),
            name: self.name().to_string(),
            available: true,
            note: None,
            windows,
            total_tokens: Some(total_tokens),
            estimated_cost_usd: Some(estimated_cost_usd),
            local_models: Vec::new(),
            active_sessions: detect_ide_sessions(),
        }
    }
}

/// Approximate input/output price per million tokens (USD) by model family.
fn model_prices(model: &str) -> (f64, f64) {
    let m = model.to_ascii_lowercase();
    if m.contains("opus")   { (15.0, 75.0) }
    else if m.contains("sonnet") { (3.0, 15.0) }
    else if m.contains("haiku")  { (0.80, 4.0) }
    else                         { (3.0, 15.0) }  // default: sonnet tier
}

fn window(key: &str, label: &str, q: Quota) -> WindowUsage {
    let used = q.utilization.unwrap_or(0.0).clamp(0.0, 100.0);
    let remaining = (100.0 - used).round().clamp(0.0, 100.0) as u32;
    let reset = q.resets_at.as_ref().and_then(parse_reset);
    WindowUsage::from_percent(key, label, remaining, reset)
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
fn load_token(state: &AppState) -> Option<String> {
    if let Some(t) = state.claude_token.as_ref() {
        if !t.trim().is_empty() {
            return Some(t.clone());
        }
    }
    if let Ok(t) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !t.trim().is_empty() {
            return Some(t);
        }
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

/// Sum tokens and estimate cost from Claude Code JSONL logs (last 30 days, up to 300 files).
pub fn count_tokens_from_logs() -> (u64, f64) {
    let Some(projects_dir) = dirs::home_dir().map(|h| h.join(".claude").join("projects")) else {
        return (0, 0.0);
    };
    if !projects_dir.exists() {
        return (0, 0.0);
    }
    let files = super::newest_jsonl_files(&projects_dir, 30);
    let mut total_tokens = 0u64;
    let mut total_cost = 0.0f64;
    for path in files.iter().take(300) {
        let (t, c) = count_file_tokens(path);
        total_tokens += t;
        total_cost += c;
    }
    (total_tokens, total_cost)
}

fn count_file_tokens(path: &std::path::Path) -> (u64, f64) {
    let Ok(content) = std::fs::read_to_string(path) else { return (0, 0.0); };
    if content.len() > 10_000_000 { return (0, 0.0); }
    let mut total_tok = 0u64;
    let mut total_cost = 0.0f64;
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue; };
        if let Some((tok, cost)) = extract_usage(&v) {
            total_tok += tok;
            total_cost += cost;
        }
    }
    (total_tok, total_cost)
}

fn extract_usage(v: &serde_json::Value) -> Option<(u64, f64)> {
    let model = v.get("model")
        .or_else(|| v.get("message").and_then(|m| m.get("model")))
        .and_then(|m| m.as_str())
        .unwrap_or("claude-sonnet");
    let (price_in, price_out) = model_prices(model);

    let usage = v.get("usage")
        .or_else(|| v.get("message").and_then(|m| m.get("usage")))?;
    let inp     = usage.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
    let out     = usage.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
    let cache_c = usage.get("cache_creation_input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
    let cache_r = usage.get("cache_read_input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);

    let tokens = inp + out + cache_c + cache_r;
    let cost = (inp as f64 * price_in
        + out as f64 * price_out
        + cache_c as f64 * price_in * 1.25   // cache write: 1.25× input
        + cache_r as f64 * price_in * 0.10)  // cache read:  0.10× input
        / 1_000_000.0;
    Some((tokens, cost))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_token_uses_only_the_user_provided_state_token() {
        let mut state = AppState::default();
        assert_eq!(load_token(&state), None, "no token configured ⇒ none");

        state.claude_token = Some("   ".to_string());
        assert_eq!(load_token(&state), None, "blank token ⇒ none");

        state.claude_token = Some("sk-ant-oat01-test".to_string());
        assert_eq!(load_token(&state).as_deref(), Some("sk-ant-oat01-test"));
    }
}

enum FetchError {
    /// 401/403 — the token itself was rejected.
    Auth,
    /// 429 — with the `retry-after` value (seconds) when the API sent one.
    RateLimited(Option<u64>),
    Other,
}

fn fetch_usage(token: &str) -> Result<UsageResponse, FetchError> {
    let token = token.trim().to_string();
    let mut last_err = FetchError::Other;
    // Retry only transport-level failures. A definitive HTTP answer (401, 429,
    // even 5xx) must NOT be retried in a tight loop — hammering the endpoint is
    // what gets the token rate-limited in the first place.
    for attempt in 0..3u64 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(attempt));
        }
        let mut resp = match agent(false)
            .get(USAGE_URL)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", "ClaudTray")
            .call()
        {
            Ok(r) => r,
            Err(_) => continue, // transport error — worth retrying
        };
        match resp.status().as_u16() {
            200 => {
                let text = resp
                    .body_mut()
                    .with_config()
                    .limit(super::http::MAX_BODY_BYTES)
                    .read_to_string()
                    .map_err(|_| FetchError::Other)?;
                return serde_json::from_str::<UsageResponse>(&text).map_err(|_| FetchError::Other);
            }
            401 | 403 => return Err(FetchError::Auth),
            429 => {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<u64>().ok());
                return Err(FetchError::RateLimited(retry_after));
            }
            _ => {
                last_err = FetchError::Other;
                break;
            }
        }
    }
    Err(last_err)
}
