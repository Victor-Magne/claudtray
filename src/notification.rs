#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::AtomicBool;
#[cfg(windows)]
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[cfg(windows)]
fn wide(s: &str, buf: &mut [u16]) {
    let v: Vec<u16> = OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let len = v.len().min(buf.len());
    buf[..len].copy_from_slice(&v[..len]);
}

/// Show a Windows balloon notification anchored to the dashboard window.
/// The balloon appears in the system tray area without adding a visible icon.
/// Auto-dismissed by Windows after ~5 seconds; cleaned up after 8 s.
///
/// `alive` is the dashboard window's liveness flag: the cleanup thread captures
/// it and only issues `NIM_DELETE` while it is still `true`. If the window is
/// destroyed within the 8-second window (e.g. the user quits the app), the flag
/// flips to `false` and we skip the call — avoiding a `Shell_NotifyIconW` on a
/// dangling `HWND`, which is undefined behaviour at the Win32 level.
#[cfg(windows)]
pub fn show_alert(hwnd: isize, alive: Arc<AtomicBool>, title: &str, body: &str) {
    use winapi::shared::windef::HWND;
    use winapi::um::shellapi::{
        Shell_NotifyIconW, NIF_INFO, NIF_MESSAGE, NIIF_INFO, NIIF_NOSOUND, NIM_ADD, NIM_DELETE,
        NOTIFYICONDATAW,
    };

    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd as HWND;
        nid.uID = 0xCE;
        nid.uFlags = NIF_INFO | NIF_MESSAGE;
        nid.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
        wide(title, &mut nid.szInfoTitle);
        wide(body, &mut nid.szInfo);
        Shell_NotifyIconW(NIM_ADD, &mut nid);

        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(8));
            if !alive.load(Ordering::Acquire) {
                return; // window gone — HWND would be dangling
            }
            let mut del: NOTIFYICONDATAW = std::mem::zeroed();
            del.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            del.hWnd = hwnd as HWND;
            del.uID = 0xCE;
            Shell_NotifyIconW(NIM_DELETE, &mut del);
        });
    }
}

/// Show a desktop notification via D-Bus (`org.freedesktop.Notifications`).
/// Rendered by whatever notification daemon is running — on DankMaterialShell
/// that's the DMS notification popup. `hwnd`/`alive` are Windows-only concepts
/// and ignored here; the daemon owns the popup's lifecycle.
#[cfg(target_os = "linux")]
pub fn show_alert(_hwnd: isize, _alive: Arc<AtomicBool>, title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    // notify-rust blocks on the D-Bus round-trip; keep it off the UI thread.
    std::thread::spawn(move || {
        let _ = notify_rust::Notification::new()
            .appname("ClaudTray")
            .summary(&title)
            .body(&body)
            .icon("dialog-information")
            .timeout(notify_rust::Timeout::Milliseconds(5000))
            .show();
    });
}
