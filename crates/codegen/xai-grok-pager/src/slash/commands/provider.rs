//! `/provider` — select and log in to LLM auth providers (Grok, OpenAI).

use xai_grok_shell::auth::{ProviderId, format_provider_status, provider_statuses};

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

pub struct ProviderCommand;

impl SlashCommand for ProviderCommand {
    fn name(&self) -> &str {
        "provider"
    }

    fn aliases(&self) -> &[&str] {
        &["providers"]
    }

    fn description(&self) -> &str {
        "Select or log in to an LLM provider (Grok, OpenAI)"
    }

    fn usage(&self) -> &str {
        "/provider [grok|openai] | /provider login <grok|openai>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("[grok|openai|login …]")
    }

    fn suggest_args(&self, _ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let q = args_query.trim().to_ascii_lowercase();
        let mut items = vec![
            ArgItem {
                display: "grok".into(),
                match_text: "grok".into(),
                insert_text: "grok".into(),
                description: "Use Grok (xAI) as the active provider".into(),
            },
            ArgItem {
                display: "openai".into(),
                match_text: "openai chatgpt codex".into(),
                insert_text: "openai".into(),
                description: "Use OpenAI ChatGPT OAuth (Codex) as the active provider".into(),
            },
            ArgItem {
                display: "login grok".into(),
                match_text: "login grok".into(),
                insert_text: "login grok".into(),
                description: "Log in with Grok / xAI".into(),
            },
            ArgItem {
                display: "login openai".into(),
                match_text: "login openai chatgpt".into(),
                insert_text: "login openai".into(),
                description: "Log in with OpenAI ChatGPT OAuth".into(),
            },
        ];
        if !q.is_empty() {
            items.retain(|i| {
                i.match_text.contains(&q)
                    || i.display.contains(&q)
                    || i.description.to_ascii_lowercase().contains(&q)
            });
        }
        Some(items)
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            let statuses = provider_statuses(None);
            return CommandResult::Message(format_provider_status(&statuses));
        }

        let mut parts = trimmed.split_whitespace();
        let first = parts.next().unwrap_or_default();
        let second = parts.next();

        if first.eq_ignore_ascii_case("login") {
            let Some(name) = second else {
                return CommandResult::Error(
                    "Usage: /provider login <grok|openai>".into(),
                );
            };
            let Some(id) = ProviderId::parse(name) else {
                return CommandResult::Error(format!(
                    "Unknown provider '{name}'. Use grok or openai."
                ));
            };
            return CommandResult::Action(Action::LoginProvider(id));
        }

        if second.is_some() {
            return CommandResult::Error(
                "Usage: /provider [grok|openai] | /provider login <grok|openai>".into(),
            );
        }

        let Some(id) = ProviderId::parse(first) else {
            return CommandResult::Error(format!(
                "Unknown provider '{first}'. Use grok or openai."
            ));
        };
        CommandResult::Action(Action::SetProvider(id))
    }
}
