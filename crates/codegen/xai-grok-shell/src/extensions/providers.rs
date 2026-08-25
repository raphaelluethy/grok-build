//! `x.ai/providers/*` extension handlers for third-party model providers.

use agent_client_protocol as acp;
use serde::Deserialize;

use super::{ExtResult, parse_params};
use crate::agent::MvpAgent;
use crate::providers::{ProviderId, credentials};
use crate::session::ExtMethodResult;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectParams {
    provider: String,
    #[serde(default)]
    api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DisconnectParams {
    provider: String,
}

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "x.ai/providers/connect" => handle_connect(agent, args).await,
        "x.ai/providers/disconnect" => handle_disconnect(agent, args),
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_connect(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ConnectParams = parse_params(args)?;
    let provider_id = parse_connectable_provider(&params.provider)?;

    match provider_id {
        ProviderId::OpenRouter => {
            let key = params
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| acp::Error::invalid_params().data("apiKey required"))?;
            credentials::set_api_key(provider_id, key.to_string())
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
        }
        ProviderId::ChatGpt => {
            crate::providers::openai_subscribed::sign_in()
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
        }
        ProviderId::Xai => unreachable!(),
    }

    let count = apply_provider_config(agent);
    ExtMethodResult::success(serde_json::json!({
        "ok": true,
        "provider": provider_id.as_str(),
        "models": count,
    }))
    .to_ext_response()
    .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

fn handle_disconnect(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: DisconnectParams = parse_params(args)?;
    let provider_id = parse_connectable_provider(&params.provider)?;

    credentials::clear(provider_id)
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

    let count = apply_provider_config(agent);
    ExtMethodResult::success(serde_json::json!({
        "ok": true,
        "provider": provider_id.as_str(),
        "models": count,
    }))
    .to_ext_response()
    .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

fn parse_connectable_provider(provider: &str) -> Result<ProviderId, acp::Error> {
    ProviderId::parse(provider)
        .filter(|id| !matches!(id, ProviderId::Xai))
        .ok_or_else(|| acp::Error::invalid_params().data(format!("unknown provider: {provider}")))
}

fn apply_provider_config(agent: &MvpAgent) -> usize {
    let cfg = agent.cfg.borrow().clone();
    agent.models_manager.apply_config(cfg);
    agent.models_manager.models().len()
}
