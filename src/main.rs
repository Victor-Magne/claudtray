#![windows_subsystem = "windows"]

mod model;
mod monitor;
mod providers;
mod renderer;
mod state;
mod window;

use model::{Snapshot, Status};
use monitor::QuotaMonitor;
use renderer::generate_dynamic_icon;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, ContextMenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use window::{Dashboard, IpcMessage, UserEvent};

type SharedMonitor = Arc<Mutex<QuotaMonitor>>;

#[tokio::main]
async fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let monitor: SharedMonitor = Arc::new(Mutex::new(QuotaMonitor::new()));

    // Debug helper: `CLOUDTRAY_DUMP=1 cloudtray` writes one snapshot to
    // %TEMP%/cloudtray_snapshot.json and exits (no UI).
    if std::env::var("CLOUDTRAY_DUMP").is_ok() {
        let snap = monitor.lock().unwrap().refresh();
        let path = std::env::temp_dir().join("cloudtray_snapshot.json");
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&snap).unwrap_or_default());
        return;
    }

    let initial_dark = monitor.lock().unwrap().state.theme != "light";
    let mut last: Option<Snapshot> = monitor.lock().unwrap().state.last_snapshot.clone();

    // --- Tray menu (fallback controls) ---
    let tray_menu = Menu::new();
    let show_item = MenuItem::new("Mostrar painel", true, None);
    let refresh_item = MenuItem::new("Atualizar", true, None);
    let exit_item = MenuItem::new("Sair", true, None);
    let _ = tray_menu.append_items(&[
        &show_item,
        &refresh_item,
        &PredefinedMenuItem::separator(),
        &exit_item,
    ]);
    let show_id = show_item.id().clone();
    let refresh_id = refresh_item.id().clone();
    let exit_id = exit_item.id().clone();

    let menu_for_manual_show = tray_menu.clone();

    let initial_status = last.as_ref().map(|s| s.worst_status()).unwrap_or(Status::Healthy);
    let initial_tooltip = last.as_ref().map(|s| tooltip(s)).unwrap_or_else(|| "CloudTray — a carregar…".to_string());

    let icon = Icon::from_rgba(generate_dynamic_icon(initial_status), 64, 64)
        .expect("ícone RGBA inválido");
    let mut tray: Option<TrayIcon> = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip(initial_tooltip)
            .with_icon(icon)
            .build()
            .expect("falha ao criar o tray icon"),
    );

    // --- Dashboard popover (starts hidden) ---
    let mut dashboard = Dashboard::new(&event_loop, proxy.clone(), initial_dark);

    // First refresh + adaptive background ticker. While the popover is open we
    // poll fast (near real-time); when it's hidden we slow down to spare the
    // provider APIs. All refreshes run off the UI thread (network I/O).
    spawn_refresh(&monitor, &proxy);
    let dashboard_open = Arc::new(AtomicBool::new(false));
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

    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();
    // Debounces tray clicks and guards the post-show focus settling.
    let mut last_action = Instant::now() - Duration::from_secs(10);
    let mut pending_left_click: Option<Instant> = None;

    event_loop.run(move |event, _, control_flow| {
        if pending_left_click.is_some() {
            *control_flow = ControlFlow::Poll;
        } else {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(150));
        }

        match event {
            Event::UserEvent(UserEvent::Tick) => spawn_refresh(&monitor, &proxy),
            Event::UserEvent(UserEvent::Snapshot(snap)) => {
                update_tray(&mut tray, &snap);
                dashboard.push(&snap);
                last = Some(snap);
            }
            Event::UserEvent(UserEvent::Ipc(msg)) => match msg {
                IpcMessage::Ready => {
                    if let Some(snap) = &last {
                        dashboard.push(snap);
                    }
                }
                IpcMessage::Refresh => spawn_refresh(&monitor, &proxy),
                IpcMessage::SetTheme(theme) => {
                    // Applied instantly in JS; here we only persist + retint Mica.
                    dashboard.set_dark(theme != "light");
                    spawn_set_theme(&monitor, theme);
                }
                IpcMessage::SetCopilotToken(token) => {
                    spawn_set_token(&monitor, &proxy, token);
                }
                IpcMessage::Close => {
                    dashboard.hide();
                    last_action = Instant::now();
                }
                IpcMessage::Blur => {
                    // Click-away: the webview lost focus to another window. We
                    // route this through the webview (not tao's Focused event)
                    // because the WebView2 child window holds the real focus.
                    // The grace period ignores the focus settling right after a
                    // show.
                    if dashboard.is_visible()
                        && last_action.elapsed() > Duration::from_millis(600)
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
                    && last_action.elapsed() > Duration::from_millis(600)
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
            _ => {}
        }

        // Process all tray events.
        while let Ok(tray_event) = tray_rx.try_recv() {
            match tray_event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => {
                    pending_left_click = Some(Instant::now());
                }
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => {
                    pending_left_click = None;
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
                _ => {}
            }
        }

        // Check single click timer.
        if let Some(click_time) = pending_left_click {
            if click_time.elapsed() >= Duration::from_millis(250) {
                pending_left_click = None;
                if last_action.elapsed() > Duration::from_millis(300) {
                    unsafe {
                        let hwnd = dashboard.hwnd();
                        let _ = menu_for_manual_show.show_context_menu_for_hwnd(hwnd, None);
                    }
                    last_action = Instant::now();
                }
            }
        }

        // Tray menu items.
        if let Ok(ev) = menu_rx.try_recv() {
            if ev.id == exit_id {
                tray.take();
                *control_flow = ControlFlow::Exit;
            } else if ev.id == refresh_id {
                spawn_refresh(&monitor, &proxy);
            } else if ev.id == show_id {
                dashboard.show();
                if let Some(snap) = &last {
                    dashboard.push(snap);
                }
                last_action = Instant::now();
            }
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

/// Refresh the tray icon colour + tooltip from the latest snapshot.
fn update_tray(tray: &mut Option<TrayIcon>, snap: &Snapshot) {
    let Some(t) = tray else {
        return;
    };
    if let Ok(icon) = Icon::from_rgba(generate_dynamic_icon(snap.worst_status()), 64, 64) {
        let _ = t.set_icon(Some(icon));
    }
    let _ = t.set_tooltip(Some(tooltip(snap)));
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
            return format!("CloudTray — SESSION {}% · WEEKLY {}%", s, w);
        }
    }
    "CloudTray — AI Usage Monitor".to_string()
}
