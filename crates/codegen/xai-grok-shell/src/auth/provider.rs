//! Multi-provider selection (Grok / OpenAI ChatGPT OAuth).
//!
//! The active provider decides which credentials drive inference. Credentials
//! for each provider can coexist in `auth.json` under distinct scope keys;
//! `[provider] active` in `config.toml` picks which one is current.

use serde::{Deserialize, Serialize};

use super::config::{XAI_OAUTH2_ISSUER, is_xai_oauth2_issuer};
use super::model::{AuthStore, GrokAuth, lookup_auth};
use super::storage::read_auth_json;

/// Wire / config identifiers for first-class LLM auth providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    /// xAI Grok (OAuth2 / API key / enterprise OIDC).
    #[default]
    Grok,
    /// OpenAI ChatGPT OAuth (Codex backend), same client as Codex CLI / OpenCode.
    Openai,
}

impl ProviderId {
    pub const ALL: &[ProviderId] = &[ProviderId::Grok, ProviderId::Openai];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grok => "grok",
            Self::Openai => "openai",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Grok => "Grok (xAI)",
            Self::Openai => "OpenAI (ChatGPT OAuth)",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "grok" | "xai" | "x.ai" => Some(Self::Grok),
            "openai" | "chatgpt" | "codex" => Some(Self::Openai),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[provider]` section in `~/.grok/config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProviderConfig {
    /// Which provider drives inference. Defaults to Grok.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<ProviderId>,
}

impl ProviderConfig {
    pub fn active_or_default(&self) -> ProviderId {
        self.active.unwrap_or_default()
    }
}

/// Codex / OpenCode ChatGPT OAuth client (public CLI client id).
pub const OPENAI_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const OPENAI_OAUTH_ISSUER: &str = "https://auth.openai.com";
/// ChatGPT Codex Responses API base (OpenCode / Codex CLI WHAM endpoint).
pub const OPENAI_CODEX_API_BASE: &str = "https://chatgpt.com/backend-api/codex";
/// Loopback port reserved by Codex CLI's Hydra redirect allow-list.
pub const OPENAI_OAUTH_CALLBACK_PORT: u16 = 1455;

/// auth.json scope for ChatGPT OAuth credentials.
pub fn openai_auth_scope() -> String {
    format!(
        "{}::{}",
        OPENAI_OAUTH_ISSUER.trim_end_matches('/'),
        OPENAI_OAUTH_CLIENT_ID
    )
}

/// auth.json scope for the default xAI OAuth2 client (when no enterprise OIDC).
pub fn grok_default_oauth_scope() -> String {
    // Keep in sync with `GrokComConfig::default()` client id.
    format!(
        "{}::{}",
        XAI_OAUTH2_ISSUER.trim_end_matches('/'),
        obfstr::obfstr!("b1a00492-073a-47ea-816f-4c329264a828")
    )
}

/// Whether this credential belongs to the OpenAI ChatGPT OAuth provider.
pub fn is_openai_auth(auth: &GrokAuth) -> bool {
    auth.oidc_issuer
        .as_deref()
        .is_some_and(|iss| iss.trim_end_matches('/') == OPENAI_OAUTH_ISSUER)
        || auth.oidc_client_id.as_deref() == Some(OPENAI_OAUTH_CLIENT_ID)
}

/// Whether this credential belongs to the Grok / xAI provider family.
pub fn is_grok_provider_auth(auth: &GrokAuth) -> bool {
    if is_openai_auth(auth) {
        return false;
    }
    match auth.auth_mode {
        super::AuthMode::Oidc => auth
            .oidc_issuer
            .as_deref()
            .map(is_xai_oauth2_issuer)
            .unwrap_or(true),
        super::AuthMode::ApiKey | super::AuthMode::External | super::AuthMode::WebLogin => true,
    }
}

/// Read active provider from effective config (env override wins).
pub fn active_provider() -> ProviderId {
    if let Ok(v) = std::env::var("GROK_ACTIVE_PROVIDER")
        && let Some(id) = ProviderId::parse(&v)
    {
        return id;
    }
    load_provider_config().active_or_default()
}

/// Load `[provider]` from effective config.toml layers.
pub fn load_provider_config() -> ProviderConfig {
    let Ok(root) = crate::config::load_effective_config() else {
        return ProviderConfig::default();
    };
    root.get("provider")
        .and_then(|v| v.clone().try_into().ok())
        .unwrap_or_default()
}

/// Persist `[provider].active` to the user config.toml.
pub async fn set_active_provider(id: ProviderId) -> anyhow::Result<()> {
    crate::util::config::update_config(|cfg| {
        cfg.provider.active = Some(id);
    })
    .await
}

/// Snapshot of login state for one provider.
#[derive(Debug, Clone)]
pub struct ProviderStatus {
    pub id: ProviderId,
    pub active: bool,
    pub logged_in: bool,
    pub email: Option<String>,
    pub account_label: Option<String>,
}

/// Status for every known provider, using the given auth store (or disk).
pub fn provider_statuses(store: Option<&AuthStore>) -> Vec<ProviderStatus> {
    let owned;
    let map = match store {
        Some(m) => m,
        None => {
            let path = crate::util::grok_home::grok_home().join("auth.json");
            owned = read_auth_json(&path).unwrap_or_default();
            &owned
        }
    };
    let active = active_provider();
    ProviderId::ALL
        .iter()
        .copied()
        .map(|id| {
            let auth = lookup_provider_auth(map, id);
            ProviderStatus {
                id,
                active: id == active,
                logged_in: auth.is_some(),
                email: auth.as_ref().and_then(|a| a.email.clone()),
                account_label: auth.as_ref().and_then(|a| {
                    a.chatgpt_account_id
                        .clone()
                        .or_else(|| a.organization_id.clone())
                        .or_else(|| a.team_name.clone())
                }),
            }
        })
        .collect()
}

/// Look up credentials for a provider from an auth store.
pub fn lookup_provider_auth(map: &AuthStore, id: ProviderId) -> Option<GrokAuth> {
    match id {
        ProviderId::Openai => lookup_auth(map, &openai_auth_scope()),
        ProviderId::Grok => {
            // Prefer the default xAI OAuth scope, then any non-OpenAI entry,
            // including the API-key scope.
            if let Some(a) = lookup_auth(map, &grok_default_oauth_scope()) {
                return Some(a);
            }
            if let Some(a) = lookup_auth(map, super::model::API_KEY_SCOPE) {
                return Some(a);
            }
            map.values()
                .find(|a| is_grok_provider_auth(a) && a.auth_mode != super::AuthMode::WebLogin)
                .cloned()
        }
    }
}

/// Human-readable multi-line status for `/provider` with no args.
pub fn format_provider_status(statuses: &[ProviderStatus]) -> String {
    let mut lines = vec!["Providers:".to_string()];
    for s in statuses {
        let marker = if s.active { "*" } else { " " };
        let login = if s.logged_in {
            match (&s.email, &s.account_label) {
                (Some(email), Some(acct)) => format!("logged in as {email} ({acct})"),
                (Some(email), None) => format!("logged in as {email}"),
                (None, Some(acct)) => format!("logged in ({acct})"),
                (None, None) => "logged in".to_string(),
            }
        } else {
            "not logged in".to_string()
        };
        lines.push(format!(
            "  {marker} {} — {login}",
            s.id.display_name()
        ));
    }
    lines.push(String::new());
    lines.push("Usage: /provider <grok|openai> | /provider login <grok|openai>".into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMode;
    use chrono::Utc;

    fn sample_auth(issuer: &str, client_id: &str) -> GrokAuth {
        GrokAuth {
            key: "tok".into(),
            auth_mode: AuthMode::Oidc,
            create_time: Utc::now(),
            user_id: "u".into(),
            oidc_issuer: Some(issuer.into()),
            oidc_client_id: Some(client_id.into()),
            ..GrokAuth::test_default()
        }
    }

    #[test]
    fn parse_aliases() {
        assert_eq!(ProviderId::parse("GROK"), Some(ProviderId::Grok));
        assert_eq!(ProviderId::parse("chatgpt"), Some(ProviderId::Openai));
        assert_eq!(ProviderId::parse("codex"), Some(ProviderId::Openai));
        assert_eq!(ProviderId::parse("nope"), None);
    }

    #[test]
    fn openai_scope_matches_codex_client() {
        assert_eq!(
            openai_auth_scope(),
            "https://auth.openai.com::app_EMoamEEZ73f0CkXaXp7hrann"
        );
    }

    #[test]
    fn is_openai_auth_detects_issuer() {
        let a = sample_auth(OPENAI_OAUTH_ISSUER, OPENAI_OAUTH_CLIENT_ID);
        assert!(is_openai_auth(&a));
        assert!(!is_grok_provider_auth(&a));
        let g = sample_auth(XAI_OAUTH2_ISSUER, "b1a00492-073a-47ea-816f-4c329264a828");
        assert!(!is_openai_auth(&g));
        assert!(is_grok_provider_auth(&g));
    }

    #[test]
    fn lookup_separates_providers() {
        let mut map = AuthStore::new();
        map.insert(
            openai_auth_scope(),
            sample_auth(OPENAI_OAUTH_ISSUER, OPENAI_OAUTH_CLIENT_ID),
        );
        map.insert(
            grok_default_oauth_scope(),
            sample_auth(XAI_OAUTH2_ISSUER, "b1a00492-073a-47ea-816f-4c329264a828"),
        );
        assert!(lookup_provider_auth(&map, ProviderId::Openai).is_some());
        assert!(lookup_provider_auth(&map, ProviderId::Grok).is_some());
    }
}
