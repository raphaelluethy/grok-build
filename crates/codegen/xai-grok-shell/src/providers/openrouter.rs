//! OpenRouter catalog adapter (`https://openrouter.ai/api/v1`).

use indexmap::IndexMap;
use serde::Deserialize;
use xai_grok_sampler::ApiBackend;

use super::credentials::ProviderStore;
use super::entry::{ProviderModelSpec, build_entry};
use crate::agent::config::ModelEntry;

pub const API_URL: &str = "https://openrouter.ai/api/v1";
/// OpenRouter's unified OpenResponses-compatible request path (`/responses`).
pub const API_BACKEND: ApiBackend = ApiBackend::Responses;
pub const ENV_KEY: &str = "OPENROUTER_API_KEY";
pub const PROVIDER_LABEL: &str = "OpenRouter";

const REFERER: &str = "https://grok.com";
const TITLE: &str = "Grok";

pub fn catalog_entries(store: &ProviderStore) -> IndexMap<String, ModelEntry> {
    let Some(key) = super::credentials::resolve_api_key(store.openrouter.as_ref(), ENV_KEY) else {
        return IndexMap::new();
    };
    let mut out = IndexMap::new();
    insert_model(
        &mut out,
        "openrouter/auto",
        "openrouter/auto",
        "Auto Router",
        &key,
        2_000_000,
        false,
    );
    if cfg!(test) {
        return out;
    }
    match fetch_models(&key) {
        Ok(models) => {
            for model in models {
                let id = format!("openrouter/{}", model.id);
                if out.contains_key(&id) {
                    continue;
                }
                let display = model
                    .name
                    .rsplit(':')
                    .next()
                    .unwrap_or(&model.name)
                    .trim()
                    .to_owned();
                let reasoning = model
                    .supported_parameters
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case("reasoning"));
                insert_model(
                    &mut out,
                    &id,
                    &model.id,
                    &display,
                    &key,
                    model.context_length.unwrap_or(200_000).max(1),
                    reasoning,
                );
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "openrouter: failed to list models; offering auto router only");
        }
    }
    out
}

fn insert_model(
    out: &mut IndexMap<String, ModelEntry>,
    catalog_id: &str,
    routing_slug: &str,
    display_name: &str,
    api_key: &str,
    context_window: u64,
    supports_reasoning: bool,
) {
    let mut extra_headers = IndexMap::new();
    extra_headers.insert("HTTP-Referer".into(), REFERER.into());
    extra_headers.insert("X-OpenRouter-Title".into(), TITLE.into());
    let (id, entry) = build_entry(ProviderModelSpec {
        catalog_id,
        routing_slug,
        display_name,
        provider: PROVIDER_LABEL,
        base_url: API_URL,
        api_backend: API_BACKEND,
        api_key: api_key.to_owned(),
        env_key: Some(ENV_KEY),
        context_window,
        extra_headers,
        supports_reasoning,
        max_completion_tokens: None,
    });
    out.insert(id, entry);
}

#[derive(Deserialize)]
struct ListModelsResponse {
    data: Vec<ListedModel>,
}

#[derive(Deserialize)]
struct ListedModel {
    id: String,
    #[serde(default)]
    name: String,
    context_length: Option<u64>,
    #[serde(default)]
    supported_parameters: Vec<String>,
}

fn fetch_models(api_key: &str) -> anyhow::Result<Vec<ListedModel>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?;
    let url = format!("{API_URL}/models");
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("HTTP-Referer", REFERER)
        .header("X-OpenRouter-Title", TITLE)
        .send()?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} from {url}", resp.status());
    }
    let body: ListModelsResponse = resp.json()?;
    Ok(body.data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::credentials::{ApiKeyCreds, ProviderStore};

    #[test]
    fn no_key_yields_empty_catalog() {
        let store = ProviderStore::default();
        assert!(catalog_entries(&store).is_empty());
    }

    #[test]
    fn stored_key_injects_auto_router() {
        let mut store = ProviderStore::default();
        store.openrouter = Some(ApiKeyCreds {
            api_key: "sk-or-test".into(),
        });
        let catalog = catalog_entries(&store);
        assert!(catalog.contains_key("openrouter/auto"));
        let auto = &catalog["openrouter/auto"];
        assert_eq!(auto.info.provider.as_deref(), Some(PROVIDER_LABEL));
        assert_eq!(auto.info.base_url, API_URL);
        assert_eq!(auto.info.api_backend, ApiBackend::Responses);
        assert_eq!(
            auto.info
                .extra_headers
                .get("X-OpenRouter-Title")
                .map(String::as_str),
            Some(TITLE)
        );
        assert_eq!(auto.api_key.as_deref(), Some("sk-or-test"));
    }
}
