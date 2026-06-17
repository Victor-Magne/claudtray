use super::http::agent;
use super::Provider;
use crate::model::{ProviderSnapshot, WindowUsage};
use crate::state::AppState;
use serde::Deserialize;

/// GitHub Copilot. Reads the premium-request quota from the same internal
/// endpoint the editors use (`/copilot_internal/user`). Requires a GitHub token
/// with Copilot access — VS Code keeps its token in the OS credential vault, so
/// on Windows the user typically supplies one in Settings (or via env / `gh`).
pub struct CopilotProvider;

#[derive(Deserialize)]
struct UserResp {
    copilot_plan: Option<String>,
    quota_snapshots: Option<Quotas>,
}

#[derive(Deserialize)]
struct Quotas {
    premium_interactions: Option<Quota>,
}

#[derive(Deserialize)]
struct Quota {
    entitlement: Option<f64>,
    remaining: Option<f64>,
    percent_remaining: Option<f64>,
    unlimited: Option<bool>,
}

impl Provider for CopilotProvider {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn name(&self) -> &'static str {
        "Copilot"
    }

    fn collect(&self, state: &AppState) -> ProviderSnapshot {
        let Some(token) = resolve_token(state) else {
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Configura token nas definições",
            );
        };

        match fetch(&token) {
            Some(resp) => self.build(resp),
            None => ProviderSnapshot::unavailable(self.id(), self.name(), "Falha ao obter quota"),
        }
    }
}

impl CopilotProvider {
    fn build(&self, resp: UserResp) -> ProviderSnapshot {
        let q = resp.quota_snapshots.and_then(|q| q.premium_interactions);
        let window = match q {
            Some(q) if q.unlimited.unwrap_or(false) => {
                WindowUsage::from_percent("premium", "PREMIUM ∞", 100, None)
            }
            Some(q) => {
                let pct = q
                    .percent_remaining
                    .or_else(|| match (q.entitlement, q.remaining) {
                        (Some(e), Some(r)) if e > 0.0 => Some((r / e) * 100.0),
                        _ => None,
                    })
                    .unwrap_or(100.0)
                    .round()
                    .clamp(0.0, 100.0) as u32;
                WindowUsage::from_percent("premium", "PREMIUM", pct, None)
            }
            None => WindowUsage::from_percent("premium", "PREMIUM", 100, None),
        };

        ProviderSnapshot {
            id: self.id().to_string(),
            name: self.name().to_string(),
            available: true,
            note: resp.copilot_plan,
            windows: vec![window],
        }
    }
}

fn fetch(token: &str) -> Option<UserResp> {
    let mut resp = agent(false)
        .get("https://api.github.com/copilot_internal/user")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "ClaudeBar")
        .call()
        .ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp.body_mut().read_to_string().ok()?;
    serde_json::from_str::<UserResp>(&text).ok()
}

fn resolve_token(state: &AppState) -> Option<String> {
    if let Some(t) = state.copilot_token.as_ref().filter(|t| !t.is_empty()) {
        return Some(t.clone());
    }
    for var in ["GH_TOKEN", "GITHUB_TOKEN", "GH_COPILOT_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    token_from_gh_hosts()
}

/// Pull `oauth_token:` from the `gh` CLI hosts file without a YAML dependency.
fn token_from_gh_hosts() -> Option<String> {
    let mut candidates = Vec::new();
    if let Some(cfg) = dirs::config_dir() {
        candidates.push(cfg.join("GitHub CLI").join("hosts.yml"));
        candidates.push(cfg.join("gh").join("hosts.yml"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".config").join("gh").join("hosts.yml"));
    }

    for path in candidates {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            if let Some(rest) = line.trim().strip_prefix("oauth_token:") {
                let token = rest.trim().trim_matches('"').trim_matches('\'');
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}
