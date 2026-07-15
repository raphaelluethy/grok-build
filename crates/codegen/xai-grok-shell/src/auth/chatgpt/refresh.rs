//! ChatGPT OAuth token refresh (hardcoded token endpoint, Codex-compatible).

use chrono::{Duration, Utc};

use super::token::{account_id_from_tokens, email_from_tokens};
use crate::auth::model::GrokAuth;
use crate::auth::provider::{OPENAI_OAUTH_CLIENT_ID, OPENAI_OAUTH_ISSUER, is_openai_auth};

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

/// Refresh a ChatGPT OAuth access token. Returns an updated [`GrokAuth`].
pub async fn refresh_chatgpt_tokens(auth: &GrokAuth) -> anyhow::Result<GrokAuth> {
    if !is_openai_auth(auth) {
        anyhow::bail!("not a ChatGPT OAuth credential");
    }
    let refresh = auth
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing refresh_token"))?;
    let issuer = auth
        .oidc_issuer
        .as_deref()
        .unwrap_or(OPENAI_OAUTH_ISSUER)
        .trim_end_matches('/');
    let client_id = auth
        .oidc_client_id
        .as_deref()
        .unwrap_or(OPENAI_OAUTH_CLIENT_ID);

    let token_url = format!("{issuer}/oauth/token");
    let client = crate::http::shared_client();
    let resp = client
        .post(&token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", client_id),
        ])
        .send()
        .await?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("ChatGPT token refresh failed: HTTP {status} — {body}");
    }
    let tokens: TokenResponse = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("ChatGPT token refresh parse failed: {e}"))?;

    let now = Utc::now();
    let account_id = account_id_from_tokens(tokens.id_token.as_deref(), &tokens.access_token)
        .or_else(|| auth.chatgpt_account_id.clone());
    let email = email_from_tokens(tokens.id_token.as_deref(), &tokens.access_token)
        .or_else(|| auth.email.clone());

    Ok(GrokAuth {
        key: tokens.access_token,
        auth_mode: crate::auth::AuthMode::Oidc,
        create_time: now,
        user_id: auth.user_id.clone(),
        email,
        refresh_token: tokens.refresh_token.or_else(|| auth.refresh_token.clone()),
        expires_at: tokens
            .expires_in
            .map(|s| now + Duration::seconds(s as i64)),
        oidc_issuer: Some(issuer.to_owned()),
        oidc_client_id: Some(client_id.to_owned()),
        chatgpt_account_id: account_id,
        // Carry profile fields.
        first_name: auth.first_name.clone(),
        last_name: auth.last_name.clone(),
        profile_image_asset_id: auth.profile_image_asset_id.clone(),
        principal_type: auth.principal_type.clone(),
        principal_id: auth.principal_id.clone(),
        team_id: auth.team_id.clone(),
        team_name: auth.team_name.clone(),
        team_role: auth.team_role.clone(),
        organization_id: auth.organization_id.clone(),
        organization_name: auth.organization_name.clone(),
        organization_role: auth.organization_role.clone(),
        user_blocked_reason: auth.user_blocked_reason.clone(),
        team_blocked_reasons: auth.team_blocked_reasons.clone(),
        coding_data_retention_opt_out: auth.coding_data_retention_opt_out,
        has_grok_code_access: None,
    })
}
