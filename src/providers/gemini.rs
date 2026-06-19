use super::http::agent;
use super::Provider;
use crate::model::ProviderSnapshot;
use crate::state::AppState;
use serde::Deserialize;

/// Google Gemini / AI Studio. Verifies the API key and lists available models.
/// The public API does not expose per-key quota, so we show connectivity status
/// and the number of available generation models.
pub struct GeminiProvider;

#[derive(Deserialize)]
struct ModelsResp {
    models: Option<Vec<ModelEntry>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelEntry {
    supported_generation_methods: Option<Vec<String>>,
}

impl Provider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn name(&self) -> &'static str {
        "Gemini"
    }

    fn collect(&self, state: &AppState) -> ProviderSnapshot {
        let Some(key) = state.gemini_key.as_deref().filter(|k| !k.is_empty()) else {
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Adiciona a tua API key do Google AI Studio nas definições",
            );
        };

        match fetch(key) {
            Some((total, generative)) => ProviderSnapshot {
                id: self.id().to_string(),
                name: self.name().to_string(),
                available: true,
                note: Some(format!("{generative} modelo(s) de geração · {total} total")),
                windows: Vec::new(),
                total_tokens: None,
                estimated_cost_usd: None,
                local_models: Vec::new(),
                active_sessions: Vec::new(),
            },
            None => ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Falha na ligação (API key inválida?)",
            ),
        }
    }
}

fn fetch(key: &str) -> Option<(usize, usize)> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models?key={key}&pageSize=50"
    );
    let url_clone = url.clone();
    super::http::with_retry(3, 1, || {
        let mut resp = agent(false)
            .get(&url_clone)
            .header("User-Agent", "CloudTray")
            .call()
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
        let parsed: ModelsResp = serde_json::from_str(&text).ok()?;
        let models = parsed.models.unwrap_or_default();
        let total = models.len();
        let generative = models.iter().filter(|m| {
            m.supported_generation_methods
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|m| m == "generateContent")
        }).count();
        Some((total, generative))
    })
}
