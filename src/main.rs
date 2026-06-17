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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use window::{Dashboard, IpcMessage, UserEvent};

type SharedMonitor = Arc<Mutex<QuotaMonitor>>;

#[tokio::main]
async fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let monitor: SharedMonitor = Arc::new(Mutex::new(QuotaMonitor::new()));

    // Debug helper: `CLAUDEBAR_DUMP=1 claudebar-rs` writes one snapshot to
    // %TEMP%/claudebar_snapshot.json and exits (no UI).
    if std::env::var("CLAUDEBAR_DUMP").is_ok() {
        let snap = monitor.lock().unwrap().refresh();
        let path = std::env::temp_dir().join("claudebar_snapshot.json");
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&snap).unwrap_or_default());
        return;
    }

    let initial_dark = monitor.lock().unwrap().state.theme != "light";

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

    let icon = Icon::from_rgba(generate_dynamic_icon(Status::Healthy), 64, 64)
        .expect("ícone RGBA inválido");
    let mut tray: Option<TrayIcon> = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("ClaudeBar — a carregar…")
            .with_icon(icon)
            .build()
            .expect("falha ao criar o tray icon"),
    );

    // --- Dashboard popover (starts hidden) ---
    let mut dashboard = Dashboard::new(&event_loop, proxy.clone(), initial_dark);
    let mut last: Option<Snapshot> = None;

    // First refresh + background ticker (every 60s). All refreshes run off the
    // UI thread because providers may do network I/O.
    spawn_refresh(&monitor, &proxy);
    let tick_proxy = proxy.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if tick_proxy.send_event(UserEvent::Tick).is_err() {
                break;
            }
        }
    });

    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();
    let mut last_hide = Instant::now() - Duration::from_secs(10);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(150));

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
                    last_hide = Instant::now();
                }
            },
            Event::WindowEvent {
                event: WindowEvent::Focused(false),
                ..
            } => {
                if dashboard.is_visible() {
                    dashboard.hide();
                    last_hide = Instant::now();
                }
            }
            _ => {}
        }

        // Tray icon click → toggle the popover.
        if let Ok(TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }) = tray_rx.try_recv()
        {
            if dashboard.is_visible() {
                dashboard.hide();
                last_hide = Instant::now();
            } else if last_hide.elapsed() > Duration::from_millis(300) {
                dashboard.show();
                if let Some(snap) = &last {
                    dashboard.push(snap);
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
            }
        }
    });
}

/// Run a refresh on a background thread and deliver the snapshot to the UI.
fn spawn_refresh(monitor: &SharedMonitor, proxy: &EventLoopProxy<UserEvent>) {
    let monitor = Arc::clone(monitor);
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        let snapshot = monitor.lock().unwrap().refresh();
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
            return format!("ClaudeBar — SESSION {}% · WEEKLY {}%", s, w);
        }
    }
    "ClaudeBar — AI Usage Monitor".to_string()
}
