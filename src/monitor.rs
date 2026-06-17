use crate::model::{ProviderSnapshot, Snapshot};
use crate::providers;
use crate::state::AppState;
use chrono::Local;

/// Default budget floors per window key. These only matter before the user has
/// built up real usage history: they give the gauge sensible headroom on a
/// cold start (so a fresh install doesn't read 0%/100%-used). Once observed
/// usage exceeds a floor, the historical peak takes over and the gauge
/// self-calibrates to the real limit.
fn budget_floor(window_key: &str) -> u64 {
    match window_key {
        "session" => 2_000_000,  // ~2M tokens in a 5h window
        "weekly" => 10_000_000,  // ~10M tokens in 7 days
        "opus" => 5_000_000,     // ~5M Opus tokens in 7 days
        _ => 2_000_000,
    }
}

/// Single source of truth: runs every provider, applies the auto-detected
/// budgets and produces a [`Snapshot`] for the dashboard + tray icon.
pub struct QuotaMonitor {
    pub state: AppState,
    pub last: Option<Snapshot>,
}

impl QuotaMonitor {
    pub fn new() -> Self {
        Self {
            state: AppState::load(),
            last: None,
        }
    }

    /// Collect from all providers, derive budgets/percentages, persist the
    /// observed maxima and theme, and return the fresh snapshot.
    pub fn refresh(&mut self) -> Snapshot {
        let mut snaps: Vec<ProviderSnapshot> = Vec::new();

        for provider in providers::all() {
            let mut snap = provider.collect(&self.state);
            for w in &mut snap.windows {
                // Percentage-based providers already carry their status.
                if !w.auto {
                    continue;
                }
                let key = format!("{}:{}", snap.id, w.key);
                // Historical peak BEFORE folding in the current value, so a new
                // all-time-high reads near 0% rather than pinning at the peak.
                let historical_peak = *self.state.observed_max.get(&key).unwrap_or(&0);
                let budget = historical_peak.max(budget_floor(&w.key));
                w.finalize(budget);
                // Now record the current value for future refreshes.
                if w.used_tokens > 0 {
                    self.state.observe(&key, w.used_tokens);
                }
            }
            snaps.push(snap);
        }

        self.state.save();

        let snapshot = Snapshot {
            updated_at: Local::now().to_rfc3339(),
            theme: self.state.theme.clone(),
            providers: snaps,
        };
        self.last = Some(snapshot.clone());
        snapshot
    }

    pub fn set_theme(&mut self, theme: &str) {
        self.state.theme = theme.to_string();
        self.state.save();
        if let Some(s) = self.last.as_mut() {
            s.theme = theme.to_string();
        }
    }

    pub fn set_copilot_token(&mut self, token: &str) {
        self.state.copilot_token = if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        };
        self.state.save();
    }
}
