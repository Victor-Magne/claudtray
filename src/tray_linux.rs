//! Linux tray icon via `ksni` (pure-Rust StatusNotifierItem over D-Bus).
//!
//! The `tray-icon` crate's Linux backend (libayatana-appindicator) exports no
//! handler for the SNI `Activate` method, so left-clicks from hosts like the
//! DankMaterialShell bar are silently dropped. `ksni` implements the full SNI
//! interface: left-click → `Tray::activate` → `UserEvent::TrayToggle`, and the
//! context menu is served over dbusmenu with per-item callbacks — same
//! behaviour as the Windows tray.

use crate::i18n::{catalog, Lang};
use crate::model::Status;
use crate::renderer::generate_dynamic_icon;
use crate::window::UserEvent;
use ksni::menu::{MenuItem, StandardItem};
use ksni::TrayMethods;
use tao::event_loop::EventLoopProxy;

pub struct ClaudTray {
    proxy: EventLoopProxy<UserEvent>,
    icon: ksni::Icon,
    tooltip: String,
    lang: Lang,
}

/// RGBA (renderer output) → ARGB32 network byte order (SNI wire format).
fn to_ksni_icon(mut rgba: Vec<u8>) -> ksni::Icon {
    for px in rgba.chunks_exact_mut(4) {
        px.rotate_right(1);
    }
    ksni::Icon {
        width: 64,
        height: 64,
        data: rgba,
    }
}

impl ksni::Tray for ClaudTray {
    fn id(&self) -> String {
        "claudtray".into()
    }

    fn title(&self) -> String {
        "ClaudTray".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.icon.clone()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.tooltip.clone(),
            ..Default::default()
        }
    }

    /// Left-click on the tray icon — toggle the dashboard popover.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.proxy.send_event(UserEvent::TrayToggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let l = catalog(self.lang);
        vec![
            StandardItem {
                label: l.menu_show.into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.proxy.send_event(UserEvent::MenuShow);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: l.menu_refresh.into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.proxy.send_event(UserEvent::MenuRefresh);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: l.menu_exit.into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.proxy.send_event(UserEvent::MenuExit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Register the tray on the session bus. Async because ksni runs on the tokio
/// runtime; called from `main` before the event loop starts.
pub async fn spawn(
    proxy: EventLoopProxy<UserEvent>,
    status: Status,
    tooltip: String,
    lang: Lang,
) -> Option<ksni::Handle<ClaudTray>> {
    let tray = ClaudTray {
        proxy,
        icon: to_ksni_icon(generate_dynamic_icon(status)),
        tooltip,
        lang,
    };
    tray.spawn().await.ok()
}

/// Refresh icon colour + tooltip (and, on a language change, the menu labels
/// picked up on the next open) from the UI thread (fire-and-forget task —
/// `Handle::update` is async and the event loop closure is not).
pub fn update(handle: &ksni::Handle<ClaudTray>, status: Status, tooltip: String, lang: Lang) {
    let handle = handle.clone();
    tokio::spawn(async move {
        handle
            .update(move |t| {
                t.icon = to_ksni_icon(generate_dynamic_icon(status));
                t.tooltip = tooltip;
                t.lang = lang;
            })
            .await;
    });
}
