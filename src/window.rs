use crate::model::Snapshot;
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Window, WindowBuilder};
use window_vibrancy::apply_mica;
use wry::{WebView, WebViewBuilder};
// Duplicate import removed
use serde_json;


const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 600.0;

/// Custom events pumped through the tao event loop.
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// A message arrived from the dashboard JS.
    Ipc(IpcMessage),
    /// The background timer asked for a refresh.
    Tick,
    /// A background refresh finished and produced this snapshot.
    Snapshot(Snapshot),
}

/// Actions the dashboard can request from Rust (mirrors `dashboard.js`).
#[derive(Debug, Clone)]
pub enum IpcMessage {
    Ready,
    Refresh,
    SetTheme(String),
    SetCopilotToken(String),
    Close,
    /// The webview lost focus to another window (click-away).
    Blur,
}

fn build_html() -> String {
    let html = include_str!("ui/dashboard.html");
    let css = include_str!("ui/dashboard.css");
    let js = include_str!("ui/dashboard.js");
    html.replace("__CLAUDEBAR_CSS__", css)
        .replace("__CLAUDEBAR_JS__", js)
}

fn parse_ipc(body: &str) -> Option<IpcMessage> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    match v.get("type")?.as_str()? {
        "ready" => Some(IpcMessage::Ready),
        "refresh" => Some(IpcMessage::Refresh),
        "close" => Some(IpcMessage::Close),
        "blur" => Some(IpcMessage::Blur),
        "setTheme" => {
            let theme = v.get("theme")?.as_str()?.to_string();
            Some(IpcMessage::SetTheme(theme))
        }
        "setCopilotToken" => {
            let token = v.get("token")?.as_str()?.to_string();
            Some(IpcMessage::SetCopilotToken(token))
        }
        _ => None,
    }
}

/// The popover dashboard: a frameless, always-on-top tao window with a Mica
/// backdrop hosting a WebView2 webview, anchored to the bottom-right above the
/// taskbar like a native Win11 flyout.
pub struct Dashboard {
    // Field order matters: the webview must drop before the window it lives in.
    webview: WebView,
    window: Window,
    visible: bool,
}

impl Dashboard {
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        proxy: EventLoopProxy<UserEvent>,
        dark: bool,
    ) -> Self {
        let window = WindowBuilder::new()
            .with_title("ClaudeBar")
            .with_inner_size(LogicalSize::new(WIDTH, HEIGHT))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_always_on_top(true)
            .with_visible(false)
            .with_skip_taskbar(true)
            .build(target)
            .expect("falha ao criar a janela do dashboard");

        // Authentic Win11 translucent backdrop.
        let _ = apply_mica(&window, Some(dark));

        let webview = WebViewBuilder::new()
            .with_transparent(true)
            .with_html(build_html())
            .with_ipc_handler(move |req| {
                if let Some(msg) = parse_ipc(req.body()) {
                    println!("Received IPC: {:?}", msg);
                    let _ = proxy.send_event(UserEvent::Ipc(msg));
                } else {
                    println!("Failed to parse IPC: {}", req.body());
                }
            })
            .build(&window)
            .expect("falha ao criar o webview");

        Self {
            webview,
            window,
            visible: false,
        }
    }

    /// Push a fresh snapshot to the dashboard.
    pub fn push(&self, snapshot: &Snapshot) {
        if let Ok(json) = serde_json::to_string(snapshot) {
            let _ = self
                .webview
                .evaluate_script(&format!("window.updateData({});", json));
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn hwnd(&self) -> isize {
        self.window.hwnd() as isize
    }

    pub fn show(&mut self) {
        self.position_bottom_right();
        self.window.set_visible(true);
        self.window.set_focus();
        // Give the webview real keyboard focus so it receives Esc and fires
        // `blur` on click-away (the parent window's focus alone is not enough).
        let _ = self.webview.focus();
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.window.set_visible(false);
        self.visible = false;
    }

    /// Re-apply the Mica backdrop tint when the theme changes.
    pub fn set_dark(&self, dark: bool) {
        let _ = apply_mica(&self.window, Some(dark));
    }

    fn position_bottom_right(&self) {
        let Some(monitor) = self.window.current_monitor() else {
            return;
        };
        let scale = self.window.scale_factor();
        let msize = monitor.size();
        let mpos = monitor.position();
        let wsize = self.window.outer_size();
        let margin = (12.0 * scale) as i32;
        let taskbar = (48.0 * scale) as i32; // approximate taskbar height
        let x = mpos.x + msize.width as i32 - wsize.width as i32 - margin;
        let y = mpos.y + msize.height as i32 - wsize.height as i32 - taskbar - margin;
        self.window
            .set_outer_position(PhysicalPosition::new(x.max(mpos.x), y.max(mpos.y)));
    }
}
