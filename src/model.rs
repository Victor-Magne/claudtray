use serde::Serialize;

/// Health status of a usage window. Mirrors the colour rules used by the
/// macOS ClaudeBar: >50% remaining = Healthy (green), 20-50% = Warning
/// (yellow), 1-19% = Critical (red), 0% / no data = Depleted (gray).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
    /// RFC3339 (local offset) timestamp of when the oldest usage in this
    /// rolling window expires. `None` if there is no activity.
    pub reset_at: Option<String>,
    /// When true the monitor applies auto-detected budgets (token-based
    /// providers like Claude). When false the window already carries a known
    /// remaining percentage from the provider (Codex/Antigravity/Copilot).
    #[serde(skip)]
    pub auto: bool,
}

impl WindowUsage {
    /// Build a window with only the raw measurement filled in. The budget,
    /// remaining percentage and status are computed later by the monitor via
    /// [`WindowUsage::finalize`] so the auto-detect logic lives in one place.
    pub fn raw(key: &str, label: &str, used_tokens: u64, reset_at: Option<String>) -> Self {
        Self {
            key: key.to_string(),
            label: label.to_string(),
            used_tokens,
            budget: 0,
            remaining_pct: 100,
            status: Status::Depleted,
            reset_at,
            auto: true,
        }
    }

    /// Build a window from a known remaining percentage (providers that report
    /// usage directly, e.g. Codex/Antigravity/Copilot). Not touched by the
    /// monitor's budget auto-detection.
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
            auto: false,
        }
    }

    /// Apply the auto-detected `budget` (max of the historical usage peak and a
    /// sane default floor, computed by the monitor) and derive the remaining
    /// percentage + status. An idle window reads ~100% (as the macOS ClaudeBar
    /// shows OPUS at 100% when unused); it only reaches 0% once usage exceeds
    /// the whole observed history.
    pub fn finalize(&mut self, budget: u64) {
        self.budget = budget;
        if budget == 0 {
            self.remaining_pct = 100;
            self.status = Status::Healthy;
            return;
        }
        self.remaining_pct = if self.used_tokens >= budget {
            0
        } else {
            (((budget - self.used_tokens) * 100) / budget) as u32
        };
        self.status = Status::from_remaining(self.remaining_pct, true);
    }
}

#[derive(Debug, Clone, Serialize)]
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
}

impl ProviderSnapshot {
    pub fn unavailable(id: &str, name: &str, note: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            available: false,
            note: Some(note.to_string()),
            windows: Vec::new(),
        }
    }
}

/// The full picture pushed to the dashboard on every refresh.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    /// RFC3339 (local offset) of when this snapshot was produced.
    pub updated_at: String,
    /// Active theme: "dark" | "light" | "system".
    pub theme: String,
    pub providers: Vec<ProviderSnapshot>,
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
