use indexmap::IndexMap;
use xai_grok_sampler::ApiBackend;
use xai_grok_shell::providers::credentials::{ApiKeyCreds, ChatGptCreds, ProviderStore};
use xai_grok_shell::providers::openrouter;
use xai_grok_shell::providers::{
    ProviderId, ProviderVisibility, endpoint_uses_provider_credentials, merge_from_store,
};

#[test]
fn provider_catalog_merge_is_safe_on_current_thread_runtime() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");

    runtime.block_on(async {
        let mut catalog = IndexMap::new();
        merge_from_store(
            &mut catalog,
            &ProviderStore::default(),
            ProviderVisibility {
                openrouter: false,
                chatgpt: false,
            },
        );
    });
}

#[test]
fn disabled_authenticated_providers_do_not_contribute_models() {
    let store = ProviderStore {
        openrouter: Some(ApiKeyCreds {
            api_key: "sk-or-test".into(),
        }),
        openai_subscribed: Some(ChatGptCreds {
            access_token: "access-test".into(),
            refresh_token: "refresh-test".into(),
            expires_at_ms: u64::MAX,
            account_id: None,
            email: None,
        }),
    };
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

#[test]
fn openrouter_uses_the_unified_responses_endpoint() {
    assert_eq!(openrouter::API_URL, "https://openrouter.ai/api/v1");
    assert_eq!(openrouter::API_BACKEND, ApiBackend::Responses);
    assert!(ProviderId::parse("opencode").is_none());
}

#[test]
fn connected_provider_endpoints_do_not_inherit_ambient_xai_auth() {
    assert!(endpoint_uses_provider_credentials(
        "https://chatgpt.com/backend-api/codex"
    ));
    assert!(endpoint_uses_provider_credentials(
        "https://openrouter.ai/api/v1"
    ));
    assert!(!endpoint_uses_provider_credentials("https://api.x.ai/v1"));
    assert!(!endpoint_uses_provider_credentials(
        "https://chatgpt.com.evil.example/backend-api/codex"
    ));
}
