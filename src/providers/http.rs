use std::sync::RwLock;
use std::time::Duration;
use ureq::tls::TlsConfig;
use ureq::Agent;

/// Hard cap on any provider HTTP response body we read into memory. Quota/usage
/// JSON payloads are a few KB; 1 MiB is generous headroom. Capping this stops a
/// compromised endpoint or malicious proxy from forcing multi-GB reads that
/// exhaust memory — amplified by the parallel per-provider threads.
pub const MAX_BODY_BYTES: u64 = 1_048_576;

/// Global proxy URL applied to every agent. Stored behind a `RwLock` (not a
/// `OnceLock`) so a runtime change in Settings takes effect on the next refresh
/// instead of being silently ignored until the next app launch.
static PROXY_URL: RwLock<Option<String>> = RwLock::new(None);

/// Validate a user-supplied proxy URL. Only `http`/`https` with a host are
/// accepted — this is a defence-in-depth gate so that even if a malicious IPC
/// message reaches `set_proxy`, it cannot point traffic at an arbitrary scheme.
pub fn is_valid_proxy(url: &str) -> bool {
    let url = url.trim();
    let rest = match url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    {
        Some(r) => r,
        None => return false,
    };
    // Require a non-empty host portion (before any path/credentials separator).
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host); // drop user:pass@
    !host.is_empty() && !host.contains(' ')
}

/// Update the proxy applied to all agents. An invalid URL clears the proxy
/// rather than installing an attacker-chosen endpoint.
pub fn set_proxy(url: Option<String>) {
    let sanitized = url.filter(|u| is_valid_proxy(u));
    if let Ok(mut w) = PROXY_URL.write() {
        *w = sanitized;
    }
}

fn proxy_url() -> Option<String> {
    PROXY_URL.read().ok().and_then(|g| g.clone())
}

/// A short-timeout ureq agent. `insecure` disables TLS certificate
/// verification — required for the Antigravity language server, which serves
/// over HTTPS on localhost with a self-signed certificate. The global timeout
/// keeps a hung endpoint from stalling a refresh.
pub fn agent(insecure: bool) -> Agent {
    let tls = if insecure {
        TlsConfig::builder().disable_verification(true).build()
    } else {
        TlsConfig::builder().build()
    };
    let mut builder = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .tls_config(tls);
    if let Some(proxy) = proxy_url() {
        if let Ok(p) = ureq::Proxy::new(&proxy) {
            builder = builder.proxy(Some(p));
        }
    }
    builder.build().new_agent()
}

/// Retry a fallible operation up to `attempts` times with linear backoff.
/// Backoff starts at `delay_secs` and increases by `delay_secs` per retry.
/// Returns the first `Some` result, or `None` if all attempts fail.
pub fn with_retry<T, F>(attempts: u8, delay_secs: u64, mut f: F) -> Option<T>
where
    F: FnMut() -> Option<T>,
{
    for i in 0..attempts {
        if i > 0 {
            std::thread::sleep(Duration::from_secs(delay_secs * i as u64));
        }
        if let Some(v) = f() {
            return Some(v);
        }
    }
    None
}
