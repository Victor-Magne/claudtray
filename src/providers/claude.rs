use super::http::agent;
use super::{reset_from_epoch, Provider};
use crate::model::{ProviderSnapshot, WindowUsage};
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

        ProviderSnapshot {
            id: self.id().to_string(),
            name: self.name().to_string(),
            available: true,
            note: None,
            windows,
        }
    }
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

fn fetch_usage(token: &str) -> Option<UsageResponse> {
    let mut resp = agent(false)
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {}", token.trim()))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", "ClaudeBar")
        .call()
        .ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp.body_mut().read_to_string().ok()?;
    serde_json::from_str::<UsageResponse>(&text).ok()
}
