use agent_client_protocol as acp;
use serde::Serialize;
use xai_grok_sampling_types::{ReasoningEffort, ReasoningEffortOption};

use crate::session::unified_list::SessionKind;

pub(crate) const SELECTABLE_REASONING_EFFORTS: [ReasoningEffort; 5] = [
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
];

pub(crate) const CONFIG_ID_MODEL: &str = "model";
pub(crate) const CONFIG_ID_REASONING_EFFORT: &str = "reasoning_effort";
pub(crate) const CONFIG_ID_FAST_MODE: &str = "fast_mode";

/// Session `_meta` / initialize `_meta` key carrying whether the client
/// supports standard ACP boolean config options. Leader mode injects this
/// from the raw initialize shape
/// `params.clientCapabilities.session.configOptions.boolean`.
pub(crate) const BOOLEAN_CONFIG_OPTIONS_META_KEY: &str = "x.ai/booleanConfigOptions";

const ZED_CLIENT_INFO_NAME: &str = "zed";

/// Whether standard boolean config options may be emitted for a session.
///
/// Preference order:
/// 1. Per-session compatibility metadata (leader-injected `_meta`)
/// 2. Stored per-session verdict (resident handle)
/// 3. Direct stdio: ACP `clientInfo.name == "zed"`
/// 4. Optional namespaced compatibility key on initialize `_meta`
pub(crate) fn client_supports_standard_boolean_config_options(
    session_meta: Option<&acp::Meta>,
    session_stored: Option<bool>,
    init: Option<&acp::InitializeRequest>,
) -> bool {
    if let Some(value) = meta_boolean_config_options(session_meta) {
        return value;
    }
    if let Some(stored) = session_stored {
        return stored;
    }
    let Some(init) = init else {
        return false;
    };
    if init
        .client_info
        .as_ref()
        .is_some_and(|info| info.name == ZED_CLIENT_INFO_NAME)
    {
        return true;
    }
    meta_boolean_config_options(init.meta.as_ref()).unwrap_or(false)
}

fn meta_boolean_config_options(meta: Option<&acp::Meta>) -> Option<bool> {
    meta.and_then(|m| m.get(BOOLEAN_CONFIG_OPTIONS_META_KEY))
        .and_then(|v| v.as_bool())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionConfigOption {
    pub id: String,
    pub category: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrokSessionDetail {
    pub session_id: String,
    pub kind: String,
    pub cwd: String,
    pub current_model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl GrokSessionDetail {
    pub(crate) fn build(
        session_id: String,
        cwd: String,
        current_model_id: String,
        title: Option<String>,
    ) -> Self {
        Self {
            session_id,
            kind: SessionKind::Build.as_str().to_string(),
            cwd,
            current_model_id,
            title,
        }
    }
}

fn effort_label(effort: ReasoningEffort) -> String {
    match effort {
        ReasoningEffort::None => "None",
        ReasoningEffort::Minimal => "Minimal",
        ReasoningEffort::Low => "Low",
        ReasoningEffort::Medium => "Medium",
        ReasoningEffort::High => "High",
        ReasoningEffort::Xhigh => "X-High",
        ReasoningEffort::Max => "Max",
    }
    .to_string()
}

/// The built-in session-picker modes used when the model has no server list.
/// Reproduces the historical five rows and their labels.
pub(crate) fn legacy_session_effort_options() -> Vec<ReasoningEffortOption> {
    SELECTABLE_REASONING_EFFORTS
        .iter()
        .map(|&effort| ReasoningEffortOption {
            id: effort.as_str().to_string(),
            value: effort,
            label: effort_label(effort),
            description: None,
            default: false,
        })
        .collect()
}

fn model_display_name(model: &acp::ModelInfo) -> String {
    if model.name.is_empty() {
        model.model_id.0.to_string()
    } else {
        model.name.clone()
    }
}

pub(crate) fn build_session_config_options(
    available_models: &[acp::ModelInfo],
    current_model_id: &acp::ModelId,
    effort_options: &[ReasoningEffortOption],
    current_effort: Option<ReasoningEffort>,
) -> Vec<SessionConfigOption> {
    let mut options = Vec::with_capacity(available_models.len() + effort_options.len());

    for model in available_models {
        options.push(SessionConfigOption {
            id: model.model_id.0.to_string(),
            category: "model".to_string(),
            label: model_display_name(model),
            description: None,
            selected: model.model_id == *current_model_id,
        });
    }

    for effort in effort_options {
        options.push(SessionConfigOption {
            id: effort.id.clone(),
            category: "mode".to_string(),
            label: effort.label.clone(),
            description: effort.description.clone(),
            selected: Some(effort.value) == current_effort,
        });
    }

    options
}

pub(crate) fn build_acp_config_options(
    available_models: &[acp::ModelInfo],
    current_model_id: &acp::ModelId,
    effort_options: &[ReasoningEffortOption],
    current_effort: Option<ReasoningEffort>,
) -> Vec<acp::SessionConfigOption> {
    let mut options = Vec::new();

    if !available_models.is_empty() {
        let values: Vec<acp::SessionConfigSelectOption> = available_models
            .iter()
            .map(|model| {
                acp::SessionConfigSelectOption::new(
                    model.model_id.0.to_string(),
                    model_display_name(model),
                )
            })
            .collect();
        // Keep the real model even if the catalog doesn't list it.
        let current_value = current_model_id.0.to_string();
        options.push(
            acp::SessionConfigOption::select(CONFIG_ID_MODEL, "Model", current_value, values)
                .category(acp::SessionConfigOptionCategory::Model),
        );
    }

    if !effort_options.is_empty() {
        // Keep the real effort even if it isn't a listed option (e.g. none/max);
        // fall back to the model default when none is set.
        let current_value = match current_effort {
            Some(effort) => effort_options
                .iter()
                .find(|option| option.value == effort)
                .map(|option| option.id.clone())
                .unwrap_or_else(|| effort.as_str().to_string()),
            None => effort_options
                .iter()
                .find(|option| option.default)
                .unwrap_or(&effort_options[0])
                .id
                .clone(),
        };
        let values: Vec<acp::SessionConfigSelectOption> = effort_options
            .iter()
            .map(|option| {
                let mut value =
                    acp::SessionConfigSelectOption::new(option.id.clone(), option.label.clone());
                if let Some(description) = &option.description {
                    value = value.description(description.clone());
                }
                value
            })
            .collect();
        options.push(
            acp::SessionConfigOption::select(
                CONFIG_ID_REASONING_EFFORT,
                "Reasoning Effort",
                current_value,
                values,
            )
            .category(acp::SessionConfigOptionCategory::ThoughtLevel),
        );
    }

    options
}

/// Fast-mode inputs supplied by the session-handle / capability slice.
/// This module does not own process-global fast-mode state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FastModeOptionInput {
    pub current_value: bool,
    pub boolean_supported: bool,
    pub model_fast_capable: bool,
}

impl FastModeOptionInput {
    fn should_emit(self) -> bool {
        self.boolean_supported && self.model_fast_capable
    }
}

/// Append capability-gated Fast Mode after upstream model/effort selectors.
pub(crate) fn with_fast_mode_option(
    mut options: Vec<acp::SessionConfigOption>,
    fast_mode: Option<FastModeOptionInput>,
) -> Vec<acp::SessionConfigOption> {
    if fast_mode.is_some_and(FastModeOptionInput::should_emit) {
        let current_value = fast_mode.map(|input| input.current_value).unwrap_or(false);
        options.push(
            acp::SessionConfigOption::boolean(CONFIG_ID_FAST_MODE, "Fast Mode", current_value)
                .description("Use the priority service tier for lower latency")
                .category(acp::SessionConfigOptionCategory::Other(
                    "model_config".to_string(),
                )),
        );
    }
    options
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedSessionConfigSet {
    Model(acp::ModelId),
    ReasoningEffort(ReasoningEffort),
    FastMode(bool),
}

fn invalid_config_params(message: &str) -> acp::Error {
    acp::Error::invalid_params().data(message)
}

fn select_has_value(
    options: &acp::SessionConfigSelectOptions,
    value: &acp::SessionConfigValueId,
) -> bool {
    match options {
        acp::SessionConfigSelectOptions::Ungrouped(opts) => {
            opts.iter().any(|option| &option.value == value)
        }
        acp::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .any(|group| group.options.iter().any(|option| &option.value == value)),
        _ => false,
    }
}

fn effort_for_config_value(
    value: &str,
    effort_options: &[ReasoningEffortOption],
) -> Option<ReasoningEffort> {
    effort_options
        .iter()
        .find(|option| option.id == value || option.value.as_str() == value)
        .map(|option| option.value)
        .or_else(|| value.parse().ok())
}

/// Validate a `session/set_config_option` request against the currently
/// offered standard ACP options. Unknown IDs, type mismatches, and values
/// outside the advertised set are `invalid_params`.
pub(crate) fn parse_set_session_config_option(
    args: &acp::SetSessionConfigOptionRequest,
    offered: &[acp::SessionConfigOption],
    effort_options: &[ReasoningEffortOption],
) -> Result<ParsedSessionConfigSet, acp::Error> {
    let config_id = args.config_id.0.as_ref();
    let option = offered
        .iter()
        .find(|option| option.id.0.as_ref() == config_id)
        .ok_or_else(|| invalid_config_params("unknown config id"))?;
    match &option.kind {
        acp::SessionConfigKind::Select(select) => {
            let value_id = args
                .value
                .as_value_id()
                .ok_or_else(|| invalid_config_params("expected a select value id"))?;
            if !select_has_value(&select.options, value_id) {
                return Err(invalid_config_params("unknown option value"));
            }
            match config_id {
                CONFIG_ID_MODEL => Ok(ParsedSessionConfigSet::Model(acp::ModelId::new(
                    value_id.0.clone(),
                ))),
                CONFIG_ID_REASONING_EFFORT => {
                    let effort = effort_for_config_value(value_id.0.as_ref(), effort_options)
                        .ok_or_else(|| invalid_config_params("unknown option value"))?;
                    Ok(ParsedSessionConfigSet::ReasoningEffort(effort))
                }
                _ => Err(invalid_config_params("unknown config id")),
            }
        }
        acp::SessionConfigKind::Boolean(_) => {
            let value = args
                .value
                .as_bool()
                .ok_or_else(|| invalid_config_params("expected a boolean value"))?;
            if config_id != CONFIG_ID_FAST_MODE {
                return Err(invalid_config_params("unknown config id"));
            }
            Ok(ParsedSessionConfigSet::FastMode(value))
        }
        _ => Err(invalid_config_params("unsupported config option kind")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &'static str, name: &str) -> acp::ModelInfo {
        acp::ModelInfo::new(acp::ModelId::new(id), name.to_string())
    }

    #[test]
    fn options_have_one_selected_model_and_a_mode_per_effort() {
        let models = [
            model("grok-build", "Grok Build"),
            model("grok-4.5", "Grok 4.5"),
        ];
        let current = acp::ModelId::from("grok-build");
        let opts = build_session_config_options(
            &models,
            &current,
            &legacy_session_effort_options(),
            Some(ReasoningEffort::High),
        );

        let model_opts: Vec<_> = opts.iter().filter(|o| o.category == "model").collect();
        assert_eq!(model_opts.len(), 2);
        let selected_models: Vec<_> = model_opts.iter().filter(|o| o.selected).collect();
        assert_eq!(selected_models.len(), 1);
        assert_eq!(selected_models[0].id, "grok-build");

        let mode_opts: Vec<_> = opts.iter().filter(|o| o.category == "mode").collect();
        assert_eq!(mode_opts.len(), SELECTABLE_REASONING_EFFORTS.len());
        let selected_modes: Vec<_> = mode_opts.iter().filter(|o| o.selected).collect();
        assert_eq!(selected_modes.len(), 1);
        assert_eq!(selected_modes[0].id, "high");
        assert_eq!(selected_modes[0].label, "High");
    }

    #[test]
    fn none_effort_is_not_a_user_selectable_mode() {
        assert!(!SELECTABLE_REASONING_EFFORTS.contains(&ReasoningEffort::None));
        let models = [model("grok-build", "Grok Build")];
        let current = acp::ModelId::from("grok-build");
        let opts = build_session_config_options(
            &models,
            &current,
            &legacy_session_effort_options(),
            Some(ReasoningEffort::None),
        );
        let modes: Vec<_> = opts.iter().filter(|o| o.category == "mode").collect();
        assert!(modes.iter().all(|o| o.id != "none"));
        assert!(modes.iter().all(|o| !o.selected));
    }

    #[test]
    fn no_mode_options_when_model_lacks_effort_support() {
        let models = [model("grok-build", "Grok Build")];
        let current = acp::ModelId::from("grok-build");
        let opts = build_session_config_options(&models, &current, &[], None);
        assert_eq!(opts.len(), 1);
        assert!(opts.iter().all(|o| o.category == "model"));
    }

    #[test]
    fn model_label_falls_back_to_id_when_name_empty() {
        let models = [model("grok-build", "")];
        let current = acp::ModelId::from("grok-build");
        let opts = build_session_config_options(&models, &current, &[], None);
        assert_eq!(opts[0].label, "grok-build");
    }

    #[test]
    fn session_config_option_serializes_camel_case() {
        let opt = SessionConfigOption {
            id: "grok-build".to_string(),
            category: "model".to_string(),
            label: "Grok Build".to_string(),
            description: None,
            selected: true,
        };
        let v = serde_json::to_value(&opt).expect("serialize");
        assert_eq!(v["id"], "grok-build");
        assert_eq!(v["category"], "model");
        assert_eq!(v["label"], "Grok Build");
        assert_eq!(v["selected"], true);
        assert!(v.get("description").is_none());
    }

    #[test]
    fn grok_session_detail_serializes_camel_case() {
        let detail = GrokSessionDetail::build(
            "sess-1".to_string(),
            "/Users/me/xai".to_string(),
            "grok-build".to_string(),
            None,
        );
        let v = serde_json::to_value(&detail).expect("serialize");
        assert_eq!(v["sessionId"], "sess-1");
        assert_eq!(v["kind"], "build");
        assert_eq!(v["cwd"], "/Users/me/xai");
        assert_eq!(v["currentModelId"], "grok-build");
        assert!(v.get("title").is_none());
    }

    #[test]
    fn acp_config_options_map_model_and_effort_selectors() {
        let models = [
            model("grok-build", "Grok Build"),
            model("grok-4.5", "Grok 4.5"),
        ];
        let efforts = [ReasoningEffortOption {
            id: "high".to_string(),
            value: ReasoningEffort::High,
            label: "High".to_string(),
            description: None,
            default: false,
        }];

        let options = build_acp_config_options(
            &models,
            &acp::ModelId::from("grok-4.5"),
            &efforts,
            Some(ReasoningEffort::High),
        );

        let expected = vec![
            acp::SessionConfigOption::select(
                CONFIG_ID_MODEL,
                "Model",
                "grok-4.5",
                vec![
                    acp::SessionConfigSelectOption::new("grok-build", "Grok Build"),
                    acp::SessionConfigSelectOption::new("grok-4.5", "Grok 4.5"),
                ],
            )
            .category(acp::SessionConfigOptionCategory::Model),
            acp::SessionConfigOption::select(
                CONFIG_ID_REASONING_EFFORT,
                "Reasoning Effort",
                "high",
                vec![acp::SessionConfigSelectOption::new("high", "High")],
            )
            .category(acp::SessionConfigOptionCategory::ThoughtLevel),
        ];
        assert_eq!(options, expected);
    }

    #[test]
    fn acp_config_options_effort_current_preserves_unlisted_value() {
        let models = [model("grok-4.5", "Grok 4.5")];
        let efforts = [ReasoningEffortOption {
            id: "high".to_string(),
            value: ReasoningEffort::High,
            label: "High".to_string(),
            description: None,
            default: false,
        }];
        let options = build_acp_config_options(
            &models,
            &acp::ModelId::from("grok-4.5"),
            &efforts,
            Some(ReasoningEffort::Low),
        );
        let effort = options
            .iter()
            .find(|o| o.id.0.as_ref() == CONFIG_ID_REASONING_EFFORT)
            .expect("effort selector present when the model supports effort");
        match &effort.kind {
            acp::SessionConfigKind::Select(select) => {
                assert_eq!(select.current_value.0.as_ref(), "low");
            }
            _ => panic!("effort must be a select"),
        }
    }

    #[test]
    fn acp_config_options_model_current_preserves_unlisted_value() {
        let models = [
            model("grok-build", "Grok Build"),
            model("grok-4.5", "Grok 4.5"),
        ];
        let options =
            build_acp_config_options(&models, &acp::ModelId::from("stale-model"), &[], None);
        let model = options
            .iter()
            .find(|o| o.id.0.as_ref() == CONFIG_ID_MODEL)
            .expect("model selector present");
        match &model.kind {
            acp::SessionConfigKind::Select(select) => {
                assert_eq!(select.current_value.0.as_ref(), "stale-model");
            }
            _ => panic!("model must be a select"),
        }
    }

    fn init_with_client_name(name: &str) -> acp::InitializeRequest {
        acp::InitializeRequest::new(acp::ProtocolVersion::V1)
            .client_info(acp::Implementation::new(name, "1.0.0"))
    }

    fn init_with_meta(meta: acp::Meta) -> acp::InitializeRequest {
        acp::InitializeRequest::new(acp::ProtocolVersion::V1).meta(Some(meta))
    }

    #[test]
    fn boolean_config_prefers_session_meta_over_zed_and_stored() {
        let mut session_meta = acp::Meta::new();
        session_meta.insert(
            BOOLEAN_CONFIG_OPTIONS_META_KEY.to_string(),
            serde_json::json!(false),
        );
        let init = init_with_client_name("zed");
        assert!(!client_supports_standard_boolean_config_options(
            Some(&session_meta),
            Some(true),
            Some(&init),
        ));
        session_meta.insert(
            BOOLEAN_CONFIG_OPTIONS_META_KEY.to_string(),
            serde_json::json!(true),
        );
        assert!(client_supports_standard_boolean_config_options(
            Some(&session_meta),
            Some(false),
            None,
        ));
    }

    #[test]
    fn boolean_config_uses_stored_session_verdict_when_meta_absent() {
        let init = init_with_client_name("zed");
        assert!(client_supports_standard_boolean_config_options(
            None,
            Some(true),
            Some(&init),
        ));
        assert!(!client_supports_standard_boolean_config_options(
            None,
            Some(false),
            Some(&init),
        ));
    }

    #[test]
    fn boolean_config_direct_stdio_supports_zed_client_info() {
        let zed = init_with_client_name("zed");
        assert!(client_supports_standard_boolean_config_options(
            None,
            None,
            Some(&zed)
        ));
        let other = init_with_client_name("grok-tui");
        assert!(!client_supports_standard_boolean_config_options(
            None,
            None,
            Some(&other),
        ));
    }

    #[test]
    fn boolean_config_accepts_initialize_meta_compatibility_key() {
        let mut meta = acp::Meta::new();
        meta.insert(
            BOOLEAN_CONFIG_OPTIONS_META_KEY.to_string(),
            serde_json::json!(true),
        );
        let init = init_with_meta(meta);
        assert!(client_supports_standard_boolean_config_options(
            None,
            None,
            Some(&init)
        ));
        let mut denied = acp::Meta::new();
        denied.insert(
            BOOLEAN_CONFIG_OPTIONS_META_KEY.to_string(),
            serde_json::json!(false),
        );
        let init = init_with_meta(denied);
        assert!(!client_supports_standard_boolean_config_options(
            None,
            None,
            Some(&init)
        ));
    }

    fn fast_model(id: &'static str, name: &str) -> acp::ModelInfo {
        model(id, name).meta(
            serde_json::json!({ "supportsFastMode": true })
                .as_object()
                .cloned(),
        )
    }

    fn acp_opts(
        models: &[acp::ModelInfo],
        current: &acp::ModelId,
        efforts: &[ReasoningEffortOption],
        current_effort: Option<ReasoningEffort>,
        fast: Option<FastModeOptionInput>,
    ) -> Vec<acp::SessionConfigOption> {
        with_fast_mode_option(
            build_acp_config_options(models, current, efforts, current_effort),
            fast,
        )
    }

    #[test]
    fn acp_fast_mode_emitted_only_when_supported_and_model_fast_capable() {
        let models = [fast_model("codex", "Codex")];
        let current = acp::ModelId::from("codex");
        let gated_off = acp_opts(
            &models,
            &current,
            &[],
            None,
            Some(FastModeOptionInput {
                current_value: true,
                boolean_supported: false,
                model_fast_capable: true,
            }),
        );
        assert!(
            gated_off
                .iter()
                .all(|option| option.id.0.as_ref() != CONFIG_ID_FAST_MODE)
        );
        let not_capable = acp_opts(
            &models,
            &current,
            &[],
            None,
            Some(FastModeOptionInput {
                current_value: false,
                boolean_supported: true,
                model_fast_capable: false,
            }),
        );
        assert!(
            not_capable
                .iter()
                .all(|option| option.id.0.as_ref() != CONFIG_ID_FAST_MODE)
        );
        let enabled = acp_opts(
            &models,
            &current,
            &[],
            None,
            Some(FastModeOptionInput {
                current_value: true,
                boolean_supported: true,
                model_fast_capable: true,
            }),
        );
        let fast = enabled
            .iter()
            .find(|option| option.id.0.as_ref() == CONFIG_ID_FAST_MODE)
            .expect("fast_mode");
        match &fast.kind {
            acp::SessionConfigKind::Boolean(flag) => assert!(flag.current_value),
            other => panic!("expected boolean kind, got {other:?}"),
        }
    }

    #[test]
    fn acp_session_config_option_serializes_to_protocol_shape() {
        let models = [model("grok-build", "Grok Build")];
        let current = acp::ModelId::from("grok-build");
        let opts = acp_opts(
            &models,
            &current,
            &legacy_session_effort_options(),
            Some(ReasoningEffort::High),
            Some(FastModeOptionInput {
                current_value: false,
                boolean_supported: true,
                model_fast_capable: true,
            }),
        );
        let json = serde_json::to_value(&opts).expect("serialize");
        assert_eq!(json[0]["id"], "model");
        assert_eq!(json[0]["name"], "Model");
        assert_eq!(json[0]["category"], "model");
        assert_eq!(json[0]["type"], "select");
        assert_eq!(json[0]["currentValue"], "grok-build");
        assert_eq!(json[0]["options"][0]["value"], "grok-build");
        assert_eq!(json[0]["options"][0]["name"], "Grok Build");
        assert_eq!(json[1]["id"], "reasoning_effort");
        assert_eq!(json[1]["category"], "thought_level");
        assert_eq!(json[1]["type"], "select");
        assert_eq!(json[1]["currentValue"], "high");
        assert_eq!(json[2]["id"], "fast_mode");
        assert_eq!(json[2]["type"], "boolean");
        assert_eq!(json[2]["currentValue"], false);
        assert_eq!(json[2]["category"], "model_config");
    }

    #[test]
    fn new_session_response_keeps_models_legacy_meta_and_top_level_config_options() {
        let models = [model("grok-build", "Grok Build")];
        let current = acp::ModelId::from("grok-build");
        let private = build_session_config_options(&models, &current, &[], None);
        let acp_opts = acp_opts(&models, &current, &[], None, None);
        let mut meta = acp::Meta::new();
        meta.insert(
            "x.ai/sessionConfig".to_string(),
            serde_json::json!({ "options": private }),
        );
        let response = acp::NewSessionResponse::new("sess-1")
            .models(Some(acp::SessionModelState::new(current, models.to_vec())))
            .config_options(Some(acp_opts))
            .meta(meta);
        let json = serde_json::to_value(&response).expect("serialize");
        assert_eq!(json["sessionId"], "sess-1");
        assert_eq!(json["models"]["currentModelId"], "grok-build");
        assert_eq!(json["configOptions"][0]["id"], "model");
        assert_eq!(json["configOptions"][0]["type"], "select");
        assert_eq!(
            json["_meta"]["x.ai/sessionConfig"]["options"][0]["id"],
            "grok-build"
        );
        assert_eq!(
            json["_meta"]["x.ai/sessionConfig"]["options"][0]["category"],
            "model"
        );
    }

    fn offered_for_validation() -> (Vec<acp::SessionConfigOption>, Vec<ReasoningEffortOption>) {
        let models = [
            model("grok-build", "Grok Build"),
            model("grok-4.5", "Grok 4.5"),
        ];
        let current = acp::ModelId::from("grok-build");
        let efforts = legacy_session_effort_options();
        let offered = acp_opts(
            &models,
            &current,
            &efforts,
            Some(ReasoningEffort::High),
            Some(FastModeOptionInput {
                current_value: false,
                boolean_supported: true,
                model_fast_capable: true,
            }),
        );
        (offered, efforts)
    }

    #[test]
    fn set_config_option_accepts_model_effort_and_fast_mode() {
        let (offered, efforts) = offered_for_validation();
        let model_req =
            acp::SetSessionConfigOptionRequest::new("sess", CONFIG_ID_MODEL, "grok-4.5");
        assert_eq!(
            parse_set_session_config_option(&model_req, &offered, &efforts).unwrap(),
            ParsedSessionConfigSet::Model(acp::ModelId::new("grok-4.5"))
        );
        let effort_req = acp::SetSessionConfigOptionRequest::new(
            "sess",
            CONFIG_ID_REASONING_EFFORT,
            "low",
        );
        assert_eq!(
            parse_set_session_config_option(&effort_req, &offered, &efforts).unwrap(),
            ParsedSessionConfigSet::ReasoningEffort(ReasoningEffort::Low)
        );
        let fast_req =
            acp::SetSessionConfigOptionRequest::new("sess", CONFIG_ID_FAST_MODE, true);
        assert_eq!(
            parse_set_session_config_option(&fast_req, &offered, &efforts).unwrap(),
            ParsedSessionConfigSet::FastMode(true)
        );
    }

    #[test]
    fn set_config_option_rejects_unknown_id_value_and_type() {
        let (offered, efforts) = offered_for_validation();
        let unknown = acp::SetSessionConfigOptionRequest::new("sess", "not_a_real_id", "x");
        assert_eq!(
            parse_set_session_config_option(&unknown, &offered, &efforts)
                .unwrap_err()
                .code,
            acp::ErrorCode::InvalidParams
        );
        let bad_model =
            acp::SetSessionConfigOptionRequest::new("sess", CONFIG_ID_MODEL, "not-a-model");
        assert_eq!(
            parse_set_session_config_option(&bad_model, &offered, &efforts)
                .unwrap_err()
                .code,
            acp::ErrorCode::InvalidParams
        );
        let type_mismatch =
            acp::SetSessionConfigOptionRequest::new("sess", CONFIG_ID_MODEL, true);
        assert_eq!(
            parse_set_session_config_option(&type_mismatch, &offered, &efforts)
                .unwrap_err()
                .code,
            acp::ErrorCode::InvalidParams
        );
        let bool_as_id =
            acp::SetSessionConfigOptionRequest::new("sess", CONFIG_ID_FAST_MODE, "yes");
        assert_eq!(
            parse_set_session_config_option(&bool_as_id, &offered, &efforts)
                .unwrap_err()
                .code,
            acp::ErrorCode::InvalidParams
        );
    }
}
