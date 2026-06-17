use std::time::Duration;
use ureq::tls::TlsConfig;
use ureq::Agent;

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
    ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .tls_config(tls)
        .build()
        .new_agent()
}
