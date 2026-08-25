//! `/connect` — attach OpenRouter or ChatGPT Plus/Pro models.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

/// Connect a third-party model provider.
pub struct ConnectCommand;

impl SlashCommand for ConnectCommand {
    fn name(&self) -> &str {
        "connect"
    }

    fn description(&self) -> &str {
        "Connect OpenRouter or ChatGPT Plus/Pro"
    }

    fn usage(&self) -> &str {
        "/connect <openrouter|chatgpt> [api-key]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<openrouter|chatgpt>")
    }

    fn suggest_args(&self, _ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        Some(vec![
            ArgItem {
                display: "openrouter".into(),
                match_text: "openrouter".into(),
                insert_text: "openrouter ".into(),
                description: "OpenRouter API key".into(),
            },
            ArgItem {
                display: "chatgpt".into(),
                match_text: "chatgpt".into(),
                insert_text: "chatgpt".into(),
                description: "ChatGPT Plus/Pro (browser sign-in)".into(),
            },
        ])
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        let mut parts = trimmed.split_whitespace();
        let provider = match parts.next() {
            Some(p) => p,
            None => {
                return CommandResult::Error(
                    "provider required: openrouter or chatgpt".to_string(),
                );
            }
        };

        match provider.to_ascii_lowercase().as_str() {
            "openrouter" => {
                let api_key: String = parts.collect::<Vec<_>>().join(" ").trim().to_string();
                if api_key.is_empty() {
                    return CommandResult::Error(format!(
                        "API key required for {provider}; usage: /connect {provider} <api-key>"
                    ));
                }
                CommandResult::Action(Action::ConnectProvider {
                    provider: provider.to_ascii_lowercase(),
                    api_key: Some(api_key),
                })
            }
            "chatgpt" => {
                if parts.next().is_some() {
                    return CommandResult::Error(
                        "ChatGPT uses browser sign-in; usage: /connect chatgpt".to_string(),
                    );
                }
                CommandResult::Action(Action::ConnectProvider {
                    provider: "chatgpt".to_string(),
                    api_key: None,
                })
            }
            _ => CommandResult::Error(format!(
                "Unknown provider {provider:?}; try openrouter or chatgpt"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;

    static DEFAULT_BUNDLE_STATE: crate::app::bundle::BundleState =
        crate::app::bundle::BundleState {
            has_cache: false,
            version: String::new(),
            personas: Vec::new(),
            roles: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        };

    fn make_ctx<'a>(models: &'a ModelState) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot {
                multiline_mode: false,
                yolo_mode: false,
                ..crate::settings::PagerLocalSnapshot::default()
            },
        }
    }

    #[test]
    fn openrouter_with_key() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        match ConnectCommand.run(&mut ctx, "openrouter sk-or-test") {
            CommandResult::Action(Action::ConnectProvider { provider, api_key }) => {
                assert_eq!(provider, "openrouter");
                assert_eq!(api_key.as_deref(), Some("sk-or-test"));
            }
            other => panic!("expected ConnectProvider action, got {other:?}"),
        }
    }

    #[test]
    fn chatgpt_without_key() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        match ConnectCommand.run(&mut ctx, "chatgpt") {
            CommandResult::Action(Action::ConnectProvider { provider, api_key }) => {
                assert_eq!(provider, "chatgpt");
                assert!(api_key.is_none());
            }
            other => panic!("expected ConnectProvider action, got {other:?}"),
        }
    }

    #[test]
    fn openrouter_without_key_errors() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        assert!(matches!(
            ConnectCommand.run(&mut ctx, "openrouter"),
            CommandResult::Error(_)
        ));
    }

    #[test]
    fn opencode_is_not_a_connectable_provider() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        assert!(matches!(
            ConnectCommand.run(&mut ctx, "opencode oc-test"),
            CommandResult::Error(_)
        ));
    }
}
