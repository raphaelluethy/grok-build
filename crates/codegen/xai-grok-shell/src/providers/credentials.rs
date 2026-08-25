//! Persistence for third-party LLM provider credentials (`~/.grok/providers.json`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use super::ProviderId;

const PROVIDERS_FILE: &str = "providers.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderStore {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openrouter: Option<ApiKeyCreds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_subscribed: Option<ChatGptCreds>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyCreds {
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatGptCreds {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl ChatGptCreds {
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        now + 5 * 60 * 1000 >= self.expires_at_ms
    }
}

pub fn providers_path() -> PathBuf {
    crate::util::grok_home::grok_home().join(PROVIDERS_FILE)
}

pub fn load() -> ProviderStore {
    load_from(&providers_path()).unwrap_or_default()
}

pub fn load_from(path: &Path) -> Result<ProviderStore> {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProviderStore::default());
        }
        Err(e) => return Err(e.into()),
    };
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Ok(ProviderStore::default());
    }
    serde_json::from_str(trimmed).context("parse providers.json")
}

pub fn save(store: &ProviderStore) -> Result<()> {
    save_to(&providers_path(), store)
}

pub fn save_to(path: &Path, store: &ProviderStore) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(store)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn set_api_key(id: ProviderId, api_key: String) -> Result<()> {
    let mut store = load();
    let creds = ApiKeyCreds { api_key };
    match id {
        ProviderId::OpenRouter => store.openrouter = Some(creds),
        ProviderId::ChatGpt => {
            anyhow::bail!("ChatGPT uses OAuth; run /connect chatgpt");
        }
        ProviderId::Xai => anyhow::bail!("xAI uses /login"),
    }
    save(&store)
}

pub fn clear(id: ProviderId) -> Result<()> {
    let mut store = load();
    match id {
        ProviderId::OpenRouter => store.openrouter = None,
        ProviderId::ChatGpt => store.openai_subscribed = None,
        ProviderId::Xai => {}
    }
    save(&store)
}

pub fn save_chatgpt(creds: ChatGptCreds) -> Result<()> {
    let mut store = load();
    store.openai_subscribed = Some(creds);
    save(&store)
}

/// First non-empty: stored key, then `env_var`.
pub fn resolve_api_key(stored: Option<&ApiKeyCreds>, env_var: &str) -> Option<String> {
    stored
        .map(|c| c.api_key.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            std::env::var(env_var)
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_file_is_empty_store() {
        let dir = TempDir::new().unwrap();
        let store = load_from(&dir.path().join("providers.json")).unwrap();
        assert!(store.openrouter.is_none());
        assert!(store.openai_subscribed.is_none());
    }

    #[test]
    fn round_trip_api_key() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("providers.json");
        let mut store = ProviderStore::default();
        store.openrouter = Some(ApiKeyCreds {
            api_key: "sk-or-test".into(),
        });
        save_to(&path, &store).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.openrouter.as_ref().unwrap().api_key, "sk-or-test");
    }

    #[test]
    fn resolve_prefers_stored_over_env() {
        let creds = ApiKeyCreds {
            api_key: "stored".into(),
        };
        assert_eq!(
            resolve_api_key(Some(&creds), "DEFINITELY_UNSET_GROK_PROVIDER_KEY"),
            Some("stored".into())
        );
    }
}
