use crate::i18n::Lang;
use crate::model::{ProviderInfo, ProviderSnapshot, Snapshot, UsagePoint};
use crate::providers::{self, Provider};
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

/// After a failed collection, retry sooner than a long `min_interval` (e.g.
/// Gemini's 10 min) would allow, so a transient blip doesn't hide the provider
/// for that long. Still far gentler than the 5 s tick.
const FAIL_RETRY: Duration = Duration::from_secs(30);

/// Decides which providers to collect on a tick and what to show for the rest.
#[derive(Default)]
struct Collector {
    /// Last raw collection result per provider id, with when it was attempted.
    last_try: HashMap<String, (ProviderSnapshot, Instant)>,
    /// Last successful snapshot per provider id, with the time it was taken.
    last_good: HashMap<String, (ProviderSnapshot, Instant)>,
}

impl Collector {
    /// A provider is due when it was never collected, or its last attempt is
    /// older than `min_interval` (`FAIL_RETRY` at most if that attempt failed).
    fn is_due(&self, id: &str, interval: Duration, now: Instant) -> bool {
        match self.last_try.get(id) {
            None => true,
            Some((snap, at)) => {
                let wait = if snap.available { interval } else { interval.min(FAIL_RETRY) };
                now.saturating_duration_since(*at) >= wait
            }
        }
    }

    /// One tick: collect the due (or all, if `force`) enabled providers in
    /// parallel, reuse the cached result for the rest, and return the display
    /// list in provider order. `now` is injected so tests control the clock.
    fn round(
        &mut self,
        all: &[Box<dyn Provider + Send + Sync>],
        state: &AppState,
        force: bool,
        now: Instant,
    ) -> Vec<ProviderSnapshot> {
        // Hidden providers are skipped entirely: no collection thread (no API
        // calls) and no entry in the snapshot, so they don't affect the tray
        // icon or alerts. The full catalog still goes out for the settings UI.
        let enabled: Vec<_> = all
            .iter()
            .filter(|p| !state.disabled_providers.iter().any(|d| d == p.id()))
            .collect();
        let due: Vec<bool> = enabled
            .iter()
            .map(|p| force || self.is_due(p.id(), p.min_interval(), now))
            .collect();

        // Scoped threads borrow the shared `&AppState` instead of each cloning
        // it, so the credential fields are not duplicated across N threads.
        let mut raw: HashMap<String, ProviderSnapshot> = std::thread::scope(|scope| {
            let handles: Vec<_> = enabled
                .iter()
                .zip(&due)
                .filter(|(_, due)| **due)
                .map(|(provider, _)| {
                    scope.spawn(move || (provider.id().to_string(), provider.collect(state)))
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });

        let mut snaps = Vec::with_capacity(enabled.len());
        // A provider that just failed keeps showing its last good value for
        // STALE_TTL so fast polling doesn't flicker.
        for (provider, due) in enabled.iter().zip(due) {
            let id = provider.id();
            let unavailable =
                || ProviderSnapshot::unavailable(id, provider.name(), "Erro na recolha");
            let fresh = match raw.remove(id) {
                Some(snap) => snap,
                None if due => unavailable(), // collector thread panicked
                None => self.last_try.get(id).map_or_else(unavailable, |(s, _)| s.clone()),
            };
            if due {
                self.last_try.insert(id.to_string(), (fresh.clone(), now));
                if fresh.available {
                    self.last_good.insert(id.to_string(), (fresh.clone(), now));
                }
            }
            let snap = if fresh.available {
                fresh
            } else {
                match self.last_good.get(id) {
                    // A rejected token is not a blip: never mask it with old numbers.
                    Some((good, ts))
                        if !fresh.auth_failed
                            && now.saturating_duration_since(*ts) < STALE_TTL =>
                    {
                        let mut good = good.clone();
                        good.stale_secs = Some(now.saturating_duration_since(*ts).as_secs());
                        good
                    }
                    _ => fresh,
                }
            };
            snaps.push(snap);
        }
        snaps
    }
}

/// Single source of truth: runs every provider and produces a [`Snapshot`] for
/// the dashboard + tray icon.
pub struct QuotaMonitor {
    pub state: AppState,
    pub last: Option<Snapshot>,
    collector: Collector,
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
            collector: Collector::default(),
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

    /// Periodic refresh (the ticker): only providers whose `min_interval` has
    /// elapsed are collected; the others reuse their last snapshot as-is.
    pub fn refresh(&mut self) -> Snapshot {
        self.refresh_with(false)
    }

    /// Refresh every provider now, ignoring `min_interval`. For explicit user
    /// actions (manual refresh, new credential).
    pub fn refresh_now(&mut self) -> Snapshot {
        self.refresh_with(true)
    }

    /// Collect from the due providers in parallel, ride out transient failures,
    /// cache the snapshot, and return it.
    fn refresh_with(&mut self, force: bool) -> Snapshot {
        let all = providers::all();
        let mut snaps = self.collector.round(&all, &self.state, force, Instant::now());
        let disabled = self.state.disabled_providers.clone();

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

        let catalog = all
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

#[cfg(test)]
mod ttl_tests {
    use super::*;
    use crate::model::WindowUsage;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy)]
    enum Mode {
        Ok,
        Fail,
        Auth,
    }

    struct Fake {
        interval: Duration,
        calls: Arc<AtomicUsize>,
        mode: Arc<Mutex<Mode>>,
    }

    impl Provider for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn name(&self) -> &'static str {
            "Fake"
        }
        fn min_interval(&self) -> Duration {
            self.interval
        }
        fn collect(&self, _: &AppState) -> ProviderSnapshot {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match *self.mode.lock().unwrap() {
                Mode::Ok => {
                    let mut s = ProviderSnapshot::unavailable("fake", "Fake", "");
                    s.available = true;
                    s.windows = vec![WindowUsage::from_percent("session", "SESSION", 80, None)];
                    s
                }
                Mode::Fail => ProviderSnapshot::unavailable("fake", "Fake", "down"),
                Mode::Auth => ProviderSnapshot::signed_out("fake", "Fake", "401"),
            }
        }
    }

    type Rig = (
        Vec<Box<dyn Provider + Send + Sync>>,
        Arc<AtomicUsize>,
        Arc<Mutex<Mode>>,
    );

    /// One fake provider plus handles to count its calls and flip its outcome.
    fn fake(secs: u64) -> Rig {
        let calls = Arc::new(AtomicUsize::new(0));
        let mode = Arc::new(Mutex::new(Mode::Ok));
        let p = Fake {
            interval: Duration::from_secs(secs),
            calls: calls.clone(),
            mode: mode.clone(),
        };
        (vec![Box::new(p)], calls, mode)
    }

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn five_minutes_of_5s_ticks_collect_a_60s_provider_at_most_5_times() {
        let (all, calls, _) = fake(60);
        let (state, mut c, base) = (AppState::default(), Collector::default(), Instant::now());
        for tick in 0..60 {
            let snaps = c.round(&all, &state, false, at(base, tick * 5));
            // Reusing the last good snapshot must not look stale.
            assert!(snaps[0].available && snaps[0].stale_secs.is_none());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn local_provider_is_collected_on_every_5s_tick() {
        let (all, calls, _) = fake(4);
        let (state, mut c, base) = (AppState::default(), Collector::default(), Instant::now());
        for tick in 0..60 {
            c.round(&all, &state, false, at(base, tick * 5));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 60);
    }

    #[test]
    fn force_ignores_the_interval() {
        let (all, calls, _) = fake(600);
        let (state, mut c, base) = (AppState::default(), Collector::default(), Instant::now());
        c.round(&all, &state, false, base);
        c.round(&all, &state, false, at(base, 5));
        c.round(&all, &state, true, at(base, 10));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failure_serves_stale_and_retries_every_30s_not_every_tick() {
        let (all, calls, mode) = fake(60);
        let (state, mut c, base) = (AppState::default(), Collector::default(), Instant::now());
        c.round(&all, &state, false, base);
        *mode.lock().unwrap() = Mode::Fail;
        // t=0 ok; t=60 fails; t=65..85 reuse the cached failure (no new call).
        for tick in 12..18 {
            let snaps = c.round(&all, &state, false, at(base, tick * 5));
            assert!(snaps[0].available, "last good is shown after a failure");
            assert_eq!(snaps[0].stale_secs, Some(tick * 5));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // 30 s after the failure (not the 60 s interval) it is retried.
        c.round(&all, &state, false, at(base, 90));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        // The stale cache still expires after STALE_TTL.
        let snaps = c.round(&all, &state, false, at(base, 301));
        assert!(!snaps[0].available);
    }

    #[test]
    fn auth_failure_is_never_masked_by_the_cache() {
        let (all, _, mode) = fake(60);
        let (state, mut c, base) = (AppState::default(), Collector::default(), Instant::now());
        c.round(&all, &state, false, base);
        *mode.lock().unwrap() = Mode::Auth;
        let snaps = c.round(&all, &state, false, at(base, 60));
        assert!(!snaps[0].available && snaps[0].auth_failed);
        // Still flagged on the ticks that reuse the cached 401.
        let snaps = c.round(&all, &state, false, at(base, 65));
        assert!(!snaps[0].available && snaps[0].auth_failed);
    }
}
