use super::*;

use crate::app::agent::AgentId;
use crate::app::agent_view::CodexFastModeGuard;
use crate::app::app_view::tests::test_app_with_agent;
use crate::app::status_line::test_context;
use agent_client_protocol as acp;
use std::sync::Arc;
use xai_grok_shell::sampling::types::ReasoningEffort;
use xai_grok_status_line::StatusLineEffort;

/// One labelled single-field change to the base inputs.
type Tweak = (&'static str, fn(&mut TickInputs));

fn quiet() -> TickInputs {
    TickInputs {
        row_is_drawn: true,
        settled: true,
        source_changed: false,
        client_fields_changed: false,
        turn_timer_running: false,
        has_agent: true,
        forced: false,
        refresh_due: false,
        run: RunSlot::Free,
    }
}

/// Every reason to repaint, so a suppression below is visible.
fn busy() -> TickInputs {
    TickInputs {
        settled: false,
        source_changed: true,
        client_fields_changed: true,
        turn_timer_running: true,
        forced: true,
        refresh_due: true,
        ..quiet()
    }
}

#[test]
fn quiet_settled_row_lets_the_loop_park() {
    assert_eq!(status_line_tick_demand(quiet()), TickDemand::None);
}

#[test]
fn each_reason_to_repaint_asks_for_ticks() {
    let reasons: [Tweak; 6] = [
        ("a turn is running", |i| i.turn_timer_running = true),
        ("the agent changed", |i| i.source_changed = true),
        ("the session was renamed", |i| {
            i.client_fields_changed = true
        }),
        ("nothing has painted yet", |i| i.settled = false),
        // A force deferred by the floor still needs the tick that runs it.
        ("a force is owed", |i| i.forced = true),
        // Raised behind a busy slot or a hidden row; the tick carries it.
        ("a refresh is owed", |i| i.refresh_due = true),
    ];
    for (why, raise) in reasons {
        let mut inputs = quiet();
        raise(&mut inputs);
        assert_eq!(status_line_tick_demand(inputs), TickDemand::Slow, "{why}");
    }
    let lost_run = TickInputs {
        run: RunSlot::PastDeadline,
        ..quiet()
    };
    assert_eq!(status_line_tick_demand(lost_run), TickDemand::Slow);
}

#[test]
fn suppressor_beats_every_reason_to_repaint() {
    let suppressors: [Tweak; 3] = [
        ("no agent attached", |i| i.has_agent = false),
        // Minimal mode and a row that is off both reach here as one answer.
        ("no row is drawn", |i| i.row_is_drawn = false),
        // A run answers through its own task result, not through a tick.
        ("a run is outstanding", |i| i.run = RunSlot::WithinDeadline),
    ];
    for (why, suppress) in suppressors {
        let mut inputs = busy();
        suppress(&mut inputs);
        assert_eq!(status_line_tick_demand(inputs), TickDemand::None, "{why}");
    }
}

#[test]
fn each_disposition_earns_the_log_line_the_guide_promises() {
    use crate::app::status_line::{FinishDisposition, REFRESH_FAILURES_TO_PAINT};

    let painted = |failures| FinishDisposition::RefreshFailurePainted {
        error: "exit 7".into(),
        failures,
    };
    let line = |disposition| {
        refresh_failure_log(disposition).map(|line| (line.level, line.message, line.failures))
    };

    assert!(refresh_failure_log(FinishDisposition::Applied).is_none());
    assert_eq!(
        line(FinishDisposition::RefreshFailureKept {
            error: "exit 7".into(),
            failures: 1,
        }),
        Some((
            RefreshFailureLogLevel::Debug,
            "status_line: refresh run failed; keeping the last output",
            1,
        )),
        "a kept failure changed nothing the user can see"
    );
    assert_eq!(
        line(painted(1)),
        Some((
            RefreshFailureLogLevel::Warn,
            "status_line: refresh run failed; painting the error",
            1,
        )),
        "painting an unanswered row's first failure is user visible and warns"
    );
    assert_eq!(
        line(painted(REFRESH_FAILURES_TO_PAINT)).map(|(level, ..)| level),
        Some(RefreshFailureLogLevel::Warn),
        "so is the strike that crossed the threshold"
    );
    assert_eq!(
        line(painted(REFRESH_FAILURES_TO_PAINT + 1)).map(|(level, ..)| level),
        Some(RefreshFailureLogLevel::Debug),
        "a script broken all night must not write a warn line per interval"
    );
    assert_eq!(
        refresh_failure_log(painted(1)).map(|line| line.error),
        Some("exit 7".to_string()),
        "the raw error rides the line into the log context"
    );
}

fn install_model(app: &mut AppView, supports_fast: bool) {
    let id = acp::ModelId::new(Arc::from("grok-4.5"));
    let meta = supports_fast.then(|| {
        serde_json::json!({ "supportsFastMode": true })
            .as_object()
            .cloned()
            .expect("object")
    });
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.session.models.available.insert(
        id.clone(),
        acp::ModelInfo::new(id.clone(), "Grok 4.5".to_string()).meta(meta),
    );
    agent.session.models.current = Some(id);
    agent.status_context = Some(test_context("/tmp"));
}

#[test]
fn overlay_copies_live_effort_and_fast_onto_the_snapshot() {
    let _fast = CodexFastModeGuard::set(true);
    let mut app = test_app_with_agent();
    install_model(&mut app, true);
    app.agents
        .get_mut(&AgentId(0))
        .unwrap()
        .session
        .models
        .reasoning_effort = Some(ReasoningEffort::High);

    let ctx = app.shell_status_context().expect("snapshot is present");
    assert_eq!(ctx.effort.as_ref().map(|e| e.level.as_str()), Some("high"));
    assert_eq!(ctx.model.fast_mode, Some(true));

    let json = serde_json::to_value(&ctx).expect("the command stdin serializes");
    assert_eq!(json["effort"]["level"], "high");
    assert_eq!(json["model"]["fast_mode"], true);
    assert!(
        json["model"].get("reasoning_effort").is_none(),
        "effort lives at the canonical top-level field, not on model"
    );
}

#[test]
fn overlay_reports_fast_false_when_a_capable_model_has_it_off() {
    let _fast = CodexFastModeGuard::set(false);
    let mut app = test_app_with_agent();
    install_model(&mut app, true);

    let ctx = app.shell_status_context().expect("snapshot is present");
    assert_eq!(ctx.model.fast_mode, Some(false));
    let json = serde_json::to_value(&ctx).expect("the command stdin serializes");
    assert_eq!(json["model"]["fast_mode"], false);
}

#[test]
fn overlay_omits_fast_when_the_model_does_not_support_it() {
    let _fast = CodexFastModeGuard::set(true);
    let mut app = test_app_with_agent();
    install_model(&mut app, false);

    let ctx = app.shell_status_context().expect("snapshot is present");
    assert_eq!(ctx.model.fast_mode, None);
    let json = serde_json::to_value(&ctx).expect("the command stdin serializes");
    assert!(
        json["model"].get("fast_mode").is_none(),
        "an unsupported model must not look like non-fast: {}",
        json["model"]
    );
}

#[test]
fn overlay_effort_follows_the_live_model_state_not_the_snapshot() {
    let mut app = test_app_with_agent();
    install_model(&mut app, false);
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.status_context.as_mut().unwrap().effort = Some(StatusLineEffort {
            level: "stale".into(),
        });
        agent.session.models.reasoning_effort = Some(ReasoningEffort::Low);
    }

    let ctx = app.shell_status_context().expect("snapshot is present");
    assert_eq!(ctx.effort.as_ref().map(|e| e.level.as_str()), Some("low"));
}

#[test]
fn overlay_clears_stale_effort_when_live_is_unset() {
    let mut app = test_app_with_agent();
    install_model(&mut app, false);
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.status_context.as_mut().unwrap().effort = Some(StatusLineEffort {
            level: "stale".into(),
        });
        agent.session.models.reasoning_effort = None;
    }

    let ctx = app.shell_status_context().expect("snapshot is present");
    assert!(ctx.effort.is_none());
    let json = serde_json::to_value(&ctx).expect("the command stdin serializes");
    assert!(json.get("effort").is_none());
}
