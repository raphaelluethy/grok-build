use super::*;
use xai_grok_status_line::StatusLineEffort;

const DIR: &str = "/home/user/project";

fn context() -> StatusLineContext {
    let mut ctx = crate::app::status_line::test_context(DIR);
    ctx.session_name = Some("status_line work".into());
    ctx.model.display_name = Some("Grok Build".into());
    ctx.cost.total_cost_usd = Some(0.3745);
    ctx.context_window.used_percentage = Some(42);
    ctx.context_window.auto_compact_threshold_percent = Some(80);
    ctx
}

fn plain(ctx: &StatusLineContext, turn_elapsed: Option<Duration>) -> String {
    compose_builtin(ctx, turn_elapsed, StatusLineItem::ALL)
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(SEGMENT_SEPARATOR)
}

#[test]
fn composes_every_segment_in_order() {
    assert_eq!(
        plain(&context(), Some(Duration::from_secs(83))),
        "project │ Grok Build │ 42% ctx │ $0.37 │ 1m23s │ status_line work"
    );
}

#[test]
fn omits_segments_whose_data_is_missing_or_rounds_to_zero() {
    let mut ctx = context();
    ctx.cost.total_cost_usd = Some(0.004);
    assert_eq!(
        plain(&ctx, Some(Duration::from_millis(400))),
        "project │ Grok Build │ 42% ctx │ status_line work"
    );

    ctx.cost.total_cost_usd = Some(0.006);
    assert!(plain(&ctx, None).contains("$0.01"));

    ctx.context_window.used_percentage = None;
    assert!(compose_builtin(&ctx, None, &[StatusLineItem::Context]).is_empty());
}

#[test]
fn name_past_its_budget_is_cut_by_painted_columns() {
    let mut ctx = context();
    ctx.session_name = Some("辺".repeat(SESSION_NAME_COLS));
    let cut = &compose_builtin(&ctx, None, &[StatusLineItem::SessionName])[0].text;

    let width = super::super::painted_width(cut);
    assert!(
        cut.ends_with('…'),
        "a cut name has to say it was cut: {cut}"
    );
    assert!(
        width <= SESSION_NAME_COLS,
        "{width} columns overruns the {SESSION_NAME_COLS} the segment was given"
    );
    // Within one cluster of the budget, which a byte or character cut is not.
    assert!(
        width + 2 > SESSION_NAME_COLS,
        "{width} columns leaves more than a cluster of the budget unused"
    );
}

#[test]
fn cost_the_session_does_not_have_omits_its_segment() {
    let mut ctx = context();
    ctx.cost.total_cost_usd = None;
    assert!(compose_builtin(&ctx, None, &[StatusLineItem::Cost]).is_empty());
}

#[test]
fn context_segment_warns_near_compaction() {
    let mut ctx = context();
    let tone =
        |ctx: &StatusLineContext| compose_builtin(ctx, None, &[StatusLineItem::Context])[0].tone;

    ctx.context_window.used_percentage = Some(90);
    assert_eq!(tone(&ctx), SegmentTone::Warn);
    ctx.context_window.used_percentage = Some(50);
    assert_eq!(tone(&ctx), SegmentTone::Dim);

    ctx.context_window.auto_compact_threshold_percent = Some(65);
    ctx.context_window.used_percentage = Some(70);
    assert_eq!(tone(&ctx), SegmentTone::Warn);
}

fn set_effort(ctx: &mut StatusLineContext, level: &str) {
    ctx.effort = Some(StatusLineEffort {
        level: level.into(),
    });
}

fn model_text(ctx: &StatusLineContext) -> Option<String> {
    compose_builtin(ctx, None, &[StatusLineItem::Model])
        .into_iter()
        .next()
        .map(|s| s.text)
}

#[test]
fn model_segment_includes_effort_and_fast_when_enabled() {
    let mut ctx = context();
    set_effort(&mut ctx, "high");
    ctx.model.fast_mode = Some(true);
    assert_eq!(
        model_text(&ctx).as_deref(),
        Some("Grok Build (high) · fast")
    );
}

#[test]
fn model_segment_marks_a_fast_capable_model_as_standard_when_off() {
    let mut ctx = context();
    set_effort(&mut ctx, "high");
    ctx.model.fast_mode = Some(false);
    assert_eq!(
        model_text(&ctx).as_deref(),
        Some("Grok Build (high) · standard")
    );
}

#[test]
fn model_segment_omits_fast_indicators_when_the_model_does_not_support_it() {
    let mut ctx = context();
    set_effort(&mut ctx, "high");
    ctx.model.fast_mode = None;
    assert_eq!(model_text(&ctx).as_deref(), Some("Grok Build (high)"));
}

#[test]
fn model_segment_omits_effort_when_unset() {
    let mut ctx = context();
    ctx.model.fast_mode = Some(true);
    assert_eq!(model_text(&ctx).as_deref(), Some("Grok Build · fast"));
}

#[test]
fn model_segment_is_omitted_without_a_display_name() {
    let mut ctx = context();
    ctx.model.display_name = None;
    set_effort(&mut ctx, "high");
    ctx.model.fast_mode = Some(true);
    assert!(compose_builtin(&ctx, None, &[StatusLineItem::Model]).is_empty());

    ctx.model.display_name = Some(String::new());
    assert!(compose_builtin(&ctx, None, &[StatusLineItem::Model]).is_empty());
}

#[test]
fn model_label_is_fitted_after_effort_and_fast_suffixes() {
    let mut ctx = context();
    ctx.model.display_name = Some("M".repeat(MODEL_COLS));
    set_effort(&mut ctx, "high");
    ctx.model.fast_mode = Some(true);
    let cut = model_text(&ctx).expect("a named model still paints");
    let width = super::super::painted_width(&cut);
    assert!(
        cut.ends_with('…'),
        "a combined label past the budget has to say it was cut: {cut}"
    );
    assert!(
        width <= MODEL_COLS,
        "{width} columns overruns the {MODEL_COLS} the segment was given; \
         suffixes must be fitted with the name, not appended after a pre-cut"
    );
}
