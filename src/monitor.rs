use crate::model::{ProviderSnapshot, Snapshot, UsagePoint};
use crate::providers;
use crate::state::AppState;
use chrono::Local;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a provider's last successful snapshot is reused after a failure,
/// so fast polling doesn't flicker to "unavailable" on a transient blip.
const STALE_TTL: Duration = Duration::from_secs(300);

/// Single source of truth: runs every provider and produces a [`Snapshot`] for
/// the dashboard + tray icon.
pub struct QuotaMonitor {
    pub state: AppState,
    pub last: Option<Snapshot>,
    /// Last successful snapshot per provider id, with the time it was taken.
    last_good: HashMap<String, (ProviderSnapshot, Instant)>,
    /// When the last history sample was recorded (not persisted).
    last_history_sample: Option<Instant>,
}

impl QuotaMonitor {
    pub fn new() -> Self {
        let state = AppState::load();
        // Apply proxy once at startup so all ureq agents pick it up.
        providers::http::set_proxy(state.http_proxy.clone());
        Self {
            state,
            last: None,
            last_good: HashMap::new(),
            last_history_sample: None,
        }
    }

    /// Collect from all providers in parallel, ride out transient failures, cache
    /// the snapshot, and return it.
    pub fn refresh(&mut self) -> Snapshot {
        // Collect from every provider in parallel. Scoped threads borrow the
        // shared `&AppState` instead of each cloning it, so the credential
        // fields are not duplicated across N threads' memory during a refresh.
        let state = &self.state;
        let mut raw_results: HashMap<String, ProviderSnapshot> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = providers::all()
                    .into_iter()
                    .map(|provider| {
                        scope.spawn(move || {
                            let snap = provider.collect(state);
                            (provider.id().to_string(), snap)
                        })
                    })
                    .collect();

                let mut map = HashMap::new();
                for handle in handles {
                    if let Ok((id, snap)) = handle.join() {
                        map.insert(id, snap);
                    }
                }
                map
            });

        let mut snaps: Vec<ProviderSnapshot> = Vec::new();

        // Reconstruct display order. A provider that just failed keeps showing
        // its last good value for STALE_TTL so fast polling doesn't flicker.
        for provider in providers::all() {
            let id = provider.id();
            let fresh = raw_results.remove(id).unwrap_or_else(|| {
                ProviderSnapshot::unavailable(id, provider.name(), "Erro na recolha")
            });
            let snap = if fresh.available {
                self.last_good
                    .insert(id.to_string(), (fresh.clone(), Instant::now()));
                fresh
            } else {
                match self.last_good.get(id) {
                    Some((good, ts)) if ts.elapsed() < STALE_TTL => good.clone(),
                    _ => fresh,
                }
            };
            snaps.push(snap);
        }

        // Record a history point every 5 minutes.
        let record = match self.last_history_sample {
            None => true,
            Some(t) => t.elapsed() >= Duration::from_secs(300),
        };
        if record {
            let mut values = HashMap::new();
            for p in &snaps {
                if !p.available { continue; }
                for w in &p.windows {
                    values.insert(format!("{}:{}", p.id, w.key), w.remaining_pct);
                }
            }
            self.state.history.push(UsagePoint {
                at: Local::now().to_rfc3339(),
                values,
            });
            // Keep last 288 points (24 h at 5 min).
            if self.state.history.len() > 288 {
                let drain = self.state.history.len() - 288;
                self.state.history.drain(0..drain);
            }
            self.last_history_sample = Some(Instant::now());
        }

        // Build the history map for the dashboard sparklines (last 48 points).
        let mut history_map: HashMap<String, Vec<u32>> = HashMap::new();
        let tail = self.state.history.iter().rev().take(48).collect::<Vec<_>>();
        for point in tail.into_iter().rev() {
            for (key, &pct) in &point.values {
                history_map.entry(key.clone()).or_default().push(pct);
            }
        }

        let snapshot = Snapshot {
            updated_at: Local::now().to_rfc3339(),
            theme: self.state.theme.clone(),
            providers: snaps,
            history: history_map,
        };
        self.state.last_snapshot = Some(snapshot.clone());
        self.state.save();
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
        AppState::set_secret(&mut self.state.copilot_token, token);
        self.state.save();
    }

    pub fn set_openrouter_key(&mut self, key: &str) {
        AppState::set_secret(&mut self.state.openrouter_key, key);
        self.state.save();
    }

    pub fn set_gemini_key(&mut self, key: &str) {
        AppState::set_secret(&mut self.state.gemini_key, key);
        self.state.save();
    }

    pub fn set_http_proxy(&mut self, proxy: &str) {
        // `set_proxy` validates the URL (scheme/host) and ignores anything bogus;
        // mirror its decision so we never persist an invalid/empty proxy.
        let valid = !proxy.is_empty() && providers::http::is_valid_proxy(proxy);
        providers::http::set_proxy(if valid { Some(proxy.to_string()) } else { None });
        AppState::set_secret(&mut self.state.http_proxy, if valid { proxy } else { "" });
        self.state.save();
    }
}
