//! ChatGPT OAuth PKCE loopback login (Codex CLI / OpenCode compatible).

use std::collections::HashMap;

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::get,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::token::{account_id_from_tokens, email_from_tokens};
use crate::auth::flow::{AuthChannels, AuthUrlInfo, AuthUrlMode};
use crate::auth::model::{AuthMode, GrokAuth};
use crate::auth::provider::{
    OPENAI_OAUTH_CALLBACK_PORT, OPENAI_OAUTH_CLIENT_ID, OPENAI_OAUTH_ISSUER, openai_auth_scope,
};
use crate::auth::storage;

pub(crate) const CALLBACK_PATH: &str = "/auth/callback";
const AUTH_CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
const ORIGINATOR: &str = "grok_build";

#[derive(Debug)]
struct Pkce {
    verifier: String,
    challenge: String,
}

fn generate_pkce() -> Pkce {
    let random_bytes: [u8; 64] = rand::random();
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

fn generate_state() -> String {
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn build_authorize_url(redirect_uri: &str, pkce: &Pkce, state: &str) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", OPENAI_OAUTH_CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", ORIGINATOR),
    ];
    let qs = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{OPENAI_OAUTH_ISSUER}/oauth/authorize?{qs}")
}

#[derive(Debug, serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

async fn exchange_code(
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> anyhow::Result<TokenResponse> {
    let token_url = format!("{OPENAI_OAUTH_ISSUER}/oauth/token");
    let resp = crate::http::shared_client()
        .post(&token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", OPENAI_OAUTH_CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("ChatGPT token exchange failed: HTTP {status} — {body}");
    }
    Ok(resp.json().await?)
}

type CallbackResult = Result<Callback, String>;

#[derive(Debug)]
struct Callback {
    code: String,
    state: String,
}

fn build_callback_router(tx: mpsc::Sender<CallbackResult>) -> Router {
    Router::new()
        .route(CALLBACK_PATH, get(handle_callback))
        .with_state(tx)
}

fn simple_callback_page(title: &str, message: &str, is_success: bool) -> String {
    let color = if is_success { "#22c55e" } else { "#ef4444" };
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"/><title>{title}</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;
min-height:100vh;background:#0a0a0a;color:#e5e5e5}}.card{{text-align:center}}h1{{color:{color}}}</style>
</head><body><div class="card"><h1>{title}</h1><p>{message}</p></div></body></html>"#
    )
}

async fn handle_callback(
    State(tx): State<mpsc::Sender<CallbackResult>>,
    Query(params): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let result = if let Some(code) = params.get("code") {
        Ok(Callback {
            code: code.clone(),
            state: params.get("state").cloned().unwrap_or_default(),
        })
    } else {
        let error = params.get("error").cloned().unwrap_or_default();
        let desc = params
            .get("error_description")
            .cloned()
            .unwrap_or_default();
        Err(if desc.is_empty() {
            error
        } else {
            format!("{error}: {desc}")
        })
    };
    let (title, message) = match &result {
        Ok(_) => (
            "Signed in",
            "You can close this window and return to Grok Build.",
        ),
        Err(_) => ("Access denied", "Close this window and try again."),
    };
    let page = simple_callback_page(title, message, result.is_ok());
    let _ = tx.try_send(result);
    (StatusCode::OK, Html(page))
}

fn auth_from_tokens(tokens: TokenResponse) -> GrokAuth {
    let now = Utc::now();
    let account_id = account_id_from_tokens(tokens.id_token.as_deref(), &tokens.access_token);
    let email = email_from_tokens(tokens.id_token.as_deref(), &tokens.access_token);
    let user_id = email
        .clone()
        .or_else(|| account_id.clone())
        .unwrap_or_else(|| "chatgpt-user".into());
    GrokAuth {
        key: tokens.access_token,
        auth_mode: AuthMode::Oidc,
        create_time: now,
        user_id,
        email,
        refresh_token: tokens.refresh_token,
        expires_at: tokens
            .expires_in
            .map(|s| now + Duration::seconds(s as i64)),
        oidc_issuer: Some(OPENAI_OAUTH_ISSUER.to_owned()),
        oidc_client_id: Some(OPENAI_OAUTH_CLIENT_ID.to_owned()),
        chatgpt_account_id: account_id,
        ..Default::default()
    }
}

fn persist_openai_auth(grok_home: &std::path::Path, auth: &GrokAuth) -> anyhow::Result<()> {
    let path = grok_home.join("auth.json");
    let mut map = storage::read_auth_json_or_empty(&path)?;
    map.insert(openai_auth_scope(), auth.clone());
    storage::write_auth_json(&path, &map)?;
    Ok(())
}

/// Interactive ChatGPT OAuth login (browser + loopback on port 1455).
pub async fn run_chatgpt_login(grok_home: &std::path::Path) -> anyhow::Result<GrokAuth> {
    run_chatgpt_login_with_channels(grok_home, None).await
}

/// Same as [`run_chatgpt_login`], optionally pushing the authorize URL to the TUI.
pub async fn run_chatgpt_login_with_channels(
    grok_home: &std::path::Path,
    channels: Option<AuthChannels>,
) -> anyhow::Result<GrokAuth> {
    let pkce = generate_pkce();
    let state = generate_state();

    let listener = match TcpListener::bind(("127.0.0.1", OPENAI_OAUTH_CALLBACK_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                error = %e,
                port = OPENAI_OAUTH_CALLBACK_PORT,
                "ChatGPT OAuth: port 1455 busy; binding ephemeral port"
            );
            TcpListener::bind(("127.0.0.1", 0)).await?
        }
    };
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}{CALLBACK_PATH}");
    let auth_url = build_authorize_url(&redirect_uri, &pkce, &state);

    let url_tx = channels.and_then(|ch| ch.url_tx);

    eprintln!();
    eprintln!("Signing in with OpenAI (ChatGPT)...");
    eprintln!();
    if let Err(e) = webbrowser::open(&auth_url) {
        tracing::debug!(error = %e, "ChatGPT OAuth: failed to open browser");
    }
    eprintln!("Open this URL to sign in:");
    eprintln!("  {auth_url}");
    eprintln!();

    if let Some(tx) = url_tx {
        let _ = tx.send(AuthUrlInfo {
            url: auth_url.clone(),
            mode: AuthUrlMode::Loopback,
        });
    }

    let (cb_tx, mut cb_rx) = mpsc::channel::<CallbackResult>(1);
    let app = build_callback_router(cb_tx);
    let server = axum::serve(listener, app);
    tokio::spawn(async move {
        let _ = server.await;
    });

    let callback = tokio::time::timeout(AUTH_CALLBACK_TIMEOUT, cb_rx.recv())
        .await
        .map_err(|_| anyhow::anyhow!("Login timed out after 10 minutes. Please try again."))?
        .ok_or_else(|| anyhow::anyhow!("ChatGPT OAuth callback channel closed"))?
        .map_err(|e| anyhow::anyhow!("ChatGPT OAuth failed: {e}"))?;

    if !callback.state.is_empty() && callback.state != state {
        anyhow::bail!("ChatGPT OAuth state mismatch");
    }

    let tokens = exchange_code(&callback.code, &redirect_uri, &pkce.verifier).await?;
    let auth = auth_from_tokens(tokens);
    persist_openai_auth(grok_home, &auth)?;
    tracing::info!(
        email = ?auth.email,
        account = ?auth.chatgpt_account_id,
        "ChatGPT OAuth login complete"
    );
    Ok(auth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_contains_codex_params() {
        let pkce = generate_pkce();
        let url = build_authorize_url(
            "http://localhost:1455/auth/callback",
            &pkce,
            "state123",
        );
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("originator=grok_build"));
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
    }
}
