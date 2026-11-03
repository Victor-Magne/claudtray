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

    fn min_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(4) // local; just under the 5 s tick so jitter never skips one
    }

    fn name(&self) -> &'static str {
        "Antigravity"
    }

    fn collect(&self, _state: &AppState) -> ProviderSnapshot {
        let candidates = find_processes();
        if candidates.is_empty() {
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Abre o Antigravity para ver a quota",
            );
        }

        // `sysinfo` sometimes surfaces the language server's individual threads
        // as extra "processes" sharing the same cmdline (and thus the same CSRF
        // token) as the real one — but `netstat2` can only associate a listening
        // socket with the genuine process PID, never a bare thread id. Try every
        // matching candidate rather than trusting the first (HashMap-ordered) one.
        for (pid, token) in &candidates {
            for port in find_listen_ports(*pid) {
                if let Some(windows) = probe_summary(port, token) {
                    if !windows.is_empty() {
                        let plan = fetch_user_status(port, token).as_ref().and_then(plan_name);
                        return ProviderSnapshot {
                            id: self.id().to_string(),
                            name: self.name().to_string(),
                            available: true,
                            note: None,
                            windows,
                            total_tokens: None,
                            estimated_cost_usd: None,
                            usage: None,
                            local_models: Vec::new(),
                            active_sessions: Vec::new(),
                            stale_secs: None,
                            auth_failed: false,
                            retry_in_secs: None,
                            fetched_at: None,
                            plan,
                            partial: false,
                        };
                    }
                }
                if let Some(resp) = fetch_user_status(port, token) {
                    let plan = plan_name(&resp);
                    let windows = build_fallback_windows(resp);
                    if !windows.is_empty() {
                        return ProviderSnapshot {
                            id: self.id().to_string(),
                            name: self.name().to_string(),
                            available: true,
                            note: None,
                            windows,
                            total_tokens: None,
                            estimated_cost_usd: None,
                            usage: None,
                            local_models: Vec::new(),
                            active_sessions: Vec::new(),
                            stale_secs: None,
                            auth_failed: false,
                            retry_in_secs: None,
                            fetched_at: None,
                            plan,
                            partial: true,
                        };
                    }
                }
            }
        }

        ProviderSnapshot::unavailable(
            self.id(),
            self.name(),
            "Indisponível: o servidor do Antigravity não respondeu com quotas legíveis",
        )
    }
}

/// Locate every Antigravity language-server process (and its `--csrf_token`)
/// visible to `sysinfo`. May include duplicate entries for the same real
/// process (see the comment in `collect`), which callers must tolerate.
fn find_processes() -> Vec<(u32, String)> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );

    let mut out = Vec::new();
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
            out.push((pid.as_u32(), token));
        }
    }
    out
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
    ports.sort_by_key(|p| std::cmp::Reverse(p.0));
    ports.into_iter().map(|(_, p)| p).collect()
}

fn fetch_user_status(port: u16, token: &str) -> Option<Resp> {
    // HTTPS only. The language server uses a self-signed localhost certificate
    // (hence `agent(true)` to skip CA validation), but we never fall back to
    // plaintext HTTP: doing so would leak the CSRF token in clear over loopback,
    // where any local sniffer or process could capture it.
    let url = format!(
        "https://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/GetUserStatus"
    );
    let mut resp = agent(true)
        .post(&url)
        .header("Content-Type", "application/json")
        .header("X-Codeium-Csrf-Token", token)
        .header("Connect-Protocol-Version", "1")
        .send(METADATA_BODY)
        .ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp
        .body_mut()
        .with_config()
        .limit(super::http::MAX_BODY_BYTES)
        .read_to_string()
        .ok()?;
    serde_json::from_str::<Resp>(&text).ok()
}

fn probe_summary(port: u16, token: &str) -> Option<Vec<WindowUsage>> {
    let url = format!(
        "https://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary"
    );
    let mut resp = agent(true)
        .post(&url)
        .header("Content-Type", "application/json")
        .header("X-Codeium-Csrf-Token", token)
        .header("Connect-Protocol-Version", "1")
        .send(METADATA_BODY)
        .ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp
        .body_mut()
        .with_config()
        .limit(super::http::MAX_BODY_BYTES)
        .read_to_string()
        .ok()?;
    let parsed = serde_json::from_str::<QuotaSummaryResp>(&text).ok()?;
    let windows = build_summary_windows(parsed);
    if windows.is_empty() {
        None
    } else {
        Some(windows)
    }
}

// ---- Response model ----

#[derive(Debug, Deserialize)]
struct Resp {
    #[serde(rename = "userStatus")]
    user_status: Option<UserStatus>,
}

#[derive(Debug, Deserialize)]
struct UserStatus {
    #[serde(rename = "planStatus")]
    plan_status: Option<PlanStatus>,
    #[serde(rename = "cascadeModelConfigData")]
    cascade: Option<Cascade>,
}

#[derive(Debug, Deserialize)]
struct PlanStatus {
    #[serde(rename = "planInfo")]
    plan_info: Option<PlanInfo>,
}

#[derive(Debug, Deserialize)]
struct PlanInfo {
    #[serde(rename = "planName")]
    plan_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Cascade {
    #[serde(rename = "clientModelConfigs")]
    configs: Option<Vec<ModelConfig>>,
}

#[derive(Debug, Deserialize)]
struct ModelConfig {
    #[serde(rename = "quotaInfo")]
    quota: Option<QuotaInfo>,
}

#[derive(Debug, Deserialize)]
struct QuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct QuotaSummaryResp {
    response: Option<QuotaSummaryResponse>,
}

#[derive(Debug, Deserialize)]
struct QuotaSummaryResponse {
    groups: Option<Vec<QuotaGroup>>,
}

#[derive(Debug, Deserialize)]
struct QuotaGroup {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    buckets: Option<Vec<QuotaBucket>>,
}

#[derive(Debug, Deserialize)]
struct QuotaBucket {
    #[serde(rename = "bucketId")]
    bucket_id: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    window: Option<String>,
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<serde_json::Value>,
}

fn build_summary_windows(resp: QuotaSummaryResp) -> Vec<WindowUsage> {
    let groups = resp.response.and_then(|r| r.groups).unwrap_or_default();
    let mut windows = Vec::new();
    let mut bucket_idx = 0usize;

    for group in groups {
        let group_name = group.display_name.unwrap_or_default();
        let buckets = group.buckets.unwrap_or_default();
        for bucket in buckets {
            let i = bucket_idx;
            bucket_idx += 1;
            let Some(fraction) = bucket.remaining_fraction else {
                continue;
            };
            let key = bucket
                .bucket_id
                .clone()
                .unwrap_or_else(|| format!("bucket{i}"));
            let label = bucket
                .display_name
                .or(bucket.window.clone())
                .unwrap_or_else(|| key.clone());
            let remaining = (fraction * 100.0).round().clamp(0.0, 100.0) as u32;
            let reset = bucket.reset_time.as_ref().and_then(parse_reset);
            let mut w = WindowUsage::from_percent(&key, &label, remaining, reset);
            w.window_secs = match bucket.window.as_deref() {
                Some("5h") => Some(18_000),
                Some("weekly") => Some(604_800),
                _ => None,
            };
            w.group = Some(group_name.clone());
            windows.push(w);
        }
    }
    windows
}

fn plan_name(resp: &Resp) -> Option<String> {
    resp.user_status
        .as_ref()
        .and_then(|u| u.plan_status.as_ref())
        .and_then(|p| p.plan_info.as_ref())
        .and_then(|i| i.plan_name.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn build_fallback_windows(resp: Resp) -> Vec<WindowUsage> {
    let configs = resp
        .user_status
        .and_then(|u| u.cascade)
        .and_then(|c| c.configs)
        .unwrap_or_default();

    struct Group {
        reset_at: Option<String>,
        remaining_pct: u32,
        count: usize,
    }

    let mut groups: Vec<Group> = Vec::new();

    for cfg in configs {
        let Some(quota) = cfg.quota else {
            continue;
        };
        let reset_at = quota.reset_time.as_ref().and_then(parse_reset);
        let remaining = match quota.remaining_fraction {
            Some(f) => (f * 100.0).round().clamp(0.0, 100.0) as u32,
            None if quota.reset_time.as_ref().is_some_and(|v| !v.is_null()) => 0,
            None => continue,
        };

        if let Some(g) = groups
            .iter_mut()
            .find(|g| g.reset_at == reset_at && g.remaining_pct == remaining)
        {
            g.count += 1;
        } else {
            groups.push(Group {
                reset_at,
                remaining_pct: remaining,
                count: 1,
            });
        }
    }

    groups
        .into_iter()
        .enumerate()
        .map(|(n, g)| {
            let key = format!("fallback{n}");
            let label = if g.count == 1 {
                "Quota · 1 modelo".to_string()
            } else {
                format!("Quota · {} modelos", g.count)
            };
            let mut w = WindowUsage::from_percent(&key, &label, g.remaining_pct, g.reset_at);
            w.group = None;
            w.window_secs = None;
            w
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;

    #[test]
    fn fallback_windows_groups_by_reset_and_remaining_and_reads_plan_name() {
        let resp: Resp = serde_json::from_str(include_str!(
            "../../tests/fixtures/antigravity/user_status_plan.json"
        ))
        .unwrap();

        assert_eq!(plan_name(&resp), Some("Pro".to_string()));

        let windows = build_fallback_windows(resp);
        assert_eq!(windows.len(), 3, "models without fraction and reset are ignored");

        // Window 0: (reset 2026-11-03T13:03:37Z, 80%) with label "Quota · 2 modelos"
        assert_eq!(windows[0].key, "fallback0");
        assert_eq!(windows[0].label, "Quota · 2 modelos");
        assert_eq!(windows[0].remaining_pct, 80);
        assert_eq!(windows[0].status, Status::Healthy);
        assert_eq!(
            windows[0].reset_at,
            parse_reset(&serde_json::json!("2026-11-03T13:03:37Z"))
        );
        assert!(windows[0].group.is_none());
        assert!(windows[0].window_secs.is_none());

        // Window 1: (reset 2026-11-10T08:03:37Z, 80%) "Quota · 1 modelo"
        assert_eq!(windows[1].key, "fallback1");
        assert_eq!(windows[1].label, "Quota · 1 modelo");
        assert_eq!(windows[1].remaining_pct, 80);
        assert_eq!(windows[1].status, Status::Healthy);
        assert_eq!(
            windows[1].reset_at,
            parse_reset(&serde_json::json!("2026-11-10T08:03:37Z"))
        );
        assert!(windows[1].group.is_none());
        assert!(windows[1].window_secs.is_none());

        // Window 2: (reset 2026-11-03T13:03:37Z, 0%, Status::Depleted) "Quota · 1 modelo"
        assert_eq!(windows[2].key, "fallback2");
        assert_eq!(windows[2].label, "Quota · 1 modelo");
        assert_eq!(windows[2].remaining_pct, 0);
        assert_eq!(windows[2].status, Status::Depleted);
        assert_eq!(
            windows[2].reset_at,
            parse_reset(&serde_json::json!("2026-11-03T13:03:37Z"))
        );
        assert!(windows[2].group.is_none());
        assert!(windows[2].window_secs.is_none());
    }

    #[test]
    fn fallback_windows_groups_legacy_user_status_fixture() {
        let resp: Resp = serde_json::from_str(include_str!(
            "../../tests/fixtures/antigravity/user_status.json"
        ))
        .unwrap();

        assert_eq!(plan_name(&resp), None);

        let windows = build_fallback_windows(resp);
        assert_eq!(windows.len(), 4);

        assert_eq!(windows[0].key, "fallback0");
        assert_eq!(windows[0].label, "Quota · 1 modelo");
        assert_eq!(windows[0].remaining_pct, 73);
        assert_eq!(windows[0].status, Status::Healthy);
        assert_eq!(
            windows[0].reset_at,
            parse_reset(&serde_json::json!("2026-01-16T00:00:00Z"))
        );

        assert_eq!(windows[1].key, "fallback1");
        assert_eq!(windows[1].label, "Quota · 1 modelo");
        assert_eq!(windows[1].remaining_pct, 10);
        assert_eq!(windows[1].status, Status::Critical);
        assert!(windows[1].reset_at.is_none());

        assert_eq!(windows[2].key, "fallback2");
        assert_eq!(windows[2].label, "Quota · 1 modelo");
        assert_eq!(windows[2].remaining_pct, 0);
        assert_eq!(windows[2].status, Status::Depleted);
        assert!(windows[2].reset_at.is_none());

        assert_eq!(windows[3].key, "fallback3");
        assert_eq!(windows[3].label, "Quota · 4 modelos");
        assert_eq!(windows[3].remaining_pct, 50);
        assert_eq!(windows[3].status, Status::Healthy);
        assert!(windows[3].reset_at.is_none());
    }

    #[test]
    fn user_status_plan_does_not_leak_identity_data() {
        let text = include_str!("../../tests/fixtures/antigravity/user_status_plan.json");
        let resp: Resp = serde_json::from_str(text).unwrap();

        let debug_resp = format!("{resp:?}");
        assert!(!debug_resp.contains("fixture-user@example.invalid"));
        assert!(!debug_resp.contains("Fixture User"));
        assert!(!debug_resp.contains("avatar"));

        let plan = plan_name(&resp);
        let windows = build_fallback_windows(resp);
        let snapshot = ProviderSnapshot {
            id: "antigravity".to_string(),
            name: "Antigravity".to_string(),
            available: true,
            note: None,
            windows,
            total_tokens: None,
            estimated_cost_usd: None,
            usage: None,
            local_models: Vec::new(),
            active_sessions: Vec::new(),
            stale_secs: None,
            auth_failed: false,
            retry_in_secs: None,
            fetched_at: None,
            plan,
            partial: true,
        };

        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains("fixture-user@example.invalid"));
        assert!(!serialized.contains("Fixture User"));
        assert!(!serialized.contains("avatar"));
    }

    #[test]
    fn parse_reset_accepts_iso_and_epoch() {
        assert!(parse_reset(&serde_json::json!("2026-01-16T00:00:00Z")).is_some());
        assert!(parse_reset(&serde_json::json!(1_768_500_000_i64)).is_some());
        assert!(parse_reset(&serde_json::json!("1768500000")).is_some());
        assert!(parse_reset(&serde_json::json!("not-a-date")).is_none());
    }

    #[test]
    fn quota_summary_builds_windows_with_groups_and_durations() {
        let resp: QuotaSummaryResp = serde_json::from_str(include_str!(
            "../../tests/fixtures/antigravity/quota_summary.json"
        ))
        .unwrap();
        let windows = build_summary_windows(resp);

        assert_eq!(windows.len(), 5);

        // Group names in server order
        assert_eq!(windows[0].group.as_deref(), Some("Gemini Models"));
        assert_eq!(windows[1].group.as_deref(), Some("Gemini Models"));
        assert_eq!(windows[2].group.as_deref(), Some("Claude and GPT models"));
        assert_eq!(windows[3].group.as_deref(), Some("Claude and GPT models"));
        assert_eq!(windows[4].group.as_deref(), Some("Gemini 4 Argon"));

        // Keys are the bucketIds
        assert_eq!(windows[0].key, "gemini-weekly");
        assert_eq!(windows[1].key, "gemini-5h");
        assert_eq!(windows[2].key, "3p-weekly");
        assert_eq!(windows[3].key, "3p-5h");
        assert_eq!(windows[4].key, "gemini-argon-wtu-weekly");

        // window_secs 18000 for "5h" and 604800 for "weekly"
        assert_eq!(windows[0].window_secs, Some(604_800));
        assert_eq!(windows[1].window_secs, Some(18_000));
        assert_eq!(windows[2].window_secs, Some(604_800));
        assert_eq!(windows[3].window_secs, Some(18_000));
        assert_eq!(windows[4].window_secs, Some(604_800));

        // gemini-weekly remaining_pct is 100 (0.9996752 rounds to 100)
        assert_eq!(windows[0].remaining_pct, 100);

        // reset_at is Some
        for w in &windows {
            assert!(w.reset_at.is_some(), "window {} should have reset_at", w.key);
        }
    }

    #[test]
    fn quota_summary_edge_skips_missing_fraction_and_handles_statuses() {
        let resp: QuotaSummaryResp = serde_json::from_str(include_str!(
            "../../tests/fixtures/antigravity/quota_summary_edge.json"
        ))
        .unwrap();
        let windows = build_summary_windows(resp);

        // The bucket without remainingFraction (3p-weekly) is skipped -> 4 windows
        assert_eq!(windows.len(), 4);
        assert!(!windows.iter().any(|w| w.key == "3p-weekly"));

        // remaining 15 -> Critical and 0 -> Depleted
        let gemini_5h = windows.iter().find(|w| w.key == "gemini-5h").unwrap();
        assert_eq!(gemini_5h.remaining_pct, 15);
        assert_eq!(gemini_5h.status, Status::Critical);

        let p3_5h = windows.iter().find(|w| w.key == "3p-5h").unwrap();
        assert_eq!(p3_5h.remaining_pct, 0);
        assert_eq!(p3_5h.status, Status::Depleted);

        // the "monthly" window gets window_secs None and keeps its label; no panic
        let argon = windows
            .iter()
            .find(|w| w.key == "gemini-argon-wtu-weekly")
            .unwrap();
        assert_eq!(argon.window_secs, None);
        assert_eq!(argon.label, "Gemini 4 Argon Weekly Limit");
    }

    #[test]
    fn malformed_input_gives_empty_vec_without_panicking() {
        let cases = ["{}", r#"{"response":{}}"#, r#"{"response":{"groups":[{}]}}"#];
        for case in cases {
            let resp: QuotaSummaryResp = serde_json::from_str(case).unwrap();
            let windows = build_summary_windows(resp);
            assert!(windows.is_empty(), "expected empty windows for {case}");
        }
    }

    #[test]
    fn parsed_response_debug_does_not_contain_email() {
        let json = r#"{
            "email": "root@example.com",
            "name": "Root Name",
            "profilePictureUrl": "https://example.com/avatar.png",
            "response": {
                "email": "resp@example.com",
                "groups": [
                    {
                        "email": "group@example.com",
                        "displayName": "Group 1",
                        "buckets": [
                            {
                                "email": "bucket@example.com",
                                "bucketId": "b1",
                                "displayName": "Bucket 1",
                                "window": "5h",
                                "remainingFraction": 0.5,
                                "resetTime": "2026-11-03T13:06:29Z"
                            }
                        ]
                    }
                ]
            }
        }"#;
        let parsed: QuotaSummaryResp = serde_json::from_str(json).unwrap();
        let debug = format!("{parsed:?}");
        assert!(
            !debug.contains('@'),
            "Debug output leaked '@' character from identity fields: {debug}"
        );
    }
}
