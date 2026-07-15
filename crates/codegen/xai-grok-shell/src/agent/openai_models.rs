//! Built-in OpenAI / Codex models when the active provider is OpenAI.

use std::num::NonZeroU64;

use indexmap::IndexMap;
use xai_grok_sampler::AuthScheme;
use xai_grok_sampling_types::ApiBackend;

use super::config::{EndpointsConfig, ModelEntry};
use crate::auth::provider::{OPENAI_CODEX_API_BASE, ProviderId, active_provider};

/// Catalog entries for ChatGPT Codex OAuth (Responses API).
pub fn openai_builtin_models() -> IndexMap<String, ModelEntry> {
    let specs = [
        ("gpt-5.4", "GPT-5.4", 400_000u64),
        ("gpt-5.4-mini", "GPT-5.4 Mini", 200_000),
        ("gpt-5.3-codex", "GPT-5.3 Codex", 400_000),
        ("gpt-5.2", "GPT-5.2", 400_000),
    ];
    let endpoints = EndpointsConfig::default();
    let mut map = IndexMap::new();
    for (id, name, ctx) in specs {
        let mut entry = ModelEntry::fallback(id, &endpoints);
        entry.info.model = id.to_string();
        entry.info.name = Some(name.to_string());
        entry.info.base_url = OPENAI_CODEX_API_BASE.to_string();
        entry.info.api_backend = ApiBackend::Responses;
        entry.info.auth_scheme = AuthScheme::Bearer;
        entry.info.context_window = NonZeroU64::new(ctx).unwrap();
        entry.info.user_selectable = true;
        entry
            .info
            .extra_headers
            .insert("originator".into(), "grok_build".into());
        map.insert(id.to_string(), entry);
    }
    map
}

/// True when the active provider is OpenAI ChatGPT OAuth.
pub fn openai_provider_active() -> bool {
    active_provider() == ProviderId::Openai
}
