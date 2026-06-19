use super::http::agent;
use super::Provider;
use crate::model::{ProviderSnapshot, WindowUsage};
use crate::state::AppState;
use serde::Deserialize;

/// OpenRouter — unified gateway for GPT-4, Claude, Gemini, Llama and others.
/// Reads remaining credit from `GET /api/v1/auth/key` using the user's API key.
pub struct OpenRouterProvider;

#[derive(Deserialize)]
struct KeyResp {
    data: Option<KeyData>,
}

#[derive(Deserialize)]
struct KeyData {
    label: Option<String>,
    usage: Option<f64>,
    limit: Option<f64>,
    is_free_tier: Option<bool>,
}

impl Provider for OpenRouterProvider {
    fn id(&self) -> &'static str {
        "openrouter"
    }

    fn name(&self) -> &'static str {
        "OpenRouter"
    }

    fn collect(&self, state: &AppState) -> ProviderSnapshot {
        let Some(key) = state.openrouter_key.as_deref().filter(|k| !k.is_empty()) else {
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Adiciona a tua API key nas definições",
            );
        };

        match fetch(key) {
            Some(data) => self.build(data),
            None => ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Falha ao obter créditos (key inválida?)",
            ),
        }
    }
}

impl OpenRouterProvider {
    fn build(&self, data: KeyData) -> ProviderSnapshot {
        let free = data.is_free_tier.unwrap_or(false);
        let label = data.label.as_deref().unwrap_or("CRÉDITOS");

        let window = if free || data.limit.is_none() {
            WindowUsage::from_percent("credits", "CRÉDITOS ∞", 100, None)
        } else {
            let usage = data.usage.unwrap_or(0.0);
            let limit = data.limit.unwrap_or(1.0).max(0.001);
            let pct = ((1.0 - usage / limit) * 100.0).round().clamp(0.0, 100.0) as u32;
            let mut w = WindowUsage::from_percent("credits", label, pct, None);
            // Store USD amounts for tooltip (budget = limit cents, used = usage cents)
            w.budget = (limit * 100.0) as u64;
            w.used_tokens = (usage * 100.0) as u64;
            w
        };

        ProviderSnapshot {
            id: self.id().to_string(),
            name: self.name().to_string(),
            available: true,
            note: if free { Some("Plano gratuito".to_string()) } else { None },
            windows: vec![window],
            total_tokens: None,
            estimated_cost_usd: None,
            local_models: Vec::new(),
            active_sessions: Vec::new(),
        }
    }
}

fn fetch(key: &str) -> Option<KeyData> {
    let key = key.to_string();
    super::http::with_retry(3, 1, || {
        let mut resp = agent(false)
            .get("https://openrouter.ai/api/v1/auth/key")
            .header("Authorization", format!("Bearer {key}"))
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
        let parsed: KeyResp = serde_json::from_str(&text).ok()?;
        parsed.data
    })
}
