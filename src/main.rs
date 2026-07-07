// Hide the console window in release builds (this is a tray app); keep it in
// debug builds so `println!`/panics are visible during development. The
// CLAUDTRAY_DUMP debug path writes to a file, so it needs no console either.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod dpapi;
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

use model::{Snapshot, Status};
use monitor::QuotaMonitor;
#[cfg(windows)]
use renderer::generate_dynamic_icon;
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
    let mut last: Option<Snapshot> = monitor.lock().unwrap().state.last_snapshot.clone();

    // --- Tray menu (fallback controls) ---
    #[cfg(windows)]
    let tray_menu = Menu::new();
    #[cfg(windows)]
    let (show_id, refresh_id, exit_id) = {
        let show_item = MenuItem::new("Mostrar painel", true, None);
        let refresh_item = MenuItem::new("Atualizar", true, None);
        let exit_item = MenuItem::new("Sair", true, None);
        let _ = tray_menu.append_items(&[
            &show_item,
            &refresh_item,
            &PredefinedMenuItem::separator(),
            &exit_item,
        ]);
        (
            show_item.id().clone(),
            refresh_item.id().clone(),
            exit_item.id().clone(),
        )
    };

    let initial_status = last.as_ref().map(|s| s.worst_status()).unwrap_or(Status::Healthy);
    let initial_tooltip = last.as_ref().map(|s| tooltip(s)).unwrap_or_else(|| "ClaudTray — a carregar…".to_string());
    // Alert tracking: fire a notification when any window transitions into Critical/Depleted.
    let mut prev_status = initial_status;
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
    let tray = tray_linux::spawn(proxy.clone(), initial_status, initial_tooltip).await;

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
                let worst = snap.worst_status();
                // Notify when transitioning into Critical/Depleted (5 min cooldown).
                if worst.rank() >= Status::Critical.rank()
                    && prev_status.rank() < Status::Critical.rank()
                    && last_alert.elapsed() > Duration::from_secs(300)
                {
                    let (title, body) = alert_text(&snap);
                    notification::show_alert(
                        dashboard.hwnd(),
                        dashboard.alive_flag(),
                        &title,
                        &body,
                    );
                    last_alert = Instant::now();
                }
                prev_status = worst;
                #[cfg(windows)]
                update_tray(&mut tray, &snap);
                #[cfg(target_os = "linux")]
                if let Some(handle) = &tray {
                    tray_linux::update(handle, worst, tooltip(&snap));
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
                {
                    if dashboard.is_visible() {
                        dashboard.hide();
                        last_action = Instant::now();
                    }
                }
            }
            Event::WindowEvent {
                event: tao::event::WindowEvent::ThemeChanged(theme),
                ..
            } => {
                // The user flipped Windows between light and dark. Only act while
                // following the system theme: retint Mica and tell JS so the
                // dashboard tracks the OS in real time.
                if theme_pref == "system" {
                    dashboard.set_dark(theme == tao::window::Theme::Dark);
                    dashboard.notify_os_theme();
                }
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
fn update_tray(tray: &mut Option<TrayIcon>, snap: &Snapshot) {
    let Some(t) = tray else {
        return;
    };
    if let Ok(icon) = Icon::from_rgba(generate_dynamic_icon(snap.worst_status()), 64, 64) {
        let _ = t.set_icon(Some(icon));
    }
    let _ = t.set_tooltip(Some(tooltip(snap)));
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

/// Build the title + body for a critical/depleted alert notification.
fn alert_text(snap: &Snapshot) -> (String, String) {
    let mut worst = Status::Healthy;
    let mut label = String::new();
    let mut provider = String::new();
    let mut pct = 0u32;
    for p in &snap.providers {
        if !p.available { continue; }
        for w in &p.windows {
            if w.status.rank() > worst.rank() {
                worst = w.status;
                label = w.label.clone();
                provider = p.name.clone();
                pct = w.remaining_pct;
            }
        }
    }
    let title = match worst {
        Status::Critical => "ClaudTray — Quota Crítica".to_string(),
        Status::Depleted => "ClaudTray — Quota Esgotada".to_string(),
        _ => "ClaudTray — Alerta".to_string(),
    };
    let body = if pct == 0 {
        format!("{provider} {label}: esgotado")
    } else {
        format!("{provider} {label}: {pct}% restante")
    };
    (title, body)
}

fn tooltip(snap: &Snapshot) -> String {
    if let Some(claude) = snap
        .providers
        .iter()
        .find(|p| p.id == "claude" && p.available)
    {
        let pct = |key: &str| {
            claude
                .windows
                .iter()
                .find(|w| w.key == key)
                .map(|w| w.remaining_pct)
        };
        if let (Some(s), Some(w)) = (pct("session"), pct("weekly")) {
            return format!("ClaudTray — SESSION {}% · WEEKLY {}%", s, w);
        }
    }
    "ClaudTray — AI Usage Monitor".to_string()
}
