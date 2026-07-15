//! ChatGPT / Codex OAuth (OpenAI auth.openai.com).
//!
//! Implements the same Authorization Code + PKCE flow used by the official
//! Codex CLI and OpenCode:
//! - client_id `app_EMoamEEZ73f0CkXaXp7hrann`
//! - authorize params `id_token_add_organizations`, `codex_cli_simplified_flow`
//! - loopback callback on `http://localhost:1455/auth/callback`
//! - token refresh against `{issuer}/oauth/token`
//! - inference via `https://chatgpt.com/backend-api/codex/responses`

mod login;
mod refresh;
mod token;

pub use login::{run_chatgpt_login, run_chatgpt_login_with_channels};
pub use refresh::refresh_chatgpt_tokens;
pub use token::{
    ChatgptTokenClaims, extract_account_id, extract_email, parse_jwt_claims,
};

pub(crate) use login::CALLBACK_PATH;
