use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Health status of a usage window. Mirrors the colour rules used by the
/// macOS ClaudeBar: >50% remaining = Healthy (green), 20-50% = Warning
/// (yellow), 1-19% = Critical (red), 0% / no data = Depleted (gray).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Healthy,
    Warning,
    Critical,
    Depleted,
}

impl Status {
    pub fn from_remaining(pct: u32, has_data: bool) -> Status {
        if !has_data {
            return Status::Depleted;
        }
        match pct {
            0 => Status::Depleted,
            1..=19 => Status::Critical,
            20..=49 => Status::Warning,
            _ => Status::Healthy,
        }
    }

    /// Higher rank == worse. Used to pick the worst status for the tray icon.
    pub fn rank(self) -> u8 {
        match self {
            Status::Healthy => 0,
            Status::Warning => 1,
            Status::Depleted => 2,
            Status::Critical => 3,
        }
    }
}

/// A single usage window (e.g. SESSION / WEEKLY / OPUS).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowUsage {
    /// Stable key, e.g. "session" | "weekly" | "opus".
    pub key: String,
    /// Display label, e.g. "SESSION".
    pub label: String,
    pub used_tokens: u64,
    /// Auto-detected budget (observed peak). 0 means "unknown yet".
    pub budget: u64,
    pub remaining_pct: u32,
    pub status: Status,
    /// RFC3339 (local offset) timestamp of when the window resets. `None` if
    /// unknown.
    pub reset_at: Option<String>,
}

impl WindowUsage {
    /// Build a window from a known remaining percentage. Every provider now
    /// reports usage directly (Claude/Codex/Antigravity/Copilot via their APIs
    /// or local rate-limit snapshots).
    pub fn from_percent(key: &str, label: &str, remaining_pct: u32, reset_at: Option<String>) -> Self {
        let remaining_pct = remaining_pct.min(100);
        Self {
            key: key.to_string(),
            label: label.to_string(),
            used_tokens: 0,
            budget: 0,
            remaining_pct,
            status: Status::from_remaining(remaining_pct, true),
            reset_at,
        }
    }
}

/// One usage sample for the history sparkline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsagePoint {
    /// RFC3339 timestamp when the sample was taken.
    pub at: String,
    /// "provider_id:window_key" → remaining_pct (0–100).
    pub values: HashMap<String, u32>,
}

/// An active Claude Code session detected from a running IDE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSession {
    /// IDE display name, e.g. "Visual Studio Code".
    pub ide: String,
    /// Last path component of the primary workspace folder.
    pub workspace: String,
}

/// Info about a locally installed model (e.g. from Ollama).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelInfo {
    pub name: String,
    pub size_bytes: u64,
    /// Whether this model is currently loaded in memory (VRAM).
    pub loaded: bool,
    pub parameter_size: Option<String>,
    pub quantization: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSnapshot {
    /// Stable id, e.g. "claude".
    pub id: String,
    /// Display name, e.g. "Claude".
    pub name: String,
    /// Whether usage data was found for this provider on this machine.
    pub available: bool,
    /// Optional human hint shown when `available` is false (e.g. "Não detetado").
    pub note: Option<String>,
    pub windows: Vec<WindowUsage>,
    /// Total tokens consumed (last 30 days), parsed from local log files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Estimated cost in USD for the last 30 days (based on model pricing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    /// Locally installed models (Ollama and similar runtimes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_models: Vec<LocalModelInfo>,
    /// Active Claude Code sessions (IDE integrations), detected from lock files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_sessions: Vec<ActiveSession>,
}

impl ProviderSnapshot {
    pub fn unavailable(id: &str, name: &str, note: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            available: false,
            note: Some(note.to_string()),
            windows: Vec::new(),
            total_tokens: None,
            estimated_cost_usd: None,
            local_models: Vec::new(),
            active_sessions: Vec::new(),
        }
    }
}

/// The full picture pushed to the dashboard on every refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// RFC3339 (local offset) of when this snapshot was produced.
    pub updated_at: String,
    /// Active theme: "dark" | "light" | "system".
    pub theme: String,
    pub providers: Vec<ProviderSnapshot>,
    /// Sparkline history: "provider_id:window_key" → oldest-first Vec of remaining_pct.
    #[serde(default)]
    pub history: HashMap<String, Vec<u32>>,
}

impl Snapshot {
    /// Worst status across all available providers/windows — drives the tray icon.
    pub fn worst_status(&self) -> Status {
        let mut worst = Status::Healthy;
        for p in &self.providers {
            if !p.available {
                continue;
            }
            for w in &p.windows {
                if w.status.rank() > worst.rank() {
                    worst = w.status;
                }
            }
        }
        worst
    }
}
