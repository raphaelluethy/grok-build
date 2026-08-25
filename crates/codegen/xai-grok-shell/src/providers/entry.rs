//! Shared `ModelEntry` constructor for injected provider catalogs.

use std::num::NonZeroU64;

use indexmap::IndexMap;
use xai_grok_sampler::ApiBackend;

use crate::agent::config::{EnvKeys, ModelEntry, ModelInfo};

pub(crate) struct ProviderModelSpec<'a> {
    pub catalog_id: &'a str,
    pub routing_slug: &'a str,
    pub display_name: &'a str,
    pub provider: &'a str,
    pub base_url: &'a str,
    pub api_backend: ApiBackend,
    pub api_key: String,
    pub env_key: Option<&'a str>,
    pub context_window: u64,
    pub extra_headers: IndexMap<String, String>,
    pub supports_reasoning: bool,
    pub max_completion_tokens: Option<u32>,
}

pub(crate) fn build_entry(spec: ProviderModelSpec<'_>) -> (String, ModelEntry) {
    let mut info = ModelInfo::fallback(spec.routing_slug);
    info.id = Some(spec.catalog_id.to_owned());
    info.model = spec.routing_slug.to_owned();
    info.name = Some(spec.display_name.to_owned());
    info.provider = Some(spec.provider.to_owned());
    info.base_url = spec.base_url.trim_end_matches('/').to_owned();
    info.api_backend = spec.api_backend;
    info.extra_headers = spec.extra_headers;
    info.context_window =
        NonZeroU64::new(spec.context_window.max(1)).unwrap_or(info.context_window);
    info.supported_in_api = true;
    info.user_selectable = true;
    info.hidden = false;
    info.supports_reasoning_effort = spec.supports_reasoning;
    info.max_completion_tokens = spec.max_completion_tokens;
    let entry = ModelEntry {
        info,
        api_key: Some(spec.api_key),
        env_key: spec.env_key.map(EnvKeys::single),
        auth_provider: None,
        api_base_url: None,
    };
    (spec.catalog_id.to_owned(), entry)
}
