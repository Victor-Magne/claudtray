use crate::i18n::Lang;
use crate::model::{ProviderInfo, ProviderSnapshot, Snapshot, UsagePoint, WindowUsage};
use crate::providers::{self, Provider};
use crate::state::AppState;
use chrono::{DateTime, Local};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

type BoxProvider = Arc<dyn Provider + Send + Sync>;

/// How long a provider's last successful snapshot is reused after a failure,
/// so fast polling doesn't flicker to "unavailable" on a transient blip.
const STALE_TTL: Duration = Duration::from_secs(300);

/// Minimum gap between disk writes of `state.json` from the refresh path.
/// The in-memory snapshot is always fresh; only the persisted copy is
/// throttled, so a restart is at most this stale.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);

/// First retry after a failed collection, sooner than a long `min_interval`
/// (e.g. Gemini's 10 min) would allow, so a transient blip doesn't hide the
/// provider for that long. Each further consecutive failure doubles it
/// (30 s, 1, 2, 4, 8 min) up to `MAX_BACKOFF`; a success resets it. Providers
/// whose `min_interval` is below it are local and not backed off.
const FAIL_RETRY: Duration = Duration::from_secs(30);

/// Ceiling of the failure backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// Ceiling of a server-provided `Retry-After`, so a bogus value cannot park a
/// provider for days.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

/// Wait after the `fails`-th consecutive failure: `FAIL_RETRY * 2^(fails-1)`.
fn backoff(fails: u32) -> Duration {
    FAIL_RETRY
        .saturating_mul(2u32.saturating_pow(fails.saturating_sub(1).min(16)))
        .min(MAX_BACKOFF)
}

/// Per-provider retry schedule.
struct Retry {
    /// Consecutive failed collections (auth failures excluded); 0 after a success.
    fails: u32,
    /// Earliest moment the provider may be collected again (unless forced).
    next: Instant,
}

/// How long one tick waits for a provider. Past it the provider is treated as
/// failed for this tick (its last good value stays, marked stale) and the UI
/// is not held up any longer. Above the 5 s HTTP timeout of a single attempt.
const PROVIDER_DEADLINE: Duration = Duration::from_secs(8);

/// Marks a provider's `collect` as running; cleared on drop (also on panic).
struct RunGuard(Arc<AtomicBool>);

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Decides which providers to collect on a tick and what to show for the rest.
struct Collector {
    /// Last raw collection result per provider id, with when it was attempted.
    last_try: HashMap<String, (ProviderSnapshot, Instant)>,
    /// Last successful snapshot per provider id, with the time it was taken.
    last_good: HashMap<String, (ProviderSnapshot, Instant)>,
    /// Providers whose `collect` is still running, possibly past its deadline.
    /// A new one is never started meanwhile, so a hung provider cannot pile up
    /// threads.
    running: HashMap<String, Arc<AtomicBool>>,
    /// When each provider may next be collected, and how many failures in a row.
    retry: HashMap<String, Retry>,
    /// Shown for a provider that has no result yet (last run's snapshot at startup).
    seed: HashMap<String, ProviderSnapshot>,
    deadline: Duration,
}

impl Default for Collector {
    fn default() -> Self {
        Self {
            last_try: HashMap::new(),
            last_good: HashMap::new(),
            running: HashMap::new(),
            retry: HashMap::new(),
            seed: HashMap::new(),
            deadline: PROVIDER_DEADLINE,
        }
    }
}

impl Collector {
    /// A provider is due when it was never collected, or its schedule (set by
    /// `settle`) has come up.
    fn is_due(&self, id: &str, now: Instant) -> bool {
        self.retry.get(id).is_none_or(|r| now >= r.next)
    }

    /// What to display for `id` given its latest raw result `fresh`. A provider
    /// that failed keeps showing its last good value for STALE_TTL so fast
    /// polling doesn't flicker.
    fn resolve(&self, id: &str, mut fresh: ProviderSnapshot, now: Instant) -> ProviderSnapshot {
        // Whatever the provider (or a persisted seed) said, the monitor owns this.
        fresh.retry_in_secs = None;
        if fresh.available {
            return fresh;
        }
        let retry_in = self
            .retry
            .get(id)
            .and_then(|r| r.next.checked_duration_since(now))
            .map(|d| d.as_secs());
        let mut shown = match self.last_good.get(id) {
            // A rejected token is not a blip: never mask it with old numbers.
            Some((good, ts))
                if !fresh.auth_failed && now.saturating_duration_since(*ts) < STALE_TTL =>
            {
                let mut good = good.clone();
                good.stale_secs = Some(now.saturating_duration_since(*ts).as_secs());
                good
            }
            _ => fresh,
        };
        shown.retry_in_secs = retry_in;
        shown
    }

    /// Display value from the cache alone (no collection in flight for it).
    fn cached(&self, p: &BoxProvider, now: Instant) -> ProviderSnapshot {
        let id = p.id();
        let fresh = match self.last_try.get(id) {
            Some((snap, _)) => snap.clone(),
            None => self.seed.get(id).cloned().unwrap_or_else(|| {
                ProviderSnapshot::unavailable(id, p.name(), "A carregar…")
            }),
        };
        self.resolve(id, fresh, now)
    }

    /// Record a collection result, schedule the provider's next attempt and
    /// return what to display for it. After a failure the wait is the failure
    /// backoff, or the server's `Retry-After` (capped) when that is longer; a
    /// rejected token waits the normal `interval` (it is not a blip, but the
    /// endpoint must not be hit more often than usual); a local provider
    /// (`interval` under `FAIL_RETRY`) is simply retried at its `interval`.
    fn settle(
        &mut self,
        id: &str,
        interval: Duration,
        mut fresh: ProviderSnapshot,
        now: Instant,
    ) -> ProviderSnapshot {
        let server = fresh.retry_in_secs.take().map(|s| Duration::from_secs(s).min(MAX_RETRY_AFTER));
        let r = self
            .retry
            .entry(id.to_string())
            .or_insert(Retry { fails: 0, next: now });
        let wait = if fresh.available {
            r.fails = 0;
            interval
        } else if fresh.auth_failed || interval < FAIL_RETRY {
            interval
        } else {
            r.fails += 1;
            backoff(r.fails)
        };
        r.next = now + wait.max(server.filter(|_| !fresh.available).unwrap_or_default());
        if fresh.available {
            // A provider may serve a shared-cache value (Claude) it flags with
            // `stale_secs`: the data is that old, not as old as this collect.
            let age = chrono::Duration::seconds(fresh.stale_secs.unwrap_or(0).min(31_536_000) as i64);
            fresh.fetched_at = Some((Local::now() - age).to_rfc3339());
        }
        self.last_try.insert(id.to_string(), (fresh.clone(), now));
        if fresh.available {
            self.last_good.insert(id.to_string(), (fresh.clone(), now));
        }
        self.resolve(id, fresh, now)
    }

    /// One tick: start the due (or all, if `force`) enabled providers in
    /// parallel and call `on_update(display list, done)` each time a provider
    /// finishes (the others keep their cached value), the last call with
    /// `done`; if nothing is due, once from the cache. Providers still running
    /// after `deadline` count as failed. The list is in provider order. `now`
    /// is injected so tests control the cache clock.
    fn round(
        &mut self,
        all: &[BoxProvider],
        state: &Arc<AppState>,
        force: bool,
        now: Instant,
        on_update: &mut dyn FnMut(&[ProviderSnapshot], bool),
    ) {
        // Hidden providers are skipped entirely: no collection thread (no API
        // calls) and no entry in the snapshot, so they don't affect the tray
        // icon or alerts. The full catalog still goes out for the settings UI.
        let enabled: Vec<&BoxProvider> = all
            .iter()
            .filter(|p| !state.disabled_providers.iter().any(|d| d == p.id()))
            .collect();
        let mut shown: Vec<ProviderSnapshot> =
            enabled.iter().map(|p| self.cached(p, now)).collect();

        let (tx, rx) = mpsc::channel();
        let mut waiting = vec![false; enabled.len()];
        for (i, p) in enabled.iter().enumerate() {
            let due = force || self.is_due(p.id(), now);
            let busy = self.running.entry(p.id().to_string()).or_default();
            if !due || busy.swap(true, Ordering::SeqCst) {
                continue;
            }
            let (p, state, tx) = (Arc::clone(p), Arc::clone(state), tx.clone());
            let guard = RunGuard(Arc::clone(busy));
            std::thread::spawn(move || {
                let snap = p.collect(&state);
                drop(guard); // free before the result is visible, so the next tick can reuse it
                let _ = tx.send((i, snap));
            });
            waiting[i] = true;
        }
        drop(tx); // only the collector threads hold senders now

        let mut left = waiting.iter().filter(|w| **w).count();
        if left == 0 {
            on_update(&shown, true);
            return;
        }
        let end = Instant::now() + self.deadline;
        while left > 0 {
            match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                Ok((i, snap)) => {
                    shown[i] = self.settle(enabled[i].id(), enabled[i].min_interval(), snap, now);
                    waiting[i] = false;
                    left -= 1;
                }
                // Deadline passed, or the remaining threads panicked (all senders gone).
                Err(e) => {
                    let why = match e {
                        mpsc::RecvTimeoutError::Timeout => "Tempo excedido",
                        mpsc::RecvTimeoutError::Disconnected => "Erro na recolha",
                    };
                    for (i, p) in enabled.iter().enumerate().filter(|(i, _)| waiting[*i]) {
                        let failed = ProviderSnapshot::unavailable(p.id(), p.name(), why);
                        shown[i] = self.settle(p.id(), p.min_interval(), failed, now);
                    }
                    left = 0;
                }
            }
            on_update(&shown, left == 0);
        }
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
        // Until a provider answers, show what the last run saw instead of a blank.
        let mut collector = Collector::default();
        if let Some(last) = &state.last_snapshot {
            collector.seed = last.providers.iter().map(|p| (p.id.clone(), p.clone())).collect();
        }
        Self {
            state,
            last: None,
            collector,
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
    /// `emit` gets an updated snapshot each time a provider finishes; the last
    /// one is the complete round.
    pub fn refresh(&mut self, emit: impl FnMut(Snapshot)) {
        self.refresh_with(false, emit)
    }

    /// Refresh every provider now, ignoring `min_interval`. For explicit user
    /// actions (manual refresh, new credential).
    pub fn refresh_now(&mut self, emit: impl FnMut(Snapshot)) {
        self.refresh_with(true, emit)
    }

    /// Collect from the due providers in parallel and `emit` a snapshot as each
    /// one lands, so a slow provider never holds back the others.
    fn refresh_with(&mut self, force: bool, mut emit: impl FnMut(Snapshot)) {
        let all = providers::all();
        let catalog: Vec<ProviderInfo> = all
            .iter()
            .map(|p| ProviderInfo {
                id: p.id().to_string(),
                name: p.name().to_string(),
                enabled: !self.state.disabled_providers.iter().any(|d| d == p.id()),
            })
            .collect();
        // The collector is taken out so the callback can borrow the rest of `self`.
        let mut collector = std::mem::take(&mut self.collector);
        let state = Arc::new(self.state.clone());
        collector.round(&all, &state, force, Instant::now(), &mut |snaps, done| {
            emit(self.assemble(snaps.to_vec(), catalog.clone(), done));
        });
        self.collector = collector;
    }

    /// Turn the display list into the dashboard [`Snapshot`]: pace and exhaustion
    /// projection per window, history sample (once per round, when it is `done`)
    /// and sparklines. Also caches it as `last` / `state.last_snapshot`.
    fn assemble(
        &mut self,
        mut snaps: Vec<ProviderSnapshot>,
        catalog: Vec<ProviderInfo>,
        done: bool,
    ) -> Snapshot {
        // Project each window's exhaustion time from its recent decline
        // (using history recorded so far, i.e. not counting this tick's
        // sample yet — that's added below).
        let now = Local::now();
        for snap in snaps.iter_mut() {
            if !snap.available {
                continue;
            }
            for w in snap.windows.iter_mut() {
                w.pace_pct = pace(w, now);
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
        let record = done
            && match self.last_history_sample {
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
/// Pace of a window with a known length: share of it elapsed minus share used,
/// in percentage points (positive = under pace). `None` when the length or reset
/// is unknown, the reset already passed, or the window is under 3% elapsed.
fn pace(w: &WindowUsage, now: DateTime<Local>) -> Option<i32> {
    let len = w.window_secs? as f64;
    let reset = DateTime::parse_from_rfc3339(w.reset_at.as_deref()?).ok()?;
    let left = (reset.with_timezone(&Local) - now).num_seconds() as f64;
    let elapsed = 1.0 - left / len;
    if !(0.03..=1.0).contains(&elapsed) {
        return None;
    }
    Some(((elapsed * 100.0) - (100 - w.remaining_pct) as f64).round() as i32)
}

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

    fn paced(len_secs: u64, left_secs: i64, remaining: u32) -> Option<i32> {
        let now = Local::now();
        let mut w = WindowUsage::from_percent(
            "weekly",
            "WEEKLY",
            remaining,
            Some((now + chrono::Duration::seconds(left_secs)).to_rfc3339()),
        );
        w.window_secs = Some(len_secs);
        pace(&w, now)
    }

    #[test]
    fn pace_is_elapsed_minus_used() {
        let week = 7 * 24 * 3600;
        // Resets in 2d 3h 28m: ~69% elapsed, 39% used => 30 points under pace (the
        // ClaudeBar screenshot reads "30% under pace" for these same numbers).
        let left = 2 * 86_400 + 3 * 3600 + 28 * 60;
        assert_eq!(paced(week, left, 61), Some(30));
        // Burned 80% with half the week left => 30 points over (running hot).
        assert_eq!(paced(week, week as i64 / 2, 20), Some(-30));
    }

    #[test]
    fn pace_waits_for_the_window_to_get_going() {
        let five_h = 5 * 3600;
        // 2 minutes into a 5-hour window (<3%): no pace, whatever the usage.
        assert_eq!(paced(five_h, five_h as i64 - 120, 90), None);
        // No window length, or a reset already in the past: no pace either.
        let w = WindowUsage::from_percent("m", "M", 50, None);
        assert_eq!(pace(&w, Local::now()), None);
        assert_eq!(paced(five_h, -60, 50), None);
    }

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
        /// Fails with a server `Retry-After` of this many seconds.
        Throttle(u64),
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
                Mode::Throttle(s) => {
                    ProviderSnapshot::unavailable("fake", "Fake", "429").retry_in(Some(s))
                }
            }
        }
    }

    type Rig = (
        Vec<BoxProvider>,
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
        (vec![Arc::new(p)], calls, mode)
    }

    /// One round, returning the final display list (the callback's last call).
    fn run(
        c: &mut Collector,
        all: &[BoxProvider],
        state: &Arc<AppState>,
        force: bool,
        now: Instant,
    ) -> Vec<ProviderSnapshot> {
        let mut last = Vec::new();
        c.round(all, state, force, now, &mut |snaps, _| last = snaps.to_vec());
        last
    }

    /// Fake provider that takes a configurable time to answer.
    struct Timed {
        id: &'static str,
        delay: Arc<Mutex<Duration>>,
        calls: Arc<AtomicUsize>,
    }

    impl Provider for Timed {
        fn id(&self) -> &'static str {
            self.id
        }
        fn name(&self) -> &'static str {
            self.id
        }
        fn min_interval(&self) -> Duration {
            Duration::from_secs(60)
        }
        fn collect(&self, _: &AppState) -> ProviderSnapshot {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(*self.delay.lock().unwrap());
            let mut s = ProviderSnapshot::unavailable(self.id, self.id, "");
            s.available = true;
            s
        }
    }

    fn timed(id: &'static str, ms: u64) -> (BoxProvider, Arc<Mutex<Duration>>, Arc<AtomicUsize>) {
        let delay = Arc::new(Mutex::new(Duration::from_millis(ms)));
        let calls = Arc::new(AtomicUsize::new(0));
        let p = Timed { id, delay: delay.clone(), calls: calls.clone() };
        (Arc::new(p), delay, calls)
    }

    fn ids(snaps: &[ProviderSnapshot]) -> Vec<&str> {
        snaps.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn slow_provider_does_not_delay_the_others() {
        // "slow" stands in for the provider that blocks for seconds.
        let (slow, ..) = timed("slow", 600);
        let (fast, ..) = timed("fast", 0);
        let all = vec![slow, fast];
        let (state, mut c) = (Arc::new(AppState::default()), Collector::default());
        let start = std::time::Instant::now();
        let mut updates: Vec<(Duration, Vec<ProviderSnapshot>, bool)> = Vec::new();
        c.round(&all, &state, false, Instant::now(), &mut |snaps, done| {
            updates.push((start.elapsed(), snaps.to_vec(), done));
        });

        assert_eq!(updates.len(), 2, "one update per provider");
        // The fast provider is painted long before the slow one answers, and
        // the partial list keeps the display order with the slow one pending.
        let (at, partial, done) = &updates[0];
        assert!(*at < Duration::from_millis(300), "partial arrived after {at:?}");
        assert!(!done);
        assert_eq!(ids(partial), ["slow", "fast"]);
        assert!(!partial[0].available && partial[1].available);
        // The round only completes once the slow one lands, with both filled in.
        let (at, full, done) = &updates[1];
        assert!(*at >= Duration::from_millis(600) && *done);
        assert_eq!(ids(full), ["slow", "fast"]);
        assert!(full.iter().all(|s| s.available));
    }

    #[test]
    fn partial_snapshot_shows_the_cached_value_of_providers_still_running() {
        let (slow, delay, _) = timed("slow", 0);
        let (fast, ..) = timed("fast", 0);
        let all = vec![slow, fast];
        let (state, mut c, base) = (Arc::new(AppState::default()), Collector::default(), Instant::now());
        run(&mut c, &all, &state, false, base);
        *delay.lock().unwrap() = Duration::from_millis(300);
        let mut first = None;
        c.round(&all, &state, true, at(base, 10), &mut |snaps, _| {
            first.get_or_insert_with(|| snaps.to_vec());
        });
        let first = first.unwrap();
        // "fast" finishes first; "slow" still shows last round's value, in its slot.
        assert_eq!(ids(&first), ["slow", "fast"]);
        assert!(first[0].available && first[1].available);
    }

    #[test]
    fn fetched_at_is_set_on_success_and_survives_a_failure() {
        let (all, _, mode) = fake(60);
        let (state, mut c, base) = (Arc::new(AppState::default()), Collector::default(), Instant::now());
        let ok = run(&mut c, &all, &state, false, base);
        let stamp = ok[0].fetched_at.clone().expect("set when a collect succeeds");
        assert!(DateTime::parse_from_rfc3339(&stamp).is_ok());
        // Ticks with nothing due show it from the cache too.
        let cached = run(&mut c, &all, &state, false, at(base, 1));
        assert_eq!(cached[0].fetched_at.as_ref(), Some(&stamp));

        *mode.lock().unwrap() = Mode::Fail;
        let shown = run(&mut c, &all, &state, true, at(base, 10));
        assert_eq!((shown[0].stale_secs, shown[0].fetched_at.as_ref()), (Some(10), Some(&stamp)));

        // Never succeeded: nothing to show.
        let (all, _, mode) = fake(60);
        *mode.lock().unwrap() = Mode::Fail;
        let shown = run(&mut Collector::default(), &all, &state, false, base);
        assert!(shown[0].fetched_at.is_none());
    }

    #[test]
    fn deadline_keeps_the_previous_snapshot_marked_stale() {
        let (slow, delay, calls) = timed("slow", 0);
        let all = vec![slow];
        let (state, mut c, base) = (Arc::new(AppState::default()), Collector::default(), Instant::now());
        c.deadline = Duration::from_millis(100);
        run(&mut c, &all, &state, false, base);

        *delay.lock().unwrap() = Duration::from_millis(600);
        let start = std::time::Instant::now();
        let snaps = run(&mut c, &all, &state, true, at(base, 10));
        assert!(start.elapsed() < Duration::from_millis(400), "round waited past the deadline");
        assert!(snaps[0].available, "previous snapshot is kept");
        assert_eq!(snaps[0].stale_secs, Some(10));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // The late thread is still running: not started a second time.
        run(&mut c, &all, &state, true, at(base, 11));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Once it has finished the provider is collected again.
        std::thread::sleep(Duration::from_millis(600));
        run(&mut c, &all, &state, true, at(base, 12));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn deadline_without_a_previous_value_is_a_failure_not_a_hang() {
        let (slow, ..) = timed("slow", 600);
        let all = vec![slow];
        let (state, mut c) = (Arc::new(AppState::default()), Collector::default());
        c.deadline = Duration::from_millis(100);
        let snaps = run(&mut c, &all, &state, false, Instant::now());
        assert!(!snaps[0].available && snaps[0].stale_secs.is_none());
    }

    #[test]
    fn nothing_due_still_emits_one_complete_update() {
        let (all, calls, _) = fake(60);
        let (state, mut c, base) = (Arc::new(AppState::default()), Collector::default(), Instant::now());
        run(&mut c, &all, &state, false, base);
        let mut updates = Vec::new();
        c.round(&all, &state, false, at(base, 5), &mut |snaps, done| {
            updates.push((snaps.len(), done));
        });
        assert_eq!(updates, [(1, true)]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn five_minutes_of_5s_ticks_collect_a_60s_provider_at_most_5_times() {
        let (all, calls, _) = fake(60);
        let (state, mut c, base) = (Arc::new(AppState::default()), Collector::default(), Instant::now());
        for tick in 0..60 {
            let snaps = run(&mut c, &all, &state, false, at(base, tick * 5));
            // Reusing the last good snapshot must not look stale.
            assert!(snaps[0].available && snaps[0].stale_secs.is_none());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn local_provider_is_collected_on_every_5s_tick() {
        let (all, calls, _) = fake(4);
        let (state, mut c, base) = (Arc::new(AppState::default()), Collector::default(), Instant::now());
        for tick in 0..60 {
            run(&mut c, &all, &state, false, at(base, tick * 5));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 60);
    }

    #[test]
    fn force_ignores_the_interval() {
        let (all, calls, _) = fake(600);
        let (state, mut c, base) = (Arc::new(AppState::default()), Collector::default(), Instant::now());
        run(&mut c, &all, &state, false, base);
        run(&mut c, &all, &state, false, at(base, 5));
        run(&mut c, &all, &state, true, at(base, 10));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failure_serves_stale_and_retries_every_30s_not_every_tick() {
        let (all, calls, mode) = fake(60);
        let (state, mut c, base) = (Arc::new(AppState::default()), Collector::default(), Instant::now());
        run(&mut c, &all, &state, false, base);
        *mode.lock().unwrap() = Mode::Fail;
        // t=0 ok; t=60 fails; t=65..85 reuse the cached failure (no new call).
        for tick in 12..18 {
            let snaps = run(&mut c, &all, &state, false, at(base, tick * 5));
            assert!(snaps[0].available, "last good is shown after a failure");
            assert_eq!(snaps[0].stale_secs, Some(tick * 5));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // 30 s after the failure (not the 60 s interval) it is retried.
        run(&mut c, &all, &state, false, at(base, 90));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        // The stale cache still expires after STALE_TTL.
        let snaps = run(&mut c, &all, &state, false, at(base, 301));
        assert!(!snaps[0].available);
    }

    #[test]
    fn auth_failure_is_never_masked_by_the_cache() {
        let (all, _, mode) = fake(60);
        let (state, mut c, base) = (Arc::new(AppState::default()), Collector::default(), Instant::now());
        run(&mut c, &all, &state, false, base);
        *mode.lock().unwrap() = Mode::Auth;
        let snaps = run(&mut c, &all, &state, false, at(base, 60));
        assert!(!snaps[0].available && snaps[0].auth_failed);
        // Still flagged on the ticks that reuse the cached 401.
        let snaps = run(&mut c, &all, &state, false, at(base, 65));
        assert!(!snaps[0].available && snaps[0].auth_failed);
    }

    /// Calls made so far, after a round at `secs` past `base`.
    fn calls_at(
        c: &mut Collector,
        rig: &Rig,
        base: Instant,
        secs: u64,
        force: bool,
    ) -> (usize, ProviderSnapshot) {
        let snaps = run(c, &rig.0, &Arc::new(AppState::default()), force, at(base, secs));
        (rig.1.load(Ordering::SeqCst), snaps[0].clone())
    }

    #[test]
    fn consecutive_failures_back_off_30_60_120_and_expose_the_wait() {
        let rig = fake(60);
        *rig.2.lock().unwrap() = Mode::Fail;
        let (mut c, base) = (Collector::default(), Instant::now());
        assert_eq!(calls_at(&mut c, &rig, base, 0, false).0, 1);
        // 1st failure: next attempt at +30 s.
        let (n, s) = calls_at(&mut c, &rig, base, 10, false);
        assert_eq!((n, s.retry_in_secs), (1, Some(20)));
        assert_eq!(calls_at(&mut c, &rig, base, 29, false).0, 1);
        assert_eq!(calls_at(&mut c, &rig, base, 30, false).0, 2);
        // 2nd: +60 s (t=90).
        assert_eq!(calls_at(&mut c, &rig, base, 89, false).0, 2);
        assert_eq!(calls_at(&mut c, &rig, base, 90, false).0, 3);
        // 3rd: +120 s (t=210).
        assert_eq!(calls_at(&mut c, &rig, base, 209, false).0, 3);
        assert_eq!(calls_at(&mut c, &rig, base, 210, false).0, 4);
    }

    #[test]
    fn backoff_doubles_up_to_ten_minutes() {
        let secs: Vec<u64> = (1..=8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [30, 60, 120, 240, 480, 600, 600, 600]);
        assert_eq!(backoff(u32::MAX).as_secs(), 600);
    }

    #[test]
    fn a_success_resets_the_backoff() {
        let rig = fake(60);
        *rig.2.lock().unwrap() = Mode::Fail;
        let (mut c, base) = (Collector::default(), Instant::now());
        calls_at(&mut c, &rig, base, 0, false); // fail, +30
        calls_at(&mut c, &rig, base, 30, false); // fail, +60
        *rig.2.lock().unwrap() = Mode::Ok;
        let (n, s) = calls_at(&mut c, &rig, base, 90, false);
        assert_eq!(n, 3);
        assert!(s.available && s.retry_in_secs.is_none());
        // Back to the normal 60 s interval...
        assert_eq!(calls_at(&mut c, &rig, base, 149, false).0, 3);
        assert_eq!(calls_at(&mut c, &rig, base, 150, false).0, 4);
        // ...and the next failure starts again at 30 s, not 120 s.
        *rig.2.lock().unwrap() = Mode::Fail;
        calls_at(&mut c, &rig, base, 210, false);
        assert_eq!(calls_at(&mut c, &rig, base, 239, false).0, 5);
        assert_eq!(calls_at(&mut c, &rig, base, 240, false).0, 6);
    }

    #[test]
    fn a_longer_retry_after_wins_a_shorter_one_does_not() {
        let rig = fake(60);
        let (mut c, base) = (Collector::default(), Instant::now());
        *rig.2.lock().unwrap() = Mode::Throttle(300);
        let (_, s) = calls_at(&mut c, &rig, base, 0, false);
        assert_eq!(s.retry_in_secs, Some(300), "shown to the UI as the time left");
        assert_eq!(calls_at(&mut c, &rig, base, 299, false).0, 1);
        // At t=300 it throttles with 10 s: the 2nd-failure backoff (60 s) is longer.
        *rig.2.lock().unwrap() = Mode::Throttle(10);
        assert_eq!(calls_at(&mut c, &rig, base, 300, false).0, 2);
        assert_eq!(calls_at(&mut c, &rig, base, 359, false).0, 2);
        assert_eq!(calls_at(&mut c, &rig, base, 360, false).0, 3);
    }

    #[test]
    fn retry_after_is_capped_at_one_hour() {
        let rig = fake(60);
        let (mut c, base) = (Collector::default(), Instant::now());
        *rig.2.lock().unwrap() = Mode::Throttle(1_000_000);
        let (_, s) = calls_at(&mut c, &rig, base, 0, false);
        assert_eq!(s.retry_in_secs, Some(3600));
        assert_eq!(calls_at(&mut c, &rig, base, 3599, false).0, 1);
        assert_eq!(calls_at(&mut c, &rig, base, 3600, false).0, 2);
    }

    #[test]
    fn force_bypasses_the_backoff() {
        let rig = fake(60);
        *rig.2.lock().unwrap() = Mode::Throttle(600);
        let (mut c, base) = (Collector::default(), Instant::now());
        calls_at(&mut c, &rig, base, 0, false);
        assert_eq!(calls_at(&mut c, &rig, base, 5, false).0, 1);
        *rig.2.lock().unwrap() = Mode::Ok;
        let (n, s) = calls_at(&mut c, &rig, base, 6, true);
        assert_eq!(n, 2);
        assert!(s.available && s.retry_in_secs.is_none());
    }

    #[test]
    fn auth_failure_waits_the_normal_interval_and_is_never_masked() {
        let rig = fake(300);
        let (mut c, base) = (Collector::default(), Instant::now());
        calls_at(&mut c, &rig, base, 0, false);
        *rig.2.lock().unwrap() = Mode::Auth;
        let (n, s) = calls_at(&mut c, &rig, base, 300, false);
        assert!(n == 2 && !s.available && s.auth_failed);
        // Not the 30 s failure retry: the provider's own 300 s.
        for t in [330, 400, 599] {
            let (n, s) = calls_at(&mut c, &rig, base, t, false);
            assert!(n == 2 && s.auth_failed && !s.available, "t={t}");
        }
        assert_eq!(calls_at(&mut c, &rig, base, 600, false).0, 3);
    }

    #[test]
    fn local_provider_is_not_backed_off() {
        let rig = fake(4);
        *rig.2.lock().unwrap() = Mode::Fail;
        let (mut c, base) = (Collector::default(), Instant::now());
        for tick in 0..10 {
            calls_at(&mut c, &rig, base, tick * 5, false);
        }
        assert_eq!(rig.1.load(Ordering::SeqCst), 10);
    }
}
