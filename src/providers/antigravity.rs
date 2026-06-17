use super::http::agent;
use super::{reset_from_epoch, Provider};
use crate::model::{ProviderSnapshot, WindowUsage};
use crate::state::AppState;
use chrono::{DateTime, Local, Utc};
use netstat2::{get_sockets_info, AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState};
use serde::Deserialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Google Antigravity (built on the Codeium/Windsurf language server). Usage is
/// only available while the IDE is running: the language-server process exposes
/// a CSRF token on its command line and a local Connect-RPC API. We mirror the
/// macOS ClaudeBar probe: find the process + token, discover its listening
/// port, then POST to `GetUserStatus` over (self-signed) localhost HTTPS.
pub struct AntigravityProvider;

const METADATA_BODY: &str = r#"{"metadata":{"ideName":"antigravity","extensionName":"antigravity","ideVersion":"unknown","locale":"en"}}"#;

impl Provider for AntigravityProvider {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn name(&self) -> &'static str {
        "Antigravity"
    }

    fn collect(&self, _state: &AppState) -> ProviderSnapshot {
        let Some((pid, token)) = find_process() else {
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Abre o Antigravity para ver a quota",
            );
        };

        let ports = find_listen_ports(pid);
        if ports.is_empty() {
            return ProviderSnapshot::unavailable(self.id(), self.name(), "Porta não encontrada");
        }

        for port in ports {
            if let Some(windows) = probe(port, &token) {
                if !windows.is_empty() {
                    return ProviderSnapshot {
                        id: self.id().to_string(),
                        name: self.name().to_string(),
                        available: true,
                        note: None,
                        windows,
                    };
                }
            }
        }

        ProviderSnapshot::unavailable(self.id(), self.name(), "Sem resposta do servidor")
    }
}

/// Locate the Antigravity language-server process and its `--csrf_token`.
fn find_process() -> Option<(u32, String)> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );

    for (pid, proc_) in sys.processes() {
        let args: Vec<String> = proc_
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        if args.is_empty() {
            continue;
        }
        let joined = args.join(" ").to_ascii_lowercase();
        // Must look like the Antigravity language server and carry a CSRF token.
        if !joined.contains("csrf_token") || !joined.contains("antigravity") {
            continue;
        }
        if let Some(token) = extract_csrf(&args) {
            return Some((pid.as_u32(), token));
        }
    }
    None
}

fn extract_csrf(args: &[String]) -> Option<String> {
    for (i, arg) in args.iter().enumerate() {
        if let Some(v) = arg.strip_prefix("--csrf_token=") {
            return Some(v.to_string());
        }
        if arg == "--csrf_token" {
            return args.get(i + 1).cloned();
        }
    }
    None
}

/// TCP ports the given PID is listening on (loopback preferred).
fn find_listen_ports(pid: u32) -> Vec<u16> {
    let af = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;
    let Ok(sockets) = get_sockets_info(af, ProtocolFlags::TCP) else {
        return Vec::new();
    };
    let mut ports = Vec::new();
    for si in sockets {
        if !si.associated_pids.contains(&pid) {
            continue;
        }
        if let ProtocolSocketInfo::Tcp(tcp) = si.protocol_socket_info {
            if tcp.state == TcpState::Listen {
                ports.push((tcp.local_addr.is_loopback(), tcp.local_port));
            }
        }
    }
    // loopback first
    ports.sort_by(|a, b| b.0.cmp(&a.0));
    ports.into_iter().map(|(_, p)| p).collect()
}

fn probe(port: u16, token: &str) -> Option<Vec<WindowUsage>> {
    for scheme in ["https", "http"] {
        let url = format!(
            "{scheme}://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/GetUserStatus"
        );
        let resp = agent(true)
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-Codeium-Csrf-Token", token)
            .header("Connect-Protocol-Version", "1")
            .send(METADATA_BODY);
        let Ok(mut resp) = resp else { continue };
        if resp.status().as_u16() != 200 {
            continue;
        }
        let Ok(text) = resp.body_mut().read_to_string() else {
            continue;
        };
        if let Ok(parsed) = serde_json::from_str::<Resp>(&text) {
            let windows = build_windows(parsed);
            if !windows.is_empty() {
                return Some(windows);
            }
        }
    }
    None
}

// ---- Response model ----

#[derive(Deserialize)]
struct Resp {
    #[serde(rename = "userStatus")]
    user_status: Option<UserStatus>,
}

#[derive(Deserialize)]
struct UserStatus {
    #[serde(rename = "cascadeModelConfigData")]
    cascade: Option<Cascade>,
}

#[derive(Deserialize)]
struct Cascade {
    #[serde(rename = "clientModelConfigs")]
    configs: Option<Vec<ModelConfig>>,
}

#[derive(Deserialize)]
struct ModelConfig {
    label: Option<String>,
    #[serde(rename = "quotaInfo")]
    quota: Option<QuotaInfo>,
}

#[derive(Deserialize)]
struct QuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<serde_json::Value>,
}

fn build_windows(resp: Resp) -> Vec<WindowUsage> {
    let configs = resp
        .user_status
        .and_then(|u| u.cascade)
        .and_then(|c| c.configs)
        .unwrap_or_default();

    let mut windows = Vec::new();
    for (i, cfg) in configs.into_iter().enumerate() {
        let Some(quota) = cfg.quota else { continue };
        let Some(fraction) = quota.remaining_fraction else {
            continue;
        };
        let remaining = (fraction * 100.0).round().clamp(0.0, 100.0) as u32;
        let label = cfg.label.unwrap_or_else(|| format!("MODELO {}", i + 1));
        let reset = quota.reset_time.as_ref().and_then(parse_reset);
        windows.push(WindowUsage::from_percent(
            &format!("model{i}"),
            &label,
            remaining,
            reset,
        ));
        if windows.len() >= 6 {
            break;
        }
    }
    windows
}

fn parse_reset(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        if let Ok(dt) = s.parse::<DateTime<Utc>>() {
            return Some(dt.with_timezone(&Local).to_rfc3339());
        }
        if let Ok(n) = s.parse::<i64>() {
            return reset_from_epoch(n);
        }
        return None;
    }
    if let Some(n) = v.as_i64() {
        return reset_from_epoch(n);
    }
    v.as_f64().and_then(|f| reset_from_epoch(f as i64))
}
