use super::http::agent;
use super::Provider;
use crate::model::{LocalModelInfo, ProviderSnapshot};
use crate::state::AppState;
use serde::Deserialize;

/// Ollama local model runtime. Queries the Ollama REST API on localhost:11434
/// to discover installed models and which are currently loaded in memory.
pub struct OllamaProvider;

const BASE: &str = "http://127.0.0.1:11434";

#[derive(Deserialize)]
struct TagsResp {
    models: Option<Vec<TagModel>>,
}

#[derive(Deserialize)]
struct TagModel {
    name: Option<String>,
    size: Option<u64>,
    details: Option<ModelDetails>,
}

#[derive(Deserialize)]
struct PsResp {
    models: Option<Vec<PsModel>>,
}

#[derive(Deserialize)]
struct PsModel {
    name: Option<String>,
}

#[derive(Deserialize)]
struct ModelDetails {
    parameter_size: Option<String>,
    quantization_level: Option<String>,
}

impl Provider for OllamaProvider {
    fn id(&self) -> &'static str {
        "ollama"
    }

    fn name(&self) -> &'static str {
        "Ollama"
    }

    fn collect(&self, _state: &AppState) -> ProviderSnapshot {
        let tags = match fetch_tags() {
            Some(t) => t,
            None => {
                return ProviderSnapshot::unavailable(
                    self.id(),
                    self.name(),
                    "Ollama não está a correr",
                )
            }
        };

        if tags.is_empty() {
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Sem modelos instalados",
            );
        }

        let running = fetch_running().unwrap_or_default();
        let local_models = merge_models(tags, &running);

        let note = format!(
            "{} modelo(s) · {} a correr",
            local_models.len(),
            local_models.iter().filter(|m| m.loaded).count()
        );

        ProviderSnapshot {
            id: self.id().to_string(),
            name: self.name().to_string(),
            available: true,
            note: Some(note),
            windows: Vec::new(),
            total_tokens: None,
            estimated_cost_usd: None,
            local_models,
            active_sessions: Vec::new(),
        }
    }
}

fn fetch_tags() -> Option<Vec<TagModel>> {
    let url = format!("{BASE}/api/tags");
    let mut resp = agent(false).get(&url).call().ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp
        .body_mut()
        .with_config()
        .limit(super::http::MAX_BODY_BYTES)
        .read_to_string()
        .ok()?;
    let parsed: TagsResp = serde_json::from_str(&text).ok()?;
    Some(parsed.models.unwrap_or_default())
}

/// Turn the `/api/tags` list into [`LocalModelInfo`], flagging the ones whose
/// name appears in the `/api/ps` running set.
fn merge_models(tags: Vec<TagModel>, running: &[String]) -> Vec<LocalModelInfo> {
    tags.into_iter()
        .map(|m| {
            let name = m.name.unwrap_or_default();
            let loaded = running.iter().any(|r| r == &name);
            LocalModelInfo {
                loaded,
                size_bytes: m.size.unwrap_or(0),
                parameter_size: m.details.as_ref().and_then(|d| d.parameter_size.clone()),
                quantization: m.details.as_ref().and_then(|d| d.quantization_level.clone()),
                name,
            }
        })
        .collect()
}

fn fetch_running() -> Option<Vec<String>> {
    let url = format!("{BASE}/api/ps");
    let mut resp = agent(false).get(&url).call().ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp
        .body_mut()
        .with_config()
        .limit(super::http::MAX_BODY_BYTES)
        .read_to_string()
        .ok()?;
    let parsed: PsResp = serde_json::from_str(&text).ok()?;
    Some(
        parsed
            .models
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| m.name)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_models_flags_running_models() {
        let tags: TagsResp =
            serde_json::from_str(include_str!("../../tests/fixtures/ollama/tags.json")).unwrap();
        let ps: PsResp =
            serde_json::from_str(include_str!("../../tests/fixtures/ollama/ps.json")).unwrap();
        let running: Vec<String> =
            ps.models.unwrap_or_default().into_iter().filter_map(|m| m.name).collect();

        let merged = merge_models(tags.models.unwrap_or_default(), &running);
        assert_eq!(merged.len(), 2);

        let llama = &merged[0];
        assert_eq!(llama.name, "llama3:8b");
        assert!(llama.loaded);
        assert_eq!(llama.size_bytes, 4_700_000_000);
        assert_eq!(llama.parameter_size.as_deref(), Some("8B"));

        assert_eq!(merged[1].name, "mistral:7b");
        assert!(!merged[1].loaded);
    }
}
