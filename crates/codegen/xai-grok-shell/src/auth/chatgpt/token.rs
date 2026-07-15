//! JWT claim helpers for ChatGPT OAuth tokens (account id + email).

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatgptTokenClaims {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub chatgpt_account_id: Option<String>,
    #[serde(default)]
    pub organizations: Option<Vec<ChatgptOrg>>,
    #[serde(default, rename = "https://api.openai.com/auth")]
    pub api_auth: Option<ChatgptApiAuthClaim>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatgptOrg {
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatgptApiAuthClaim {
    #[serde(default)]
    pub chatgpt_account_id: Option<String>,
}

pub fn parse_jwt_claims(token: &str) -> Option<ChatgptTokenClaims> {
    jsonwebtoken::dangerous::insecure_decode::<ChatgptTokenClaims>(token)
        .ok()
        .map(|d| d.claims)
}

pub fn extract_account_id(claims: &ChatgptTokenClaims) -> Option<String> {
    claims
        .chatgpt_account_id
        .clone()
        .or_else(|| {
            claims
                .api_auth
                .as_ref()
                .and_then(|a| a.chatgpt_account_id.clone())
        })
        .or_else(|| {
            claims
                .organizations
                .as_ref()
                .and_then(|orgs| orgs.first())
                .and_then(|o| o.id.clone())
        })
        .filter(|s| !s.is_empty())
}

pub fn extract_email(claims: &ChatgptTokenClaims) -> Option<String> {
    claims.email.clone().filter(|s| !s.is_empty())
}

/// Prefer id_token claims, then access_token claims.
pub fn account_id_from_tokens(id_token: Option<&str>, access_token: &str) -> Option<String> {
    id_token
        .and_then(parse_jwt_claims)
        .as_ref()
        .and_then(extract_account_id)
        .or_else(|| parse_jwt_claims(access_token).as_ref().and_then(extract_account_id))
}

pub fn email_from_tokens(id_token: Option<&str>, access_token: &str) -> Option<String> {
    id_token
        .and_then(parse_jwt_claims)
        .as_ref()
        .and_then(extract_email)
        .or_else(|| parse_jwt_claims(access_token).as_ref().and_then(extract_email))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn jwt(payload: &str) -> String {
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.sig",
            enc.encode(r#"{"alg":"none"}"#),
            enc.encode(payload)
        )
    }

    #[test]
    fn extracts_account_from_nested_claim() {
        let token = jwt(
            r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-1"},"email":"a@b.c"}"#,
        );
        let claims = parse_jwt_claims(&token).unwrap();
        assert_eq!(extract_account_id(&claims).as_deref(), Some("acct-1"));
        assert_eq!(extract_email(&claims).as_deref(), Some("a@b.c"));
    }
}
