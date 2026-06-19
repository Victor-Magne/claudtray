use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use zeroize::Zeroize;

/// Persisted application state, stored at
/// `%APPDATA%/CloudTray/state.json` (via `dirs::config_dir`).
///
/// Holds the user's theme choice, the optional Copilot token and — crucially
/// for the auto-detected budgets — the highest token usage ever observed for
/// each (provider, window) pair.
///
/// The credential fields (`copilot_token`, `openrouter_key`, `gemini_key`,
/// `http_proxy`) are encrypted with Windows DPAPI on disk (see the [`secret`]
/// serde module) and zeroized in memory when replaced or dropped, so secrets
/// are never written in plaintext nor left lingering in freed heap memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    #[serde(default = "default_theme")]
    pub theme: String,

    /// Observed peak usage keyed by "<provider_id>:<window_key>".
    #[serde(default)]
    pub observed_max: HashMap<String, u64>,

    #[serde(with = "secret", default)]
    pub copilot_token: Option<String>,

    /// OpenRouter API key (sk-or-…).
    #[serde(with = "secret", default)]
    pub openrouter_key: Option<String>,

    /// Google AI Studio API key (AIza…).
    #[serde(with = "secret", default)]
    pub gemini_key: Option<String>,

    /// HTTP/HTTPS proxy URL, e.g. "http://proxy.corp:8080".
    /// When set, also exported as HTTP_PROXY / HTTPS_PROXY for child processes.
    #[serde(with = "secret", default)]
    pub http_proxy: Option<String>,

    /// Usage history: up to 288 points sampled every 5 minutes (24 h).
    #[serde(default)]
    pub history: Vec<crate::model::UsagePoint>,

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
            openrouter_key: None,
            gemini_key: None,
            http_proxy: None,
            history: Vec::new(),
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

    /// Replace a credential field, zeroizing whatever it previously held so the
    /// old secret does not survive in freed memory. `value` empty ⇒ `None`.
    pub fn set_secret(field: &mut Option<String>, value: &str) {
        if let Some(old) = field.as_mut() {
            old.zeroize();
        }
        *field = if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        };
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        for field in [
            &mut self.copilot_token,
            &mut self.openrouter_key,
            &mut self.gemini_key,
            &mut self.http_proxy,
        ] {
            if let Some(s) = field.as_mut() {
                s.zeroize();
            }
        }
    }
}

/// Serde adapter that transparently encrypts credential fields with Windows
/// DPAPI on disk and decrypts them on load. Legacy plaintext values written by
/// older builds are still read (migration), then re-encrypted on the next save.
mod secret {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<String>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            None => s.serialize_none(),
            Some(plain) => match crate::dpapi::protect(plain.as_bytes()) {
                Some(blob) => s.serialize_some(&crate::dpapi::to_hex(&blob)),
                // If DPAPI is unavailable, never fall back to plaintext on disk.
                None => s.serialize_none(),
            },
        }
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Option<String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let stored = Option::<String>::deserialize(d)?;
        Ok(stored.map(|s| {
            if let Some(blob) = crate::dpapi::from_hex(&s) {
                if let Some(plain) = crate::dpapi::unprotect(&blob) {
                    if let Ok(txt) = String::from_utf8(plain) {
                        return txt;
                    }
                }
            }
            // Not a DPAPI blob → legacy plaintext; keep it (re-encrypted on save).
            s
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_encrypted_on_disk_and_recovered_on_load() {
        let mut s = AppState::default();
        s.copilot_token = Some("ghp_super_secret_value".to_string());

        let json = serde_json::to_string(&s).unwrap();
        // The plaintext token must NOT appear anywhere in the serialized state.
        assert!(
            !json.contains("ghp_super_secret_value"),
            "plaintext secret leaked into state.json: {json}"
        );

        let loaded: AppState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.copilot_token.as_deref(), Some("ghp_super_secret_value"));
    }

    #[test]
    fn legacy_plaintext_state_still_loads() {
        // A state.json written by an older build, with a plaintext token.
        let legacy = r#"{"theme":"dark","copilot_token":"ghp_legacy_plain"}"#;
        let loaded: AppState = serde_json::from_str(legacy).unwrap();
        assert_eq!(loaded.copilot_token.as_deref(), Some("ghp_legacy_plain"));
    }
}
