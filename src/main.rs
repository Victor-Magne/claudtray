// Hide the console window in release builds (this is a tray app); keep it in
// debug builds so `println!`/panics are visible during development. The
// CLAUDTRAY_DUMP debug path writes to a file, so it needs no console either.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod dpapi;
mod i18n;
mod model;
mod monitor;
mod notification;
mod providers;
mod renderer;
mod secret;
mod state;
#[cfg(target_os = "linux")]
mod tray_linux;
mod window;

use chrono::{DateTime, Local};
use i18n::{catalog, Lang};
use model::{Snapshot, Status, WindowUsage};
use monitor::QuotaMonitor;
#[cfg(windows)]
use renderer::generate_dynamic_icon;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
#[cfg(windows)]
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
#[cfg(windows)]
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use window::{Dashboard, IpcMessage, UserEvent};

type SharedMonitor = Arc<Mutex<QuotaMonitor>>;

#[tokio::main]
async fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let monitor: SharedMonitor = Arc::new(Mutex::new(QuotaMonitor::new()));

    // Debug helper: `CLAUDTRAY_DUMP=1 claudtray` writes one snapshot to
    // %TEMP%/claudtray_snapshot.json and exits (no UI).
    if std::env::var("CLAUDTRAY_DUMP").is_ok() {
        let snap = monitor.lock().unwrap().refresh();
        let path = std::env::temp_dir().join("claudtray_snapshot.json");
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&snap).unwrap_or_default());
        return;
    }

    // Single instance per session: the lock is held (and auto-released by the
    // OS on exit/crash) for the app's whole lifetime. A second launch exits at
    // once instead of registering a duplicate tray icon.
    let _instance_lock = match acquire_instance_lock() {
        Ok(lock) => lock,
        Err(()) => {
            eprintln!("ClaudTray já está em execução nesta sessão.");
            return;
        }
    };

    // Local mirror of the persisted theme preference ("dark" | "light" |
    // "system"). Kept in sync on SetTheme so the OS-theme-change handler knows
    // whether it should react.
    let mut theme_pref = monitor.lock().unwrap().state.theme.clone();
    let mut language_pref = monitor.lock().unwrap().state.language.clone();
    let mut last: Option<Snapshot> = monitor.lock().unwrap().state.last_snapshot.clone();

    // --- Tray menu (fallback controls) ---
    #[cfg(windows)]
    let tray_menu = Menu::new();
    #[cfg(windows)]
    let (show_item, refresh_item, exit_item, show_id, refresh_id, exit_id) = {
        let l = catalog(Lang::from_pref(&language_pref));
        let show_item = MenuItem::new(l.menu_show, true, None);
        let refresh_item = MenuItem::new(l.menu_refresh, true, None);
        let exit_item = MenuItem::new(l.menu_exit, true, None);
        let _ = tray_menu.append_items(&[
            &show_item,
            &refresh_item,
            &PredefinedMenuItem::separator(),
            &exit_item,
        ]);
        let show_id = show_item.id().clone();
        let refresh_id = refresh_item.id().clone();
        let exit_id = exit_item.id().clone();
        (show_item, refresh_item, exit_item, show_id, refresh_id, exit_id)
    };

    let initial_status = last.as_ref().map(|s| s.worst_status()).unwrap_or(Status::Healthy);
    let initial_lang = Lang::from_pref(&language_pref);
    let initial_tooltip = last
        .as_ref()
        .map(|s| tooltip(s, initial_lang))
        .unwrap_or_else(|| catalog(initial_lang).tray_loading.to_string());
    // Per-window alert de-duplication: remembers which notifications already
    // fired so a window sitting at Critical/Depleted doesn't re-alert every tick.
    let mut alert_state = AlertState::default();
    // Initialise "in the past" so the first alert can fire immediately.
    // checked_sub avoids an overflow panic on freshly-booted machines where the
    // monotonic clock (uptime) is still under an hour.
    let mut last_alert = Instant::now()
        .checked_sub(Duration::from_secs(3600))
        .unwrap_or_else(Instant::now);

    #[cfg(windows)]
    let icon = Icon::from_rgba(generate_dynamic_icon(initial_status), 64, 64)
        .expect("ícone RGBA inválido");
    // Attach the context menu to the tray and let tray-icon show it natively on
    // right-click. The library does the SetForegroundWindow + TrackPopupMenu
    // dance for us, so the menu pops up at the cursor and dismisses cleanly on
    // click-away. Left-click is kept menu-free so we can use it to toggle the
    // dashboard ourselves.
    #[cfg(windows)]
    let mut tray: Option<TrayIcon> = Some(
        TrayIconBuilder::new()
            .with_tooltip(initial_tooltip)
            .with_icon(icon)
            .with_menu(Box::new(tray_menu))
            .with_menu_on_left_click(false)
            .build()
            .expect("falha ao criar o tray icon"),
    );

    // Linux: ksni serves the StatusNotifierItem + dbusmenu over D-Bus; clicks
    // and menu picks arrive as UserEvents through the same proxy (tray_linux.rs).
    #[cfg(target_os = "linux")]
    let tray = tray_linux::spawn(proxy.clone(), initial_status, initial_tooltip, initial_lang).await;

    // Forward tray + menu events into the event loop through the proxy. The
    // crate's default global channels are only drained when the loop happens to
    // wake for some other reason (an IPC message or the 5–60 s ticker), which
    // made clicks feel laggy and could replay the menu; routing each event
    // through `EventLoopProxy` wakes the loop at once and handles it exactly
    // once. This replaces `TrayIconEvent::receiver()` / `MenuEvent::receiver()`.
    #[cfg(windows)]
    {
        let tray_proxy = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            // Only the left-click release matters here; right-click is handled
            // natively by the attached menu.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = tray_proxy.send_event(UserEvent::TrayToggle);
            }
        }));

        let menu_proxy = proxy.clone();
        let (id_show, id_refresh, id_exit) =
            (show_id.clone(), refresh_id.clone(), exit_id.clone());
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let mapped = if event.id == id_exit {
                UserEvent::MenuExit
            } else if event.id == id_refresh {
                UserEvent::MenuRefresh
            } else if event.id == id_show {
                UserEvent::MenuShow
            } else {
                return;
            };
            let _ = menu_proxy.send_event(mapped);
        }));
    }

    // --- Dashboard popover (starts hidden) ---
    let mut dashboard = Dashboard::new(&event_loop, proxy.clone(), &theme_pref);

    // First refresh + adaptive background ticker. While the popover is open we
    // poll fast (near real-time); when it's hidden we slow down to spare the
    // provider APIs. All refreshes run off the UI thread (network I/O).
    spawn_refresh(&monitor, &proxy);
    // The dashboard is shown on launch (see below), so start the ticker on the
    // fast cadence right away.
    let dashboard_open = Arc::new(AtomicBool::new(true));
    let tick_proxy = proxy.clone();
    let ticker_open = Arc::clone(&dashboard_open);
    tokio::spawn(async move {
        loop {
            let secs = if ticker_open.load(Ordering::Relaxed) { 5 } else { 60 };
            tokio::time::sleep(Duration::from_secs(secs)).await;
            if tick_proxy.send_event(UserEvent::Tick).is_err() {
                break;
            }
        }
    });

    // Open the dashboard on launch so it's the default window. `last_action`
    // starts at `now` so the 1500ms blur grace is armed from the moment we show
    // — the initial focus settling won't trigger a click-away that closes it
    // immediately. It also guards subsequent show/hide actions in the loop. The
    // tray keeps running in the background; closing the popover hides it.
    let mut last_action = Instant::now();
    dashboard.show();
    if let Some(snap) = &last {
        dashboard.push(snap);
    }

    event_loop.run(move |event, _, control_flow| {
        // Block until a real event arrives instead of polling every 150ms. A
        // perpetually-waking UI thread never reaches Windows' "input idle" state,
        // which makes the OS show the "working in background" (spinning) cursor
        // for the whole session and wastes CPU. Tray/menu clicks, the background
        // ticker and IPC all wake the loop via EventLoopProxy (see the
        // set_event_handler forwarding above), so nothing is missed.
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(UserEvent::Tick) => spawn_refresh(&monitor, &proxy),
            Event::UserEvent(UserEvent::Snapshot(snap)) => {
                // Fire one notification for the worst window that newly needs
                // attention — a fresh Critical/Depleted, a near-term projected
                // exhaustion, or "critically low but resets soon". The 5 min
                // cooldown and the per-window de-dup set keep it from nagging.
                let now = Local::now();
                let due = alerts_due(&snap, &alert_state, now);
                if !due.is_empty() && last_alert.elapsed() > Duration::from_secs(300) {
                    let lang = Lang::from_pref(&language_pref);
                    let (title, body) = alert_text(&due, lang);
                    notification::show_alert(
                        dashboard.hwnd(),
                        dashboard.alive_flag(),
                        &title,
                        &body,
                    );
                    alert_state.mark_fired(&due);
                    last_alert = Instant::now();
                }
                alert_state.prune(&snap, now);
                let lang = Lang::from_pref(&language_pref);
                #[cfg(windows)]
                update_tray(&mut tray, &snap, lang);
                #[cfg(target_os = "linux")]
                if let Some(handle) = &tray {
                    tray_linux::update(handle, snap.worst_status(), tooltip(&snap, lang), lang);
                }
                dashboard.push(&snap);
                last = Some(snap);
            }
            Event::UserEvent(UserEvent::TrayToggle) => {
                // Left-click on the tray icon. The 300 ms debounce collapses the
                // second click of a double-click (and a near-simultaneous
                // click-away blur) so one physical click toggles exactly once.
                if last_action.elapsed() > Duration::from_millis(300) {
                    if dashboard.is_visible() {
                        dashboard.hide();
                    } else {
                        dashboard.show();
                        if let Some(snap) = &last {
                            dashboard.push(snap);
                        }
                    }
                    last_action = Instant::now();
                }
            }
            Event::UserEvent(UserEvent::MenuShow) => {
                dashboard.show();
                if let Some(snap) = &last {
                    dashboard.push(snap);
                }
                last_action = Instant::now();
            }
            Event::UserEvent(UserEvent::MenuRefresh) => spawn_refresh(&monitor, &proxy),
            Event::UserEvent(UserEvent::MenuExit) => {
                #[cfg(windows)]
                tray.take();
                #[cfg(target_os = "linux")]
                if let Some(handle) = &tray {
                    let handle = handle.clone();
                    tokio::spawn(async move { handle.shutdown().await });
                }
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::Ipc(msg)) => match msg {
                IpcMessage::Ready => {
                    // Tell JS the current Windows theme first so "system" mode
                    // resolves correctly, then push the latest snapshot.
                    dashboard.notify_os_theme();
                    if let Some(snap) = &last {
                        dashboard.push(snap);
                    }
                }
                IpcMessage::Refresh => spawn_refresh(&monitor, &proxy),
                IpcMessage::SetTheme(theme) => {
                    // Applied instantly in JS; here we mirror the preference,
                    // retint the Mica backdrop and persist. "system" follows the
                    // live Windows theme rather than the literal string.
                    theme_pref = theme.clone();
                    let dark = if theme == "system" {
                        dashboard.os_theme_is_dark()
                    } else {
                        theme != "light"
                    };
                    dashboard.set_dark(dark);
                    spawn_set_theme(&monitor, theme);
                }
                IpcMessage::SetLanguage(language) => {
                    language_pref = language.clone();
                    let lang = Lang::from_pref(&language_pref);
                    let l = catalog(lang);
                    #[cfg(windows)]
                    {
                        show_item.set_text(l.menu_show);
                        refresh_item.set_text(l.menu_refresh);
                        exit_item.set_text(l.menu_exit);
                    }
                    #[cfg(target_os = "linux")]
                    if let Some(handle) = &tray {
                        let tooltip_text = last
                            .as_ref()
                            .map(|s| tooltip(s, lang))
                            .unwrap_or_else(|| l.tray_loading.to_string());
                        let status = last
                            .as_ref()
                            .map(|s| s.worst_status())
                            .unwrap_or(Status::Healthy);
                        tray_linux::update(handle, status, tooltip_text, lang);
                    }
                    spawn_set_language(&monitor, language);
                }
                IpcMessage::SetClaudeToken(token) => {
                    spawn_set_claude_token(&monitor, &proxy, token);
                }
                IpcMessage::SetCopilotToken(token) => {
                    spawn_set_token(&monitor, &proxy, token);
                }
                IpcMessage::SyncMica(dark) => {
                    dashboard.set_dark(dark);
                }
                IpcMessage::SetOpenRouterKey(key) => {
                    spawn_set_openrouter_key(&monitor, &proxy, key);
                }
                IpcMessage::SetGeminiKey(key) => {
                    spawn_set_gemini_key(&monitor, &proxy, key);
                }
                IpcMessage::SetHttpProxy(p) => {
                    spawn_set_http_proxy(&monitor, p);
                }
                IpcMessage::SetDisabledProviders(ids) => {
                    spawn_set_disabled_providers(&monitor, &proxy, ids);
                }
                IpcMessage::OpenUrl(target) => {
                    open_url(&target);
                }
                IpcMessage::Close => {
                    dashboard.hide();
                    last_action = Instant::now();
                }
                IpcMessage::Blur => {
                    // Click-away: the webview lost focus to another window. We
                    // route this through the webview (not tao's Focused event)
                    // because the WebView2 child window holds the real focus.
                    // The 1500ms grace period ignores focus settling right after show
                    // and prevents the window closing when the user moves the mouse
                    // away from the tray area immediately after clicking.
                    if dashboard.is_visible()
                        && last_action.elapsed() > Duration::from_millis(1500)
                    {
                        dashboard.hide();
                        last_action = Instant::now();
                    }
                }
            },
            Event::WindowEvent {
                event: tao::event::WindowEvent::Focused(false),
                ..
            } => {
                if dashboard.is_visible()
                    && last_action.elapsed() > Duration::from_millis(1500)
                {
                    dashboard.hide();
                    last_action = Instant::now();
                }
            }
            Event::WindowEvent {
                event: tao::event::WindowEvent::KeyboardInput {
                    event: key_event,
                    ..
                },
                ..
            } => {
                if key_event.state == tao::event::ElementState::Pressed
                    && key_event.physical_key == tao::keyboard::KeyCode::Escape
                    && dashboard.is_visible()
                {
                    dashboard.hide();
                    last_action = Instant::now();
                }
            }
            // The user flipped Windows between light and dark. Only act while
            // following the system theme: retint Mica and tell JS so the
            // dashboard tracks the OS in real time.
            Event::WindowEvent {
                event: tao::event::WindowEvent::ThemeChanged(theme),
                ..
            } if theme_pref == "system" => {
                dashboard.set_dark(theme == tao::window::Theme::Dark);
                dashboard.notify_os_theme();
            }
            _ => {}
        }

        // Keep the ticker cadence in sync with popover visibility (fast when
        // open, slow when hidden).
        dashboard_open.store(dashboard.is_visible(), Ordering::Relaxed);
    });
}

/// Run a refresh on a background thread and deliver the snapshot to the UI. If a
/// refresh (or token/theme update) already holds the lock, this tick is skipped
/// so fast polling never piles up.
fn spawn_refresh(monitor: &SharedMonitor, proxy: &EventLoopProxy<UserEvent>) {
    let monitor = Arc::clone(monitor);
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let snapshot = match monitor.try_lock() {
            Ok(mut guard) => guard.refresh(),
            Err(_) => return,
        };
        let _ = proxy.send_event(UserEvent::Snapshot(snapshot));
    });
}

/// Persist a theme change off the UI thread (avoids blocking on the monitor lock
/// while a refresh is in flight).
fn spawn_set_theme(monitor: &SharedMonitor, theme: String) {
    let monitor = Arc::clone(monitor);
    std::thread::spawn(move || {
        monitor.lock().unwrap().set_theme(&theme);
    });
}

/// Persist a language change off the UI thread (mirrors `spawn_set_theme`).
fn spawn_set_language(monitor: &SharedMonitor, language: String) {
    let monitor = Arc::clone(monitor);
    std::thread::spawn(move || {
        monitor.lock().unwrap().set_language(&language);
    });
}

/// Store the Copilot token and refresh so the new credential is picked up.
fn spawn_set_token(monitor: &SharedMonitor, proxy: &EventLoopProxy<UserEvent>, token: String) {
    let monitor = Arc::clone(monitor);
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let snapshot = {
            let mut guard = monitor.lock().unwrap();
            guard.set_copilot_token(&token);
            guard.refresh()
        };
        let _ = proxy.send_event(UserEvent::Snapshot(snapshot));
    });
}

/// Guard a single instance per user session. Returns the held lock file on
/// success (kept alive for the process lifetime; the OS releases it on exit or
/// crash) and `Err(())` when another instance already holds it. If the lock
/// infrastructure itself is unavailable, startup proceeds unguarded.
fn acquire_instance_lock() -> Result<Option<std::fs::File>, ()> {
    // Linux: XDG_RUNTIME_DIR is per-session and wiped on logout. Windows/other:
    // the per-user temp dir.
    let dir = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("claudtray.lock"))
    else {
        return Ok(None);
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Err(()),
        Err(_) => Ok(None),
    }
}

/// Refresh the tray icon colour + tooltip from the latest snapshot.
#[cfg(windows)]
fn update_tray(tray: &mut Option<TrayIcon>, snap: &Snapshot, lang: Lang) {
    let Some(t) = tray else {
        return;
    };
    if let Ok(icon) = Icon::from_rgba(generate_dynamic_icon(snap.worst_status()), 64, 64) {
        let _ = t.set_icon(Some(icon));
    }
    let _ = t.set_tooltip(Some(tooltip(snap, lang)));
}

/// Store the Claude OAuth token (from `claude setup-token`) and refresh so the
/// new credential is picked up.
fn spawn_set_claude_token(monitor: &SharedMonitor, proxy: &EventLoopProxy<UserEvent>, token: String) {
    let monitor = Arc::clone(monitor);
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let snapshot = {
            let mut guard = monitor.lock().unwrap();
            guard.set_claude_token(&token);
            guard.refresh()
        };
        let _ = proxy.send_event(UserEvent::Snapshot(snapshot));
    });
}

fn spawn_set_openrouter_key(monitor: &SharedMonitor, proxy: &EventLoopProxy<UserEvent>, key: String) {
    let monitor = Arc::clone(monitor);
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let snapshot = {
            let mut guard = monitor.lock().unwrap();
            guard.set_openrouter_key(&key);
            guard.refresh()
        };
        let _ = proxy.send_event(UserEvent::Snapshot(snapshot));
    });
}

fn spawn_set_gemini_key(monitor: &SharedMonitor, proxy: &EventLoopProxy<UserEvent>, key: String) {
    let monitor = Arc::clone(monitor);
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let snapshot = {
            let mut guard = monitor.lock().unwrap();
            guard.set_gemini_key(&key);
            guard.refresh()
        };
        let _ = proxy.send_event(UserEvent::Snapshot(snapshot));
    });
}

/// Persist the hidden-provider list and refresh so the dashboard, tray icon
/// and alerts all reflect the new selection at once.
fn spawn_set_disabled_providers(
    monitor: &SharedMonitor,
    proxy: &EventLoopProxy<UserEvent>,
    ids: Vec<String>,
) {
    let monitor = Arc::clone(monitor);
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let snapshot = {
            let mut guard = monitor.lock().unwrap();
            guard.set_disabled_providers(ids);
            guard.refresh()
        };
        let _ = proxy.send_event(UserEvent::Snapshot(snapshot));
    });
}

fn spawn_set_http_proxy(monitor: &SharedMonitor, proxy_url: String) {
    let monitor = Arc::clone(monitor);
    std::thread::spawn(move || {
        monitor.lock().unwrap().set_http_proxy(&proxy_url);
    });
}

/// Open a whitelisted URL in the default browser (cmd /c start on Windows,
/// xdg-open on Linux).
fn open_url(target: &str) {
    let url = match target {
        "github-tokens"  => "https://github.com/settings/tokens",
        "openrouter-keys" => "https://openrouter.ai/keys",
        "gemini-keys"    => "https://aistudio.google.com/app/apikey",
        _ => return,
    };
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// One notification-worthy condition on a single usage window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    /// Provider display name, e.g. "Claude".
    provider: String,
    /// Window display label, e.g. "SESSION".
    label: String,
    /// Remaining percentage at the time the alert was raised.
    pct: u32,
    kind: AlertKind,
    /// `"{provider_id}:{window_key}:{kind_tag}"` — the de-dup identity.
    dedup_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    /// Window just dropped into the Critical band (1–19%).
    EnteredCritical,
    /// Window is at 0% (or has no data).
    Depleted,
    /// Not Critical yet, but the projection says it hits 0% in `.0` minutes.
    ExhaustsSoon(i64),
    /// Critically low, but the window resets in `.0` minutes — just wait.
    ResetsSoon(i64),
}

impl AlertKind {
    /// Stable identity for de-dup (independent of the minutes value).
    fn tag(self) -> &'static str {
        match self {
            AlertKind::EnteredCritical => "critical",
            AlertKind::Depleted => "depleted",
            AlertKind::ExhaustsSoon(_) => "exhausts",
            AlertKind::ResetsSoon(_) => "resets",
        }
    }

    /// Higher == show this one first when several fire at once.
    fn severity(self) -> u8 {
        match self {
            AlertKind::Depleted => 3,
            AlertKind::EnteredCritical | AlertKind::ResetsSoon(_) => 2,
            AlertKind::ExhaustsSoon(_) => 1,
        }
    }
}

/// Remembers which per-window alerts already fired, so a window that stays
/// Critical doesn't re-notify on every 5–60 s refresh.
#[derive(Default)]
struct AlertState {
    fired: HashSet<String>,
}

impl AlertState {
    fn mark_fired(&mut self, alerts: &[Alert]) {
        for a in alerts {
            self.fired.insert(a.dedup_key.clone());
        }
    }

    /// Forget remembered alerts whose window no longer warrants one (recovered,
    /// reset, projection cleared, or the provider vanished) so a later
    /// regression alerts again.
    fn prune(&mut self, snap: &Snapshot, now: DateTime<Local>) {
        self.fired.retain(|key| {
            let mut parts = key.splitn(3, ':');
            let (Some(pid), Some(wkey), Some(tag)) = (parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            snap.providers
                .iter()
                .find(|p| p.id == pid && p.available)
                .and_then(|p| p.windows.iter().find(|w| w.key == wkey))
                .and_then(|w| classify(w, now))
                .is_some_and(|k| k.tag() == tag)
        });
    }
}

/// Minutes from `now` until `rfc` (an RFC3339 timestamp). `None` if unparseable
/// or already in the past.
fn minutes_until(rfc: &str, now: DateTime<Local>) -> Option<i64> {
    let target = DateTime::parse_from_rfc3339(rfc).ok()?.with_timezone(&Local);
    let mins = (target - now).num_minutes();
    (mins >= 0).then_some(mins)
}

/// The single most useful alert for one window right now, if any.
fn classify(w: &WindowUsage, now: DateTime<Local>) -> Option<AlertKind> {
    let in_trouble = matches!(w.status, Status::Critical | Status::Depleted);

    if in_trouble {
        // A reset within reach beats "you're critical" — the action is to wait.
        if let Some(mins) = w.reset_at.as_deref().and_then(|r| minutes_until(r, now)) {
            if mins < 20 {
                return Some(AlertKind::ResetsSoon(mins));
            }
        }
        return Some(match w.status {
            Status::Depleted => AlertKind::Depleted,
            _ => AlertKind::EnteredCritical,
        });
    }

    // Not Critical yet — warn only if the projection says it will be, soon.
    // `estimated_exhaustion` is already gated (enough history, actually
    // declining, resets after it would run out) by the monitor.
    let mins = w
        .estimated_exhaustion
        .as_deref()
        .and_then(|e| minutes_until(e, now))?;
    (mins < 30).then_some(AlertKind::ExhaustsSoon(mins))
}

/// Every window that should raise a notification this refresh and hasn't yet.
fn alerts_due(snap: &Snapshot, state: &AlertState, now: DateTime<Local>) -> Vec<Alert> {
    let mut due = Vec::new();
    for p in &snap.providers {
        if !p.available {
            continue;
        }
        for w in &p.windows {
            let Some(kind) = classify(w, now) else { continue };
            let dedup_key = format!("{}:{}:{}", p.id, w.key, kind.tag());
            if !state.fired.contains(&dedup_key) {
                due.push(Alert {
                    provider: p.name.clone(),
                    label: w.label.clone(),
                    pct: w.remaining_pct,
                    kind,
                    dedup_key,
                });
            }
        }
    }
    due
}

/// Build the notification title + body from the worst of the due alerts.
fn alert_text(due: &[Alert], lang: Lang) -> (String, String) {
    let l = catalog(lang);
    let a = due
        .iter()
        .max_by_key(|a| a.kind.severity())
        .expect("alert_text called with no alerts");
    let title = match a.kind {
        AlertKind::Depleted => l.alert_depleted_title,
        AlertKind::EnteredCritical => l.alert_critical_title,
        AlertKind::ExhaustsSoon(_) | AlertKind::ResetsSoon(_) => l.alert_predictive_title,
    }
    .to_string();
    let fill = |s: &str| {
        s.replace("{provider}", &a.provider)
            .replace("{label}", &a.label)
            .replace("{pct}", &a.pct.to_string())
    };
    let body = match a.kind {
        AlertKind::Depleted => fill(l.alert_body_exhausted),
        AlertKind::EnteredCritical => fill(l.alert_body_remaining),
        AlertKind::ExhaustsSoon(m) => fill(l.alert_predictive_body).replace("{mins}", &m.to_string()),
        AlertKind::ResetsSoon(m) => fill(l.alert_reset_soon_body).replace("{mins}", &m.to_string()),
    };
    (title, body)
}

/// Short relative time like "1h 5m" / "12m" until `rfc`; `None` if past/unparseable.
fn relative_until(rfc: &str, now: DateTime<Local>) -> Option<String> {
    let mins = minutes_until(rfc, now)?;
    Some(if mins >= 60 {
        format!("{}h {}m", mins / 60, mins % 60)
    } else {
        format!("{mins}m")
    })
}

/// Tray tooltip text: Claude keeps its familiar SESSION%/WEEKLY% line; otherwise
/// (or when Claude has no such windows) show the single worst window across every
/// visible provider, with its reset countdown.
fn tooltip(snap: &Snapshot, lang: Lang) -> String {
    let l = catalog(lang);

    if let Some(claude) = snap.providers.iter().find(|p| p.id == "claude" && p.available) {
        let pct = |key: &str| {
            claude
                .windows
                .iter()
                .find(|w| w.key == key)
                .map(|w| w.remaining_pct)
        };
        if let (Some(s), Some(w)) = (pct("session"), pct("weekly")) {
            return format!(
                "ClaudTray — {} {}% · {} {}%",
                l.tooltip_session, s, l.tooltip_weekly, w
            );
        }
    }

    let worst = snap
        .providers
        .iter()
        .filter(|p| p.available)
        .flat_map(|p| p.windows.iter().map(move |w| (p, w)))
        .max_by_key(|(_, w)| w.status.rank());

    let Some((p, w)) = worst else {
        return l.tooltip_default.to_string();
    };
    let mut line = format!("ClaudTray — {} {} {}%", p.name, w.label, w.remaining_pct);
    if let Some(rel) = w
        .reset_at
        .as_deref()
        .and_then(|r| relative_until(r, Local::now()))
    {
        line.push_str(&format!(" · {} {}", l.tooltip_reset, rel));
    }
    line
}

#[cfg(test)]
mod alert_tests {
    use super::*;
    use crate::model::ProviderSnapshot;

    fn window(pct: u32, reset_at: Option<String>, exhaustion: Option<String>) -> WindowUsage {
        let mut w = WindowUsage::from_percent("session", "SESSION", pct, reset_at);
        w.estimated_exhaustion = exhaustion;
        w
    }

    fn snapshot(id: &str, name: &str, windows: Vec<WindowUsage>) -> Snapshot {
        Snapshot {
            updated_at: String::new(),
            theme: "dark".into(),
            language: "en".into(),
            resolved_language: "en".into(),
            providers: vec![ProviderSnapshot {
                id: id.into(),
                name: name.into(),
                available: true,
                note: None,
                windows,
                total_tokens: None,
                estimated_cost_usd: None,
                local_models: Vec::new(),
                active_sessions: Vec::new(),
            }],
            catalog: Vec::new(),
            history: Default::default(),
        }
    }

    #[test]
    fn direct_drop_to_zero_fires_depleted() {
        // Regression: the old gate compared against Status::Critical.rank(),
        // and Depleted ranks below Critical, so a straight fall to 0% never
        // alerted.
        let snap = snapshot("claude", "Claude", vec![window(0, None, None)]);
        let due = alerts_due(&snap, &AlertState::default(), Local::now());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].kind, AlertKind::Depleted);
    }

    #[test]
    fn exhausts_soon_fires_once() {
        let now = Local::now();
        let exhaustion = (now + chrono::Duration::minutes(15)).to_rfc3339();
        let snap = snapshot("claude", "Claude", vec![window(40, None, Some(exhaustion))]);

        let mut state = AlertState::default();
        let due = alerts_due(&snap, &state, now);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, AlertKind::ExhaustsSoon(_)));

        state.mark_fired(&due);
        assert!(
            alerts_due(&snap, &state, now).is_empty(),
            "same window must not re-alert once fired"
        );
    }

    #[test]
    fn reset_soon_preferred_over_exhaustion() {
        let now = Local::now();
        let reset = (now + chrono::Duration::minutes(10)).to_rfc3339();
        let exhaustion = (now + chrono::Duration::minutes(5)).to_rfc3339();
        let snap = snapshot(
            "claude",
            "Claude",
            vec![window(10, Some(reset), Some(exhaustion))],
        );
        let due = alerts_due(&snap, &AlertState::default(), now);
        assert_eq!(due.len(), 1);
        assert!(matches!(due[0].kind, AlertKind::ResetsSoon(_)));
    }

    #[test]
    fn recovered_window_alerts_again_after_prune() {
        let now = Local::now();
        let bad = snapshot("claude", "Claude", vec![window(0, None, None)]);
        let mut state = AlertState::default();
        state.mark_fired(&alerts_due(&bad, &state, now));

        let good = snapshot("claude", "Claude", vec![window(80, None, None)]);
        state.prune(&good, now);
        state.prune(&bad, now); // back to zero later

        assert_eq!(alerts_due(&bad, &state, now).len(), 1);
    }

    #[test]
    fn tooltip_shows_worst_non_claude_window() {
        let snap = snapshot("copilot", "Copilot", vec![window(12, None, None)]);
        let t = tooltip(&snap, Lang::En);
        assert!(t.contains("Copilot"), "got: {t}");
        assert!(t.contains("12%"), "got: {t}");
    }

    #[test]
    fn tooltip_keeps_claude_session_weekly_line() {
        let snap = snapshot(
            "claude",
            "Claude",
            vec![
                WindowUsage::from_percent("session", "SESSION", 50, None),
                WindowUsage::from_percent("weekly", "WEEKLY", 80, None),
            ],
        );
        let t = tooltip(&snap, Lang::En);
        assert!(t.contains("SESSION 50%"), "got: {t}");
        assert!(t.contains("WEEKLY 80%"), "got: {t}");
    }
}
