use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Persisted application state, stored at
/// `%APPDATA%/CloudTray/state.json` (via `dirs::config_dir`).
///
/// Holds the user's theme choice, the optional Copilot token and — crucially
/// for the auto-detected budgets — the highest token usage ever observed for
/// each (provider, window) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    #[serde(default = "default_theme")]
    pub theme: String,

    /// Observed peak usage keyed by "<provider_id>:<window_key>".
    #[serde(default)]
    pub observed_max: HashMap<String, u64>,

    #[serde(default)]
    pub copilot_token: Option<String>,

    #[serde(default)]
    pub last_snapshot: Option<crate::model::Snapshot>,
}

fn default_theme() -> String {
    "dark".to_string()
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            observed_max: HashMap::new(),
            copilot_token: None,
            last_snapshot: None,
        }
    }
}

impl AppState {
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("CloudTray").join("state.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, s);
        }
    }
}
