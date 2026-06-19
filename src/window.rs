use crate::model::Snapshot;
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Window, WindowBuilder};
use window_vibrancy::apply_mica;
use wry::{WebContext, WebView, WebViewBuilder};
use serde_json;


const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 640.0;

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
    /// OS theme changed while preference is "system" — re-apply Mica tint.
    SyncMica(bool),
    SetOpenRouterKey(String),
    SetGeminiKey(String),
    SetHttpProxy(String),
    /// Open a whitelisted external URL in the default browser.
    OpenUrl(String),
}

fn build_html() -> String {
    let html = include_str!("ui/dashboard.html");
    let css = include_str!("ui/dashboard.css");
    let js = include_str!("ui/dashboard.js");
    let nonce = csp_nonce();
    html.replace("__CLAUDTRAY_CSS__", css)
        .replace("__CLAUDTRAY_JS__", js)
        .replace("__CLAUDTRAY_NONCE__", &nonce)
}

/// Generate a fresh, unguessable CSP nonce for this page load. The content is
/// embedded locally (no remote script can be injected), so the dashboard's
/// inline `<style>`/`<script>` are the only legitimate sources — a per-load
/// nonce lets the strict CSP (`script-src 'nonce-…'`) allow them while blocking
/// any injected inline handler or script element.
fn csp_nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let stack_marker = 0u8;
    let mut state = nanos
        ^ COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (&stack_marker as *const u8 as u64);

    // splitmix64 expansion → 128 bits of hex.
    let mut out = String::with_capacity(32);
    for _ in 0..2 {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.push_str(&format!("{z:016x}"));
    }
    out
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
        "syncMica" => {
            let dark = v.get("dark")?.as_bool()?;
            Some(IpcMessage::SyncMica(dark))
        }
        "setOpenRouterKey" => {
            let key = v.get("key")?.as_str()?.to_string();
            Some(IpcMessage::SetOpenRouterKey(key))
        }
        "setGeminiKey" => {
            let key = v.get("key")?.as_str()?.to_string();
            Some(IpcMessage::SetGeminiKey(key))
        }
        "setHttpProxy" => {
            let proxy = v.get("proxy")?.as_str()?.to_string();
            Some(IpcMessage::SetHttpProxy(proxy))
        }
        "openUrl" => {
            let target = v.get("target")?.as_str()?.to_string();
            Some(IpcMessage::OpenUrl(target))
        }
        _ => None,
    }
}

/// The popover dashboard: a frameless, always-on-top tao window with a Mica
/// backdrop hosting a WebView2 webview, anchored to the bottom-right above the
/// taskbar like a native Win11 flyout.
pub struct Dashboard {
    // Field order matters: webview → context → window (reverse drop order).
    webview: WebView,
    _context: WebContext,
    window: Window,
    visible: bool,
    /// True while the underlying window (and its HWND) is valid. Cleared on drop
    /// so background tray-notification threads don't touch a dangling HWND.
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Dashboard {
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        proxy: EventLoopProxy<UserEvent>,
        dark: bool,
    ) -> Self {
        let window = WindowBuilder::new()
            .with_title("ClaudTray")
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

        // Store WebView2 cache in %APPDATA%\ClaudTray\WebView2 so the app can
        // run from Program Files (read-only) without write-permission panics.
        let webview_data_dir = dirs::config_dir()
            .map(|d| d.join("ClaudTray").join("WebView2"));
        let mut context = WebContext::new(webview_data_dir);

        let webview = WebViewBuilder::new_with_web_context(&mut context)
            .with_transparent(true)
            .with_html(build_html())
            .with_ipc_handler(move |req| {
                if let Some(msg) = parse_ipc(req.body()) {
                    let _ = proxy.send_event(UserEvent::Ipc(msg));
                }
            })
            .build(&window)
            .expect("falha ao criar o webview");

        Self {
            webview,
            _context: context,
            window,
            visible: false,
            alive: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    /// A clone of the window-liveness flag, handed to background notification
    /// threads so they can skip cleanup once the window is gone.
    pub fn alive_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.alive)
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

impl Drop for Dashboard {
    fn drop(&mut self) {
        // The window (and its HWND) is about to be destroyed; signal any pending
        // tray-notification cleanup threads to stand down (see notification.rs).
        self.alive
            .store(false, std::sync::atomic::Ordering::Release);
    }
}
