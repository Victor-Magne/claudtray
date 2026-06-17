use super::{newest_jsonl_files, reset_from_epoch, Provider};
use crate::model::{ProviderSnapshot, WindowUsage};
use crate::state::AppState;
use serde::Deserialize;

/// OpenAI Codex CLI. Reads the rate-limit snapshot that Codex writes locally
/// into its session rollouts (`~/.codex/sessions/**/*.jsonl`) — no network call
/// or token required. Each `token_count` event embeds a `rate_limits` object
/// with the live `used_percent` per window.
pub struct CodexProvider;

#[derive(Deserialize)]
struct Line {
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    ptype: Option<String>,
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
struct RateLimits {
    primary: Option<Window>,
    secondary: Option<Window>,
}

#[derive(Deserialize)]
struct Window {
    used_percent: Option<f64>,
    window_minutes: Option<i64>,
    resets_at: Option<i64>,
}

impl Provider for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn name(&self) -> &'static str {
        "Codex"
    }

    fn collect(&self, _state: &AppState) -> ProviderSnapshot {
        let Some(root) = dirs::home_dir().map(|h| h.join(".codex")) else {
            return ProviderSnapshot::unavailable(self.id(), self.name(), "Não detetado");
        };
        if !root.exists() {
            return ProviderSnapshot::unavailable(self.id(), self.name(), "Codex não detetado");
        }

        let sessions = root.join("sessions");
        // Most recent rollouts first; the latest one with a rate_limits wins.
        for path in newest_jsonl_files(&sessions, 30).into_iter().take(20) {
            if let Some(rl) = latest_rate_limits(&path) {
                let mut windows = Vec::new();
                if let Some(w) = rl.primary.and_then(|w| window(w, "primary")) {
                    windows.push(w);
                }
                if let Some(w) = rl.secondary.and_then(|w| window(w, "secondary")) {
                    windows.push(w);
                }
                if !windows.is_empty() {
                    return ProviderSnapshot {
                        id: self.id().to_string(),
                        name: self.name().to_string(),
                        available: true,
                        note: None,
                        windows,
                    };
                }
            }
        }

        ProviderSnapshot::unavailable(self.id(), self.name(), "Sem dados de limites")
    }
}

/// Scan a rollout for the last `token_count` event carrying rate limits.
fn latest_rate_limits(path: &std::path::Path) -> Option<RateLimits> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut last = None;
    for line in content.lines() {
        let Ok(parsed) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        if let Some(p) = parsed.payload {
            if p.ptype.as_deref() == Some("token_count") {
                if let Some(rl) = p.rate_limits {
                    last = Some(rl);
                }
            }
        }
    }
    last
}

fn window(w: Window, fallback_key: &str) -> Option<WindowUsage> {
    let used = w.used_percent?;
    let remaining = (100.0 - used).round().clamp(0.0, 100.0) as u32;
    let (key, label) = window_label(w.window_minutes, fallback_key);
    let reset = w.resets_at.and_then(reset_from_epoch);
    Some(WindowUsage::from_percent(&key, &label, remaining, reset))
}

/// Human label for a Codex rate-limit window based on its length in minutes.
fn window_label(minutes: Option<i64>, fallback_key: &str) -> (String, String) {
    match minutes.unwrap_or(0) {
        300 => ("session".into(), "5H".into()),
        1440 => ("daily".into(), "DIÁRIO".into()),
        10080 => ("weekly".into(), "SEMANAL".into()),
        43200 => ("monthly".into(), "MENSAL".into()),
        m if m > 0 => (fallback_key.to_string(), format!("{}H", m / 60)),
        _ => (fallback_key.to_string(), "LIMITE".into()),
    }
}
