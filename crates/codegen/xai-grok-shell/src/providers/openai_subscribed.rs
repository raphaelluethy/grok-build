//! ChatGPT Plus/Pro subscription adapter (Codex Responses API).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use indexmap::IndexMap;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use xai_grok_sampler::ApiBackend;
use xai_grok_sampling_types::{ReasoningEffort, ReasoningEffortOption};

use super::credentials::{ChatGptCreds, ProviderStore};
use super::entry::{ProviderModelSpec, build_entry};
use crate::agent::config::ModelEntry;
use crate::auth::oidc::callback_page;

pub const PROVIDER_LABEL: &str = "ChatGPT Subscription";
pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_CALLBACK_HOST: &str = "127.0.0.1";
const CODEX_CALLBACK_PORT: u16 = 1455;
const CODEX_CALLBACK_FALLBACK_PORT: u16 = 1457;
const CODEX_CALLBACK_PATH: &str = "/auth/callback";
const AUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(600);
const UNGATED_CLIENT_VERSION: &str = "0.0.0";
/// Active prompt window advertised by Codex when the live catalog is unavailable.
/// `max_context_window` is only a capability ceiling (often 872k or 1M) and
/// must not be used for compaction or ACP context reporting.
const DEFAULT_CODEX_CONTEXT_WINDOW: u64 = 272_000;
const DEFAULT_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::High;

struct FallbackModel {
    id: &'static str,
    name: &'static str,
    context_window: u64,
    supports_fast_mode: bool,
}

const FALLBACK_MODELS: &[FallbackModel] = &[
    FallbackModel {
        id: "gpt-5.6-sol",
        name: "GPT-5.6 Sol",
        context_window: DEFAULT_CODEX_CONTEXT_WINDOW,
        supports_fast_mode: true,
    },
    FallbackModel {
        id: "gpt-5.6-terra",
        name: "GPT-5.6 Terra",
        context_window: DEFAULT_CODEX_CONTEXT_WINDOW,
        supports_fast_mode: true,
    },
    FallbackModel {
        id: "gpt-5.6-luna",
        name: "GPT-5.6 Luna",
        context_window: DEFAULT_CODEX_CONTEXT_WINDOW,
        supports_fast_mode: true,
    },
    FallbackModel {
        id: "gpt-5.5",
        name: "GPT-5.5",
        context_window: DEFAULT_CODEX_CONTEXT_WINDOW,
        supports_fast_mode: true,
    },
    FallbackModel {
        id: "gpt-5.4",
        name: "GPT-5.4",
        context_window: DEFAULT_CODEX_CONTEXT_WINDOW,
        supports_fast_mode: true,
    },
    FallbackModel {
        id: "gpt-5.4-mini",
        name: "GPT-5.4 Mini",
        context_window: DEFAULT_CODEX_CONTEXT_WINDOW,
        supports_fast_mode: false,
    },
];

pub fn catalog_entries(store: &ProviderStore) -> IndexMap<String, ModelEntry> {
    let Some(creds) = store.openai_subscribed.as_ref() else {
        return IndexMap::new();
    };
    let creds = match ensure_fresh(creds) {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(error = %err, "chatgpt: token refresh failed; using stored access token");
            creds.clone()
        }
    };
    let mut extra_headers = IndexMap::new();
    extra_headers.insert("originator".into(), "grog".into());
    extra_headers.insert("openai-beta".into(), "responses=experimental".into());
    if let Some(id) = creds.account_id.as_deref().filter(|s| !s.is_empty()) {
        extra_headers.insert("chatgpt-account-id".into(), id.to_owned());
    }

    let discovered = if cfg!(test) {
        Vec::new()
    } else {
        fetch_models(&creds).unwrap_or_else(|err| {
            tracing::warn!(error = %err, "chatgpt: model catalog fetch failed; using fallbacks");
            Vec::new()
        })
    };

    let mut out = IndexMap::new();
    if discovered.is_empty() {
        for model in FALLBACK_MODELS {
            insert_model(
                &mut out,
                model.id,
                model.name,
                model.context_window,
                true,
                fallback_effort_options(),
                model.supports_fast_mode,
                extra_headers.clone(),
                creds.access_token.clone(),
            );
        }
    } else {
        for model in discovered {
            let reasoning_efforts = catalog_effort_options(&model);
            let supports_fast_mode = model.supports_fast_mode();
            insert_model(
                &mut out,
                &model.slug,
                &model.display_name,
                model.effective_context_window(),
                !reasoning_efforts.is_empty(),
                reasoning_efforts,
                supports_fast_mode,
                extra_headers.clone(),
                creds.access_token.clone(),
            );
        }
    }
    out
}

fn insert_model(
    out: &mut IndexMap<String, ModelEntry>,
    slug: &str,
    name: &str,
    context_window: u64,
    supports_reasoning: bool,
    reasoning_efforts: Vec<ReasoningEffortOption>,
    supports_fast_mode: bool,
    extra_headers: IndexMap<String, String>,
    api_key: String,
) {
    let catalog_id = format!("chatgpt/{slug}");
    let (id, mut entry) = build_entry(ProviderModelSpec {
        catalog_id: &catalog_id,
        routing_slug: slug,
        display_name: name,
        provider: PROVIDER_LABEL,
        base_url: CODEX_BASE_URL,
        api_backend: ApiBackend::Responses,
        api_key,
        env_key: None,
        context_window,
        extra_headers,
        supports_reasoning,
        max_completion_tokens: None,
    });
    entry.info.reasoning_effort = Some(DEFAULT_REASONING_EFFORT);
    entry.info.reasoning_efforts = reasoning_efforts;
    entry.info.supports_fast_mode = supports_fast_mode;
    out.insert(id, entry);
}

fn fallback_effort_options() -> Vec<ReasoningEffortOption> {
    vec![
        effort_opt(
            "low",
            ReasoningEffort::Low,
            DEFAULT_REASONING_EFFORT == ReasoningEffort::Low,
        ),
        effort_opt(
            "medium",
            ReasoningEffort::Medium,
            DEFAULT_REASONING_EFFORT == ReasoningEffort::Medium,
        ),
        effort_opt(
            "high",
            ReasoningEffort::High,
            DEFAULT_REASONING_EFFORT == ReasoningEffort::High,
        ),
        effort_opt(
            "xhigh",
            ReasoningEffort::Xhigh,
            DEFAULT_REASONING_EFFORT == ReasoningEffort::Xhigh,
        ),
    ]
}

fn catalog_effort_options(model: &CatalogModel) -> Vec<ReasoningEffortOption> {
    let mut options = Vec::new();
    for preset in &model.supported_reasoning_levels {
        let Some(value) = parse_effort(Some(&preset.effort)) else {
            continue;
        };
        if options
            .iter()
            .any(|option: &ReasoningEffortOption| option.value == value)
        {
            continue;
        }
        options.push(effort_opt(
            preset.effort.trim(),
            value,
            value == DEFAULT_REASONING_EFFORT,
        ));
    }
    if !options
        .iter()
        .any(|option| option.value == DEFAULT_REASONING_EFFORT)
    {
        options.push(effort_opt(
            DEFAULT_REASONING_EFFORT.as_str(),
            DEFAULT_REASONING_EFFORT,
            true,
        ));
    }
    options
}

fn effort_opt(id: &str, value: ReasoningEffort, default: bool) -> ReasoningEffortOption {
    ReasoningEffortOption {
        id: id.to_owned(),
        value,
        label: id.to_owned(),
        description: None,
        default,
    }
}

fn parse_effort(raw: Option<&str>) -> Option<ReasoningEffort> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "none" => Some(ReasoningEffort::None),
        "minimal" => Some(ReasoningEffort::Minimal),
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" | "max" => Some(ReasoningEffort::Xhigh),
        _ => None,
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    models: Vec<CatalogModel>,
}

#[derive(Deserialize)]
struct CatalogModel {
    slug: String,
    display_name: String,
    #[serde(default)]
    supported_reasoning_levels: Vec<ReasoningEffortPreset>,
    context_window: Option<u64>,
    #[serde(default)]
    visibility: ModelVisibility,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    additional_speed_tiers: Vec<String>,
    #[serde(default)]
    service_tiers: Vec<ModelServiceTier>,
}

impl CatalogModel {
    fn effective_context_window(&self) -> u64 {
        self.context_window
            .unwrap_or(DEFAULT_CODEX_CONTEXT_WINDOW)
            .max(1)
    }

    fn supports_fast_mode(&self) -> bool {
        self.additional_speed_tiers
            .iter()
            .any(|tier| tier.eq_ignore_ascii_case("fast"))
            || self
                .service_tiers
                .iter()
                .any(|tier| tier.id.eq_ignore_ascii_case("priority"))
    }
}

#[derive(Deserialize)]
struct ReasoningEffortPreset {
    effort: String,
}

#[derive(Deserialize)]
struct ModelServiceTier {
    id: String,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ModelVisibility {
    #[default]
    List,
    Hide,
    None,
}

fn fetch_models(creds: &ChatGptCreds) -> Result<Vec<CatalogModel>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    let url = format!("{CODEX_BASE_URL}/models?client_version={UNGATED_CLIENT_VERSION}");
    let mut req = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("originator", "grog")
        .header("openai-beta", "responses=experimental");
    if let Some(id) = creds.account_id.as_deref().filter(|s| !s.is_empty()) {
        req = req.header("chatgpt-account-id", id);
    }
    let resp = req.send()?;
    if !resp.status().is_success() {
        bail!("HTTP {} listing ChatGPT models", resp.status());
    }
    let mut body: ModelsResponse = resp.json()?;
    body.models
        .retain(|model| model.visibility == ModelVisibility::List);
    body.models.sort_by_key(|model| model.priority);
    Ok(body.models)
}

fn ensure_fresh(creds: &ChatGptCreds) -> Result<ChatGptCreds> {
    if !creds.is_expired() {
        return Ok(creds.clone());
    }
    let refreshed = refresh_token_blocking(creds)?;
    let _ = super::credentials::save_chatgpt(refreshed.clone());
    Ok(refreshed)
}

fn refresh_token_blocking(creds: &ChatGptCreds) -> Result<ChatGptCreds> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    // Codex sends refresh requests as JSON. The refresh response is a patch:
    // auth.openai.com may omit unchanged token fields, so preserve the stored
    // values rather than requiring every field to be returned.
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": CLIENT_ID,
        "refresh_token": creds.refresh_token,
    });
    let resp = client.post(OPENAI_TOKEN_URL).json(&body).send()?;
    if !resp.status().is_success() {
        bail!("ChatGPT token refresh failed (HTTP {})", resp.status());
    }
    let tokens: TokenResponse = resp.json()?;
    creds_from_tokens(tokens, Some(creds))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

fn creds_from_tokens(
    tokens: TokenResponse,
    previous: Option<&ChatGptCreds>,
) -> Result<ChatGptCreds> {
    let access_token = tokens
        .access_token
        .or_else(|| previous.map(|creds| creds.access_token.clone()))
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow!("ChatGPT token response omitted access_token"))?;
    let refresh_token = tokens
        .refresh_token
        .or_else(|| previous.map(|creds| creds.refresh_token.clone()))
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow!("ChatGPT token response omitted refresh_token"))?;
    let access_claims = extract_jwt_claims(&access_token);
    let identity_claims = tokens
        .id_token
        .as_deref()
        .map(extract_jwt_claims)
        .unwrap_or_default();
    let expires_at_ms = tokens
        .expires_in
        .map(|seconds| now_ms().saturating_add(seconds.saturating_mul(1000)))
        .or(access_claims.expires_at_ms)
        .or(identity_claims.expires_at_ms)
        // The JWT normally carries `exp`; keep a conservative fallback for
        // non-JWT test/staging tokens so they are refreshed again promptly.
        .unwrap_or_else(|| now_ms().saturating_add(60 * 60 * 1000));
    Ok(ChatGptCreds {
        access_token,
        refresh_token,
        expires_at_ms,
        account_id: identity_claims
            .account_id
            .or(access_claims.account_id)
            .or_else(|| previous.and_then(|creds| creds.account_id.clone())),
        email: identity_claims
            .email
            .or(access_claims.email)
            .or(tokens.email)
            .or_else(|| previous.and_then(|creds| creds.email.clone())),
    })
}

#[derive(Default)]
struct JwtClaims {
    account_id: Option<String>,
    email: Option<String>,
    expires_at_ms: Option<u64>,
}

fn extract_jwt_claims(jwt: &str) -> JwtClaims {
    let Some(payload_b64) = jwt.split('.').nth(1) else {
        return JwtClaims {
            account_id: None,
            email: None,
            expires_at_ms: None,
        };
    };
    let Ok(payload) = URL_SAFE_NO_PAD.decode(payload_b64) else {
        return JwtClaims {
            account_id: None,
            email: None,
            expires_at_ms: None,
        };
    };
    let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return JwtClaims {
            account_id: None,
            email: None,
            expires_at_ms: None,
        };
    };
    let account_id = claims
        .get("chatgpt_account_id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|v| v.get("chatgpt_account_id"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|org| org.get("id"))
                .and_then(|v| v.as_str())
        })
        .map(str::to_owned);
    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let expires_at_ms = claims
        .get("exp")
        .and_then(|value| value.as_u64())
        .map(|seconds| seconds.saturating_mul(1000));
    JwtClaims {
        account_id,
        email,
        expires_at_ms,
    }
}

/// Browser PKCE sign-in for a ChatGPT Plus/Pro subscription.
pub async fn sign_in() -> Result<ChatGptCreds> {
    let (redirect_uri, listener) = bind_callback_listener().await?;
    let mut verifier_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut verifier_bytes);
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize().as_slice());
    let mut state_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut state_bytes);
    let oauth_state: String = state_bytes.iter().map(|b| format!("{b:02x}")).collect();

    let mut auth_url = url::Url::parse(OPENAI_AUTHORIZE_URL).expect("valid authorize URL");
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair(
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        )
        .append_pair("response_type", "code")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("state", &oauth_state)
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "grog");

    let (tx, rx) = tokio::sync::oneshot::channel::<Result<Callback>>();
    let expected_state = oauth_state.clone();
    let (server, shutdown) = spawn_callback_server(listener, expected_state, tx);
    let url_str = auth_url.to_string();
    tracing::info!(url = %url_str, "chatgpt: opening browser for sign-in");
    if let Err(e) = webbrowser::open(&url_str) {
        tracing::warn!(error = %e, "chatgpt: failed to open browser; visit {url_str}");
    }

    let callback = tokio::time::timeout(AUTH_CALLBACK_TIMEOUT, rx).await;
    shutdown.cancel();
    let _ = server.await;
    let callback = callback
        .map_err(|_| anyhow!("ChatGPT sign-in timed out after 10 minutes"))?
        .map_err(|_| anyhow!("ChatGPT sign-in was cancelled"))??;

    let tokens = exchange_code(&callback.code, &verifier, &redirect_uri).await?;
    let creds = creds_from_tokens(tokens, None)?;
    super::credentials::save_chatgpt(creds.clone())?;
    Ok(creds)
}

struct Callback {
    code: String,
}

async fn bind_callback_listener() -> Result<(String, TcpListener)> {
    for port in [CODEX_CALLBACK_PORT, CODEX_CALLBACK_FALLBACK_PORT] {
        if let Ok(listener) = TcpListener::bind((CODEX_CALLBACK_HOST, port)).await {
            let uri = format!("http://localhost:{port}{CODEX_CALLBACK_PATH}");
            return Ok((uri, listener));
        }
    }
    bail!(
        "Could not bind ChatGPT OAuth callback on ports {CODEX_CALLBACK_PORT} or {CODEX_CALLBACK_FALLBACK_PORT}"
    );
}

fn spawn_callback_server(
    listener: TcpListener,
    expected_state: String,
    tx: tokio::sync::oneshot::Sender<Result<Callback>>,
) -> (tokio::task::JoinHandle<()>, CancellationToken) {
    let shutdown = CancellationToken::new();
    let shutdown_for_server = shutdown.clone();
    let task = tokio::spawn(async move {
        let tx = Arc::new(std::sync::Mutex::new(Some(tx)));
        let state = CallbackState {
            expected_state,
            tx: tx.clone(),
            shutdown: shutdown_for_server.clone(),
        };
        let app = Router::new()
            .route(CODEX_CALLBACK_PATH, get(callback_handler))
            .with_state(state);
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_for_server.cancelled_owned())
            .await;
    });
    (task, shutdown)
}

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    tx: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Result<Callback>>>>>,
    shutdown: CancellationToken,
}

fn finish_callback(state: &CallbackState, result: Result<Callback>) {
    if let Ok(mut slot) = state.tx.lock()
        && let Some(tx) = slot.take()
    {
        let _ = tx.send(result);
    }
    state.shutdown.cancel();
}

async fn callback_handler(
    State(state): State<CallbackState>,
    Query(params): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    if let Some(error) = params.get("error") {
        let desc = params.get("error_description").cloned().unwrap_or_default();
        let msg = if desc.is_empty() {
            error.clone()
        } else {
            format!("{error}: {desc}")
        };
        finish_callback(&state, Err(anyhow!(msg.clone())));
        return (
            StatusCode::BAD_REQUEST,
            Html(callback_page("ChatGPT sign-in failed", &msg, false)),
        );
    }
    let Some(code) = params.get("code").cloned() else {
        finish_callback(&state, Err(anyhow!("Missing authorization code")));
        return (
            StatusCode::BAD_REQUEST,
            Html(callback_page(
                "ChatGPT sign-in failed",
                "Missing authorization code",
                false,
            )),
        );
    };
    let got_state = params.get("state").cloned().unwrap_or_default();
    if got_state != state.expected_state {
        finish_callback(&state, Err(anyhow!("OAuth state mismatch")));
        return (
            StatusCode::BAD_REQUEST,
            Html(callback_page(
                "ChatGPT sign-in failed",
                "OAuth state mismatch",
                false,
            )),
        );
    }
    finish_callback(&state, Ok(Callback { code }));
    (
        StatusCode::OK,
        Html(callback_page(
            "Signed in to ChatGPT",
            "You can close this tab and return to GROG.",
            true,
        )),
    )
}

async fn exchange_code(code: &str, verifier: &str, redirect_uri: &str) -> Result<TokenResponse> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let body = [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    let resp = client.post(OPENAI_TOKEN_URL).form(&body).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("Token exchange failed (HTTP {status}): {text}");
    }
    resp.json().await.context("parse ChatGPT token response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::credentials::ProviderStore;

    #[test]
    fn no_creds_yields_empty() {
        assert!(catalog_entries(&ProviderStore::default()).is_empty());
    }

    #[test]
    fn stored_creds_inject_fallback_models() {
        let mut store = ProviderStore::default();
        store.openai_subscribed = Some(ChatGptCreds {
            access_token: "tok".into(),
            refresh_token: "ref".into(),
            expires_at_ms: now_ms() + 60_000,
            account_id: Some("acct".into()),
            email: Some("user@example.com".into()),
        });
        let catalog = catalog_entries(&store);
        assert!(catalog.contains_key("chatgpt/gpt-5.4"));
        let model = &catalog["chatgpt/gpt-5.4"];
        assert_eq!(model.info.provider.as_deref(), Some(PROVIDER_LABEL));
        assert_eq!(model.info.api_backend, ApiBackend::Responses);
        assert_eq!(model.info.base_url, CODEX_BASE_URL);
        assert_eq!(
            model
                .info
                .extra_headers
                .get("originator")
                .map(String::as_str),
            Some("grog")
        );
        assert_eq!(
            model
                .info
                .extra_headers
                .get("chatgpt-account-id")
                .map(String::as_str),
            Some("acct")
        );
        assert!(model.info.supports_reasoning_effort);
        assert!(model.info.supports_fast_mode);
        assert!(!catalog["chatgpt/gpt-5.4-mini"].info.supports_fast_mode);
        for (model_id, model) in &catalog {
            assert_eq!(
                model.info.reasoning_effort,
                Some(ReasoningEffort::High),
                "{model_id} must default to high reasoning effort",
            );
            let defaults: Vec<_> = model
                .info
                .reasoning_efforts
                .iter()
                .filter(|option| option.default)
                .map(|option| option.value)
                .collect();
            assert_eq!(
                defaults,
                vec![ReasoningEffort::High],
                "{model_id} must expose high as its only menu default",
            );
        }
    }

    #[test]
    fn live_catalog_efforts_override_upstream_default_with_high() {
        let model: CatalogModel = serde_json::from_value(serde_json::json!({
            "slug": "gpt-live",
            "display_name": "GPT Live",
            "default_reasoning_level": "low",
            "supported_reasoning_levels": [
                { "effort": "low" },
                { "effort": "medium" },
                { "effort": "high" }
            ]
        }))
        .unwrap();

        let options = catalog_effort_options(&model);
        assert_eq!(
            options
                .iter()
                .find(|option| option.default)
                .map(|option| option.value),
            Some(ReasoningEffort::High),
        );
        assert_eq!(options.iter().filter(|option| option.default).count(), 1,);
    }

    #[test]
    fn live_catalog_efforts_add_high_when_upstream_omits_it() {
        let model: CatalogModel = serde_json::from_value(serde_json::json!({
            "slug": "gpt-live",
            "display_name": "GPT Live",
            "supported_reasoning_levels": [{ "effort": "low" }]
        }))
        .unwrap();

        let options = catalog_effort_options(&model);
        assert!(
            options
                .iter()
                .any(|option| { option.value == ReasoningEffort::High && option.default })
        );
    }

    #[test]
    fn fallback_catalog_uses_active_context_window() {
        let mut store = ProviderStore::default();
        store.openai_subscribed = Some(ChatGptCreds {
            access_token: "tok".into(),
            refresh_token: "ref".into(),
            expires_at_ms: now_ms() + 60_000,
            account_id: None,
            email: None,
        });

        let catalog = catalog_entries(&store);
        let acp_catalog = crate::agent::config::to_acp_model_info(&catalog);

        for model in FALLBACK_MODELS {
            let catalog_id = format!("chatgpt/{}", model.id);
            assert_eq!(
                catalog[&catalog_id].info.context_window.get(),
                DEFAULT_CODEX_CONTEXT_WINDOW,
                "{} must use the active Codex prompt window",
                model.id,
            );

            let acp_id = agent_client_protocol::ModelId::new(catalog_id);
            let meta = acp_catalog[&acp_id]
                .meta
                .as_ref()
                .expect("provider context metadata must be present");
            assert_eq!(
                meta["totalContextTokens"], DEFAULT_CODEX_CONTEXT_WINDOW,
                "{} must expose the active window to ACP clients",
                model.id,
            );
        }
    }

    #[test]
    fn max_context_window_is_not_the_active_window() {
        let model: CatalogModel = serde_json::from_value(serde_json::json!({
            "slug": "gpt-5.6-sol",
            "display_name": "GPT-5.6 Sol",
            "context_window": null,
            "max_context_window": 872000
        }))
        .unwrap();

        assert_eq!(
            model.effective_context_window(),
            DEFAULT_CODEX_CONTEXT_WINDOW
        );
    }

    #[test]
    fn catalog_speed_metadata_enables_fast_mode() {
        let by_speed: CatalogModel = serde_json::from_value(serde_json::json!({
            "slug": "gpt-fast",
            "display_name": "GPT Fast",
            "additional_speed_tiers": ["fast"]
        }))
        .unwrap();
        assert!(by_speed.supports_fast_mode());

        let by_service_tier: CatalogModel = serde_json::from_value(serde_json::json!({
            "slug": "gpt-priority",
            "display_name": "GPT Priority",
            "service_tiers": [{ "id": "priority" }]
        }))
        .unwrap();
        assert!(by_service_tier.supports_fast_mode());
    }

    #[test]
    fn jwt_extracts_chatgpt_account_id() {
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "chatgpt_account_id": "acct-1",
                "email": "a@b.c",
                "exp": 1_900_000_000_u64
            })
            .to_string(),
        );
        let jwt = format!("h.{payload}.s");
        let claims = extract_jwt_claims(&jwt);
        assert_eq!(claims.account_id.as_deref(), Some("acct-1"));
        assert_eq!(claims.email.as_deref(), Some("a@b.c"));
        assert_eq!(claims.expires_at_ms, Some(1_900_000_000_000));
    }

    #[test]
    fn refresh_token_patch_preserves_omitted_stored_fields() {
        let previous = ChatGptCreds {
            access_token: "old-access".into(),
            refresh_token: "old-refresh".into(),
            expires_at_ms: 123,
            account_id: Some("acct-1".into()),
            email: Some("user@example.com".into()),
        };
        let refreshed = creds_from_tokens(
            TokenResponse {
                access_token: Some("new-access".into()),
                refresh_token: None,
                expires_in: Some(3600),
                id_token: None,
                email: None,
            },
            Some(&previous),
        )
        .unwrap();

        assert_eq!(refreshed.access_token, "new-access");
        assert_eq!(refreshed.refresh_token, "old-refresh");
        assert_eq!(refreshed.account_id.as_deref(), Some("acct-1"));
        assert_eq!(refreshed.email.as_deref(), Some("user@example.com"));
        assert!(refreshed.expires_at_ms > now_ms());
    }

    #[tokio::test]
    async fn callback_server_exits_after_successful_callback() {
        let listener = TcpListener::bind((CODEX_CALLBACK_HOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let (server, _shutdown) = spawn_callback_server(listener, "expected-state".to_string(), tx);
        let url = format!(
            "http://{CODEX_CALLBACK_HOST}:{port}{CODEX_CALLBACK_PATH}?code=auth-code&state=expected-state"
        );

        let client = reqwest::Client::new();
        let mut response = None;
        for _ in 0..100 {
            match client.get(&url).send().await {
                Ok(value) => {
                    response = Some(value);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
        assert_eq!(
            response.expect("callback server did not start").status(),
            StatusCode::OK
        );
        let callback = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("callback result timed out")
            .expect("callback sender dropped")
            .expect("callback returned an error");
        assert_eq!(callback.code, "auth-code");

        tokio::time::timeout(Duration::from_millis(250), server)
            .await
            .expect("callback server must stop after delivering the OAuth result")
            .expect("callback server task panicked");
    }
}
