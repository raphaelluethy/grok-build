//! `/fast` -- toggle Codex Fast Mode (priority service tier).
//!
//! Dispatches `Action::SetCodexFastMode(!current)`. The dispatcher handles
//! state mutation, persistence (with rollback on disk-write failure), and
//! toast.
//!
//! Visible only when the current model advertises `supportsFastMode`. A
//! typed `/fast` on an unsupported model returns `CommandResult::Error`
//! rather than toggling.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, CommandExecCtx, CommandResult, SlashCommand};

/// Toggle Codex Fast Mode (priority service tier).
pub struct FastCommand;

impl SlashCommand for FastCommand {
    fn name(&self) -> &str {
        "fast"
    }

    fn description(&self) -> &str {
        "Toggle fast mode (priority service tier)"
    }

    fn usage(&self) -> &str {
        "/fast"
    }

    fn visible(&self, ctx: &AppCtx) -> bool {
        ctx.models.current_model_supports_fast_mode()
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        if !ctx.models.current_model_supports_fast_mode() {
            return CommandResult::Error("Current model does not support fast mode".into());
        }
        let new = !ctx.pager_state.codex_fast_mode;
        CommandResult::Action(Action::SetCodexFastMode(new))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;
    use agent_client_protocol as acp;
    use std::sync::Arc;

    fn model_with_fast_mode(supports: bool) -> ModelState {
        let id = acp::ModelId::new(Arc::from("chatgpt/gpt-5.4"));
        let mut state = ModelState::default();
        state.available.insert(
            id.clone(),
            acp::ModelInfo::new(id.clone(), "GPT-5.4".to_string()).meta(
                serde_json::json!({ "supportsFastMode": supports })
                    .as_object()
                    .cloned(),
            ),
        );
        state.current = Some(id);
        state
    }

    fn make_ctx<'a>(
        models: &'a ModelState,
        bundle: &'a BundleState,
        codex_fast_mode: bool,
    ) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot {
                codex_fast_mode,
                ..PagerLocalSnapshot::default()
            },
        }
    }

    fn make_app_ctx<'a>(models: &'a ModelState) -> AppCtx<'a> {
        AppCtx {
            models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: true,
            saved_workflows: &[],
            workflow_runs: &[],
            screen_mode: crate::app::ScreenMode::Fullscreen,
            current_title: None,
        }
    }

    #[test]
    fn off_turns_fast_on() {
        let models = model_with_fast_mode(true);
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle, false);
        assert!(matches!(
            FastCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::SetCodexFastMode(true))
        ));
    }

    #[test]
    fn on_turns_fast_off() {
        let models = model_with_fast_mode(true);
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle, true);
        assert!(matches!(
            FastCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::SetCodexFastMode(false))
        ));
    }

    #[test]
    fn unsupported_returns_error() {
        let models = model_with_fast_mode(false);
        let bundle = BundleState::default();
        let mut ctx = make_ctx(&models, &bundle, false);
        match FastCommand.run(&mut ctx, "") {
            CommandResult::Error(msg) => {
                assert!(
                    msg.contains("does not support fast mode"),
                    "unexpected error: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn visible_when_supported() {
        let models = model_with_fast_mode(true);
        assert!(FastCommand.visible(&make_app_ctx(&models)));
    }

    #[test]
    fn hidden_when_unsupported() {
        let models = model_with_fast_mode(false);
        assert!(!FastCommand.visible(&make_app_ctx(&models)));
        let empty = ModelState::default();
        assert!(!FastCommand.visible(&make_app_ctx(&empty)));
    }
}
