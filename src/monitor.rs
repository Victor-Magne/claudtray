use crate::model::{ProviderSnapshot, Snapshot};
use crate::providers;
use crate::state::AppState;
use chrono::Local;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a provider's last successful snapshot is reused after a failure,
/// so fast polling doesn't flicker to "unavailable" on a transient blip.
const STALE_TTL: Duration = Duration::from_secs(30);

/// Single source of truth: runs every provider and produces a [`Snapshot`] for
/// the dashboard + tray icon.
pub struct QuotaMonitor {
    pub state: AppState,
    pub last: Option<Snapshot>,
    /// Last successful snapshot per provider id, with the time it was taken.
    last_good: HashMap<String, (ProviderSnapshot, Instant)>,
}

impl QuotaMonitor {
    pub fn new() -> Self {
        Self {
            state: AppState::load(),
            last: None,
            last_good: HashMap::new(),
        }
    }

    /// Collect from all providers in parallel, ride out transient failures, cache
    /// the snapshot, and return it.
    pub fn refresh(&mut self) -> Snapshot {
        let state = self.state.clone();

        // Spawn collection for each provider in parallel to avoid blocking sequentially.
        let handles: Vec<_> = providers::all()
            .into_iter()
            .map(|provider| {
                let state = state.clone();
                std::thread::spawn(move || {
                    let snap = provider.collect(&state);
                    (provider.id().to_string(), snap)
                })
            })
            .collect();

        // Join threads and collect raw snapshots.
        let mut raw_results: HashMap<String, ProviderSnapshot> = HashMap::new();
        for handle in handles {
            if let Ok((id, snap)) = handle.join() {
                raw_results.insert(id, snap);
            }
        }

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

        let snapshot = Snapshot {
            updated_at: Local::now().to_rfc3339(),
            theme: self.state.theme.clone(),
            providers: snaps,
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
        self.state.copilot_token = if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        };
        self.state.save();
    }
}
