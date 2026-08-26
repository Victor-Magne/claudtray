use crate::i18n::Lang;
use crate::model::{ProviderInfo, ProviderSnapshot, Snapshot, UsagePoint};
use crate::providers;
use crate::state::AppState;
use chrono::{DateTime, Local};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a provider's last successful snapshot is reused after a failure,
/// so fast polling doesn't flicker to "unavailable" on a transient blip.
const STALE_TTL: Duration = Duration::from_secs(300);

/// Minimum gap between disk writes of `state.json` from the refresh path.
/// The in-memory snapshot is always fresh; only the persisted copy is
/// throttled, so a restart is at most this stale.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);

/// Single source of truth: runs every provider and produces a [`Snapshot`] for
/// the dashboard + tray icon.
pub struct QuotaMonitor {
    pub state: AppState,
    pub last: Option<Snapshot>,
    /// Last successful snapshot per provider id, with the time it was taken.
    last_good: HashMap<String, (ProviderSnapshot, Instant)>,
    /// When the last history sample was recorded (not persisted).
    last_history_sample: Option<Instant>,
    /// When `state.json` was last written to disk (throttles the refresh-path save).
    last_save: Option<Instant>,
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
            last_save: None,
        }
    }

    /// Persist `state.json`, but skip the write if the last one was within
    /// `SAVE_INTERVAL`. Called from the hot refresh path (every 5-60s); the
    /// in-memory snapshot (`self.last`) is updated unconditionally, so nothing
    /// user-visible depends on this write happening immediately.
    fn save_throttled(&mut self) {
        let due = self.last_save.is_none_or(|t| t.elapsed() >= SAVE_INTERVAL);
        if due {
            self.state.save();
            self.last_save = Some(Instant::now());
        }
    }

    /// Collect from all providers in parallel, ride out transient failures, cache
    /// the snapshot, and return it.
    pub fn refresh(&mut self) -> Snapshot {
        // Collect from every provider in parallel. Scoped threads borrow the
        // shared `&AppState` instead of each cloning it, so the credential
        // fields are not duplicated across N threads' memory during a refresh.
        let state = &self.state;
        // Hidden providers are skipped entirely: no collection thread (no API
        // calls) and no entry in the snapshot, so they don't affect the tray
        // icon or alerts. The full catalog still goes out for the settings UI.
        let disabled = self.state.disabled_providers.clone();
        let mut raw_results: HashMap<String, ProviderSnapshot> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = providers::all()
                    .into_iter()
                    .filter(|p| !disabled.iter().any(|d| d == p.id()))
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
            if disabled.iter().any(|d| d == id) {
                continue;
            }
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

        // Project each window's exhaustion time from its recent decline
        // (using history recorded so far, i.e. not counting this tick's
        // sample yet — that's added below).
        let now = Local::now();
        for snap in snaps.iter_mut() {
            if !snap.available {
                continue;
            }
            for w in snap.windows.iter_mut() {
                let key = format!("{}:{}", snap.id, w.key);
                w.estimated_exhaustion = estimate_exhaustion(
                    &self.state.history,
                    &key,
                    w.remaining_pct,
                    w.reset_at.as_deref(),
                    now,
                );
            }
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

        let catalog = providers::all()
            .iter()
            .map(|p| ProviderInfo {
                id: p.id().to_string(),
                name: p.name().to_string(),
                enabled: !disabled.iter().any(|d| d == p.id()),
            })
            .collect();

        let snapshot = Snapshot {
            updated_at: Local::now().to_rfc3339(),
            theme: self.state.theme.clone(),
            language: self.state.language.clone(),
            resolved_language: Lang::from_pref(&self.state.language).code().to_string(),
            providers: snaps,
            catalog,
            history: history_map,
        };
        self.state.last_snapshot = Some(snapshot.clone());
        self.save_throttled();
        self.last = Some(snapshot.clone());
        snapshot
    }

    /// Persist the set of hidden providers. Unknown ids are dropped so a stale
    /// or hand-edited list can't grow unbounded.
    pub fn set_disabled_providers(&mut self, ids: Vec<String>) {
        let known = providers::all();
        self.state.disabled_providers = ids
            .into_iter()
            .filter(|id| known.iter().any(|p| p.id() == id))
            .collect();
        self.state.save();
    }

    pub fn set_theme(&mut self, theme: &str) {
        self.state.theme = theme.to_string();
        self.state.save();
        if let Some(s) = self.last.as_mut() {
            s.theme = theme.to_string();
        }
    }

    pub fn set_language(&mut self, language: &str) {
        self.state.language = language.to_string();
        self.state.save();
        if let Some(s) = self.last.as_mut() {
            s.language = language.to_string();
            s.resolved_language = Lang::from_pref(language).code().to_string();
        }
    }

    pub fn set_claude_token(&mut self, token: &str) {
        AppState::set_secret(&mut self.state.claude_token, token);
        self.state.save();
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

/// Project when a window (identified by `"{provider_id}:{window_key}"`) will
/// hit 0%, from its recent decline in `history`. Looks at the longest
/// trailing run of non-increasing samples (i.e. since the last reset, without
/// needing to detect the reset explicitly), requires at least 15 minutes of
/// signal, and never projects past the window's own `reset_at` — a window
/// that resets before it would exhaust has no ETA to show.
///
/// ponytail: linear extrapolation over the recent trend, not a real usage
/// model — good enough for "should I worry", revisit if it proves noisy.
fn estimate_exhaustion(
    history: &[UsagePoint],
    key: &str,
    remaining_pct: u32,
    reset_at: Option<&str>,
    now: DateTime<Local>,
) -> Option<String> {
    let mut series: Vec<(DateTime<Local>, u32)> = history
        .iter()
        .filter_map(|p| {
            let pct = *p.values.get(key)?;
            let at = DateTime::parse_from_rfc3339(&p.at).ok()?.with_timezone(&Local);
            Some((at, pct))
        })
        .collect();
    series.push((now, remaining_pct));
    if series.len() < 2 {
        return None;
    }

    // Walk back from "now" while the series is non-increasing (1pt of noise
    // tolerance) to isolate the current burn window.
    let mut start = series.len() - 1;
    while start > 0 && series[start - 1].1 + 1 >= series[start].1 {
        start -= 1;
    }
    let (first_at, first_pct) = series[start];
    let (last_at, last_pct) = series[series.len() - 1];

    let minutes = (last_at - first_at).num_minutes() as f64;
    if minutes < 15.0 {
        return None;
    }
    let drop = first_pct as f64 - last_pct as f64;
    if drop <= 1.0 {
        // Flat, or within the noise tolerance used to build the run above.
        return None;
    }
    let rate_per_min = drop / minutes;
    let eta_minutes = last_pct as f64 / rate_per_min;
    let eta = last_at + chrono::Duration::minutes(eta_minutes.round() as i64);

    if let Some(reset_str) = reset_at {
        if let Ok(reset) = DateTime::parse_from_rfc3339(reset_str) {
            if eta >= reset.with_timezone(&Local) {
                return None;
            }
        }
    }
    Some(eta.to_rfc3339())
}

#[cfg(test)]
mod exhaustion_tests {
    use super::*;

    fn point(minutes_ago: i64, key: &str, pct: u32, now: DateTime<Local>) -> UsagePoint {
        let mut values = HashMap::new();
        values.insert(key.to_string(), pct);
        UsagePoint {
            at: (now - chrono::Duration::minutes(minutes_ago)).to_rfc3339(),
            values,
        }
    }

    #[test]
    fn steady_decline_projects_an_eta_before_reset() {
        let now = Local::now();
        let history = vec![
            point(60, "claude:session", 80, now),
            point(30, "claude:session", 60, now),
        ];
        let reset = (now + chrono::Duration::hours(5)).to_rfc3339();
        let eta = estimate_exhaustion(&history, "claude:session", 40, Some(&reset), now);
        assert!(eta.is_some(), "steady decline should produce an ETA");
    }

    #[test]
    fn flat_history_has_no_eta() {
        let now = Local::now();
        let history = vec![
            point(60, "claude:session", 80, now),
            point(30, "claude:session", 82, now),
        ];
        let eta = estimate_exhaustion(&history, "claude:session", 81, None, now);
        assert!(eta.is_none(), "flat/increasing usage shouldn't project an ETA");
    }

    #[test]
    fn eta_past_reset_is_suppressed() {
        let now = Local::now();
        // Very slow decline: 1pt per 30 minutes → hours from exhaustion.
        let history = vec![
            point(60, "claude:session", 50, now),
            point(30, "claude:session", 49, now),
        ];
        let reset = (now + chrono::Duration::minutes(10)).to_rfc3339();
        let eta = estimate_exhaustion(&history, "claude:session", 48, Some(&reset), now);
        assert!(eta.is_none(), "resets before exhaustion, so no ETA should show");
    }
}
