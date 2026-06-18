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

        let local_models: Vec<LocalModelInfo> = tags
            .into_iter()
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
            .collect();

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
            local_models,
        }
    }
}

fn fetch_tags() -> Option<Vec<TagModel>> {
    let url = format!("{BASE}/api/tags");
    let mut resp = agent(false).get(&url).call().ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp.body_mut().read_to_string().ok()?;
    let parsed: TagsResp = serde_json::from_str(&text).ok()?;
    Some(parsed.models.unwrap_or_default())
}

fn fetch_running() -> Option<Vec<String>> {
    let url = format!("{BASE}/api/ps");
    let mut resp = agent(false).get(&url).call().ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp.body_mut().read_to_string().ok()?;
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
