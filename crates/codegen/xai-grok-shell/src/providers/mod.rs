//! Third-party LLM provider adapters (OpenRouter and ChatGPT).
//!
//! Models are injected into the existing catalog when credentials exist.
//! They reuse the current sampling backends (`chat_completions`, `responses`,
//! `messages`) — no new protocol.

pub mod credentials;
mod entry;
pub mod openai_subscribed;
pub mod openrouter;

use indexmap::IndexMap;

use crate::agent::config::{ModelEntry, ModelInfo, UiConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderVisibility {
    pub openrouter: bool,
    pub chatgpt: bool,
}

impl Default for ProviderVisibility {
    fn default() -> Self {
        Self {
            openrouter: true,
            chatgpt: true,
        }
    }
}

impl From<&UiConfig> for ProviderVisibility {
    fn from(ui: &UiConfig) -> Self {
        Self {
            openrouter: ui.show_openrouter_models.unwrap_or(true),
            chatgpt: ui.show_chatgpt_models.unwrap_or(true),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderId {
    Xai,
    OpenRouter,
    ChatGpt,
}

impl ProviderId {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "xai" | "x.ai" | "grok" => Some(Self::Xai),
            "openrouter" | "or" => Some(Self::OpenRouter),
            "chatgpt" | "openai-subscribed" | "openai_subscribed" | "codex" => Some(Self::ChatGpt),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xai => "xai",
            Self::OpenRouter => "openrouter",
            Self::ChatGpt => "chatgpt",
        }
    }
}

/// Infer a short provider label from an explicit value or `base_url`.
pub fn infer_provider(base_url: &str, explicit: Option<&str>) -> String {
    if let Some(label) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return label.to_owned();
    }
    let url = base_url.trim();
    if url.is_empty() {
        return "xAI".to_owned();
    }
    let lower = url.to_ascii_lowercase();
    if lower.contains("openrouter.ai") {
        return "OpenRouter".to_owned();
    }
    if lower.contains("chatgpt.com") || lower.contains("backend-api/codex") {
        return "ChatGPT Subscription".to_owned();
    }
    if lower.contains("api.openai.com") || lower.contains("openai.com") {
        return "OpenAI".to_owned();
    }
    if lower.contains("anthropic.com") {
        return "Anthropic".to_owned();
    }
    if lower.contains("x.ai") || lower.contains("grok.com") {
        return "xAI".to_owned();
    }
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "Custom".to_owned())
}

pub fn fill_missing_provider(info: &mut ModelInfo) {
    if info
        .provider
        .as_deref()
        .map(str::trim)
        .is_none_or(|s| s.is_empty())
    {
        info.provider = Some(infer_provider(&info.base_url, None));
    }
}

/// Whether requests to this endpoint carry credentials owned by one of the
/// connected provider adapters rather than the ambient xAI login.
pub fn endpoint_uses_provider_credentials(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url.trim()) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let path = url.path().trim_end_matches('/');
    match host.to_ascii_lowercase().as_str() {
        "openrouter.ai" => path == "/api/v1" || path.starts_with("/api/v1/"),
        "chatgpt.com" => path == "/backend-api/codex" || path.starts_with("/backend-api/codex/"),
        _ => false,
    }
}

/// Merge connected-provider catalogs into `catalog`. Existing keys (bundled,
/// prefetched, `[model.*]` config) win. Then fill missing `provider` labels.
pub fn merge_into_catalog(catalog: &mut IndexMap<String, ModelEntry>, ui: &UiConfig) {
    // Unit tests must not pick up a real `~/.grok/providers.json`.
    if cfg!(test) && std::env::var_os("GROK_TEST_PROVIDERS").is_none() {
        for entry in catalog.values_mut() {
            fill_missing_provider(&mut entry.info);
        }
        return;
    }
    merge_from_store(catalog, &credentials::load(), ProviderVisibility::from(ui));
}

pub fn merge_from_store(
    catalog: &mut IndexMap<String, ModelEntry>,
    store: &credentials::ProviderStore,
    visibility: ProviderVisibility,
) {
    // OpenRouter / ChatGPT catalog fetches use reqwest::blocking. Tokio only
    // permits block_in_place on a multi-thread runtime; the ACP worker uses a
    // current-thread runtime, so move the blocking work to a scoped OS thread
    // there instead.
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| merge_from_store_inner(catalog, store, visibility));
        }
        Ok(_) => std::thread::scope(|scope| {
            if let Err(payload) = scope
                .spawn(move || merge_from_store_inner(catalog, store, visibility))
                .join()
            {
                std::panic::resume_unwind(payload);
            }
        }),
        Err(_) => merge_from_store_inner(catalog, store, visibility),
    }
}

fn merge_from_store_inner(
    catalog: &mut IndexMap<String, ModelEntry>,
    store: &credentials::ProviderStore,
    visibility: ProviderVisibility,
) {
    if visibility.openrouter {
        for (id, entry) in openrouter::catalog_entries(store) {
            catalog.entry(id).or_insert(entry);
        }
    }
    if visibility.chatgpt {
        for (id, entry) in openai_subscribed::catalog_entries(store) {
            catalog.entry(id).or_insert(entry);
        }
    }
    for entry in catalog.values_mut() {
        fill_missing_provider(&mut entry.info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::credentials::{ApiKeyCreds, ChatGptCreds, ProviderStore};

    #[test]
    fn infer_empty_url_is_xai() {
        assert_eq!(infer_provider("", None), "xAI");
    }

    #[test]
    fn infer_xai_hosts() {
        assert_eq!(infer_provider("https://api.x.ai/v1", None), "xAI");
        assert_eq!(
            infer_provider("https://cli-chat-proxy.grok.com/v1", None),
            "xAI"
        );
    }

    #[test]
    fn infer_named_providers() {
        assert_eq!(
            infer_provider("https://openrouter.ai/api/v1", None),
            "OpenRouter"
        );
        assert_eq!(
            infer_provider("https://chatgpt.com/backend-api/codex", None),
            "ChatGPT Subscription"
        );
        assert_eq!(infer_provider("https://api.openai.com/v1", None), "OpenAI");
        assert_eq!(
            infer_provider("https://api.anthropic.com", None),
            "Anthropic"
        );
    }

    #[test]
    fn infer_explicit_wins() {
        assert_eq!(
            infer_provider("https://openrouter.ai/api/v1", Some("Mine")),
            "Mine"
        );
    }

    #[test]
    fn merge_without_creds_only_fills_provider() {
        let mut catalog = IndexMap::new();
        let mut entry = ModelEntry::fallback(
            "grok-4.5",
            &crate::agent::config::EndpointsConfig::default(),
        );
        entry.info.base_url = "https://api.x.ai/v1".into();
        catalog.insert("grok-4.5".into(), entry);
        merge_from_store(
            &mut catalog,
            &ProviderStore::default(),
            ProviderVisibility::default(),
        );
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog["grok-4.5"].info.provider.as_deref(), Some("xAI"));
    }

    #[test]
    fn merge_injects_openrouter() {
        let mut store = ProviderStore::default();
        store.openrouter = Some(ApiKeyCreds {
            api_key: "sk-or-test".into(),
        });
        let mut catalog = IndexMap::new();
        merge_from_store(&mut catalog, &store, ProviderVisibility::default());
        assert!(catalog.contains_key("openrouter/auto"));
        assert!(!catalog.contains_key("chatgpt/gpt-5.4"));
    }

    #[test]
    fn merge_injects_chatgpt_fallbacks() {
        let mut store = ProviderStore::default();
        store.openai_subscribed = Some(ChatGptCreds {
            access_token: "tok".into(),
            refresh_token: "ref".into(),
            expires_at_ms: u64::MAX,
            account_id: None,
            email: None,
        });
        let mut catalog = IndexMap::new();
        merge_from_store(&mut catalog, &store, ProviderVisibility::default());
        assert!(catalog.contains_key("chatgpt/gpt-5.4"));
        assert_eq!(
            catalog["chatgpt/gpt-5.4"].info.provider.as_deref(),
            Some(openai_subscribed::PROVIDER_LABEL)
        );
    }

    #[test]
    fn existing_catalog_key_wins() {
        let mut store = ProviderStore::default();
        store.openrouter = Some(ApiKeyCreds {
            api_key: "sk-or-test".into(),
        });
        let mut catalog = IndexMap::new();
        let mut entry = ModelEntry::fallback(
            "openrouter/auto",
            &crate::agent::config::EndpointsConfig::default(),
        );
        entry.info.name = Some("Mine".into());
        catalog.insert("openrouter/auto".into(), entry);
        merge_from_store(&mut catalog, &store, ProviderVisibility::default());
        assert_eq!(
            catalog["openrouter/auto"].info.name.as_deref(),
            Some("Mine")
        );
    }

    #[test]
    fn disabled_connected_providers_are_not_merged() {
        let mut store = ProviderStore::default();
        store.openrouter = Some(ApiKeyCreds {
            api_key: "sk-or-test".into(),
        });
        store.openai_subscribed = Some(ChatGptCreds {
            access_token: "tok".into(),
            refresh_token: "ref".into(),
            expires_at_ms: u64::MAX,
            account_id: None,
            email: None,
        });

        let mut catalog = IndexMap::new();
        merge_from_store(
            &mut catalog,
            &store,
            ProviderVisibility {
                openrouter: false,
                chatgpt: false,
            },
        );

        assert!(!catalog.keys().any(|id| id.starts_with("openrouter/")));
        assert!(!catalog.keys().any(|id| id.starts_with("chatgpt/")));
    }
}
