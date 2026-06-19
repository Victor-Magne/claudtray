use super::http::agent;
use super::{reset_from_epoch, Provider};
use crate::model::{ActiveSession, ProviderSnapshot, WindowUsage};
use crate::state::AppState;
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;

/// Claude (claude.ai / Claude Code subscription). Reads the *real* usage that
/// Claude Desktop shows, from Anthropic's OAuth usage endpoint, using the
/// access token Claude Code stores in `~/.claude/.credentials.json`. The
/// response reports `utilization` (percent USED) per rolling window, so the
/// remaining percentage is `100 - utilization`.
pub struct ClaudeProvider;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

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

    fn collect(&self, _state: &AppState) -> ProviderSnapshot {
        let Some(token) = load_token() else {
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Inicia sessão no Claude Code",
            );
        };

        match fetch_usage(&token) {
            Some(resp) => self.build(resp),
            None => ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Não foi possível obter o uso (token expirado?)",
            ),
        }
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

/// Resolve the Claude Code OAuth access token: env override first, then the
/// `~/.claude/.credentials.json` file.
fn load_token() -> Option<String> {
    if let Ok(t) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !t.is_empty() {
            return Some(t);
        }
    }
    let path = dirs::home_dir()?.join(".claude").join(".credentials.json");
    let content = std::fs::read_to_string(path).ok()?;
    let creds: Credentials = serde_json::from_str(&content).ok()?;
    creds
        .oauth?
        .access_token
        .filter(|t| !t.trim().is_empty())
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

fn fetch_usage(token: &str) -> Option<UsageResponse> {
    let token = token.trim().to_string();
    super::http::with_retry(3, 1, || {
        let mut resp = agent(false)
            .get(USAGE_URL)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", "CloudTray")
            .call()
            .ok()?;
        if resp.status().as_u16() != 200 {
            return None;
        }
        let text = resp
            .body_mut()
            .with_config()
            .limit(super::http::MAX_BODY_BYTES)
            .read_to_string()
            .ok()?;
        serde_json::from_str::<UsageResponse>(&text).ok()
    })
}
