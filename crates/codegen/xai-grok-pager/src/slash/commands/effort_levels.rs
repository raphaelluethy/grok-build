//! Shared reasoning-effort dropdown levels for `/model` and `/effort`.

use xai_grok_shell::sampling::types::{ReasoningEffort, ReasoningEffortOption};

use crate::slash::command::ArgItem;

/// Effort levels in the built-in fallback menu (strongest first). `none`/`minimal`
/// are still accepted by `ReasoningEffort::from_str` for power users.
pub(crate) const EFFORT_LEVELS: &[ReasoningEffort] = &[
    ReasoningEffort::Xhigh,
    ReasoningEffort::High,
    ReasoningEffort::Medium,
    ReasoningEffort::Low,
];

pub(crate) fn effort_description(level: ReasoningEffort) -> &'static str {
    match level {
        ReasoningEffort::None => "No reasoning",
        ReasoningEffort::Minimal => "Minimal reasoning",
        ReasoningEffort::Low => "Faster, lighter reasoning",
        ReasoningEffort::Medium => "Balanced reasoning",
        ReasoningEffort::High => "Heavy reasoning",
        ReasoningEffort::Xhigh => "Extended reasoning",
        ReasoningEffort::Max => "Maximum reasoning",
    }
}

/// The built-in menu used when the server sends no `reasoningEfforts`. Reproduces
/// the historical rows: labels are the lowercase level (via `Display`),
/// descriptions from `effort_description`. The active row is matched by value
/// against the session effort at render time, so `default` is left unset here.
pub(crate) fn legacy_effort_options() -> Vec<ReasoningEffortOption> {
    EFFORT_LEVELS
        .iter()
        .map(|&level| ReasoningEffortOption {
            id: level.as_str().to_string(),
            value: level,
            label: level.to_string(),
            description: Some(effort_description(level).to_string()),
            default: false,
        })
        .collect()
}

/// Build effort rows for autocomplete from a per-model option list.
///
/// - `mark_active` + `current_effort` mark the current session effort with `(active)`.
/// - `insert_text_for` controls what is inserted on select:
///   - `/effort`: the option id (`"deep"`)
///   - `/model` chained phase: `"ModelName deep"`
///
/// `match_text` gets an `a `/`b `/…` sort prefix so the matcher's alphabetical
/// tiebreak preserves the option order.
pub(crate) fn build_effort_arg_items(
    options: &[ReasoningEffortOption],
    current_effort: Option<ReasoningEffort>,
    mark_active: bool,
    insert_text_for: impl Fn(&ReasoningEffortOption) -> String,
) -> Vec<ArgItem> {
    options
        .iter()
        .enumerate()
        .map(|(idx, option)| {
            let active = mark_active && current_effort == Some(option.value);
            let active_suffix = if active { " (active)" } else { "" };
            let insert_text = insert_text_for(option);
            // Sort-key prefix: 'a' for top row, 'b' for next, etc. Only
            // affects matcher tiebreak ordering, never rendered.
            let sort_prefix = char::from(b'a' + idx as u8);
            ArgItem {
                display: format!("{}{active_suffix}", option.label),
                match_text: format!("{sort_prefix} {insert_text}"),
                insert_text,
                description: option.description.clone().unwrap_or_default(),
            }
        })
        .collect()
}

/// Canonical strength order: `none < minimal < low < medium < high < xhigh < max`.
/// Used for Alt+, / Alt+. stepping so advertised menu *order* is irrelevant.
fn effort_strength(level: ReasoningEffort) -> u8 {
    match level {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::Xhigh => 5,
        ReasoningEffort::Max => 6,
    }
}

/// Pick the next advertised effort option by **semantic strength**, not list order.
///
/// Raise selects the nearest strictly stronger offered value; lower selects the
/// nearest strictly weaker. Clamps at the strongest / weakest offered values
/// (no wrap). Returns the original [`ReasoningEffortOption`] so remapped ids
/// (`deep` → `xhigh`) stay on the `/effort <id>` path.
///
/// When `current` is `None`, an option marked `default` is used as the
/// effective current. If none is default either, raise selects the strongest
/// offered value and lower selects the weakest (a deterministic boundary:
/// the user asked to go up or down with no known starting point).
pub(crate) fn step_effort_option(
    options: &[ReasoningEffortOption],
    current: Option<ReasoningEffort>,
    raise: bool,
) -> Option<&ReasoningEffortOption> {
    if options.is_empty() {
        return None;
    }

    let effective = current.or_else(|| options.iter().find(|opt| opt.default).map(|opt| opt.value));

    let Some(from) = effective else {
        return if raise {
            options.iter().max_by_key(|opt| effort_strength(opt.value))
        } else {
            options.iter().min_by_key(|opt| effort_strength(opt.value))
        };
    };

    let from_rank = effort_strength(from);
    if raise {
        options
            .iter()
            .filter(|opt| effort_strength(opt.value) > from_rank)
            .min_by_key(|opt| effort_strength(opt.value))
            .or_else(|| option_at_strength(options, from_rank))
            .or_else(|| options.iter().max_by_key(|opt| effort_strength(opt.value)))
    } else {
        options
            .iter()
            .filter(|opt| effort_strength(opt.value) < from_rank)
            .max_by_key(|opt| effort_strength(opt.value))
            .or_else(|| option_at_strength(options, from_rank))
            .or_else(|| options.iter().min_by_key(|opt| effort_strength(opt.value)))
    }
}

fn option_at_strength(
    options: &[ReasoningEffortOption],
    rank: u8,
) -> Option<&ReasoningEffortOption> {
    options
        .iter()
        .find(|opt| effort_strength(opt.value) == rank)
}

#[cfg(test)]
mod step_tests {
    use super::*;

    fn opt(id: &str, value: ReasoningEffort, default: bool) -> ReasoningEffortOption {
        ReasoningEffortOption {
            id: id.to_string(),
            value,
            label: id.to_string(),
            description: None,
            default,
        }
    }

    fn opts(ids: &[&str]) -> Vec<ReasoningEffortOption> {
        ids.iter()
            .map(|id| {
                let value: ReasoningEffort = id.parse().expect("canonical effort id");
                opt(id, value, false)
            })
            .collect()
    }

    fn id_of(
        options: &[ReasoningEffortOption],
        current: Option<ReasoningEffort>,
        raise: bool,
    ) -> Option<&str> {
        step_effort_option(options, current, raise).map(|o| o.id.as_str())
    }

    #[test]
    fn empty_returns_none() {
        assert!(step_effort_option(&[], Some(ReasoningEffort::High), true).is_none());
        assert!(step_effort_option(&[], None, false).is_none());
    }

    #[test]
    fn strongest_first_raise_and_lower() {
        let options = opts(&["xhigh", "high", "medium", "low"]);
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Medium), true),
            Some("high")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::High), true),
            Some("xhigh")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Xhigh), true),
            Some("xhigh")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::High), false),
            Some("medium")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Low), false),
            Some("low")
        );
    }

    #[test]
    fn weakest_first_raise_moves_to_stronger_not_list_neighbor() {
        let options = opts(&["low", "medium", "high", "xhigh"]);
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Medium), true),
            Some("high"),
            "raise must ignore weakest-first order"
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::High), false),
            Some("medium")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Low), true),
            Some("medium")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Xhigh), false),
            Some("high")
        );
    }

    #[test]
    fn none_high_menu_steps_semantically() {
        let options = opts(&["none", "high"]);
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::None), true),
            Some("high")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::High), false),
            Some("none")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::High), true),
            Some("high"),
            "clamp at strongest offered"
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::None), false),
            Some("none"),
            "clamp at weakest offered"
        );
        // Current not on the menu: nearest stronger / weaker among offered.
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Medium), true),
            Some("high")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Medium), false),
            Some("none")
        );
    }

    #[test]
    fn remapped_ids_are_preserved() {
        let options = vec![
            opt("deep", ReasoningEffort::Xhigh, false),
            opt("high", ReasoningEffort::High, false),
        ];
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Xhigh), false),
            Some("high")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::High), true),
            Some("deep")
        );
    }

    #[test]
    fn remapped_ids_with_weakest_first_order() {
        let options = vec![
            opt("high", ReasoningEffort::High, false),
            opt("deep", ReasoningEffort::Xhigh, false),
        ];
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::High), true),
            Some("deep")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Xhigh), false),
            Some("high")
        );
    }

    #[test]
    fn absent_current_uses_default_as_effective() {
        let options = vec![
            opt("low", ReasoningEffort::Low, false),
            opt("medium", ReasoningEffort::Medium, true),
            opt("high", ReasoningEffort::High, false),
        ];
        assert_eq!(id_of(&options, None, true), Some("high"));
        assert_eq!(id_of(&options, None, false), Some("low"));
    }

    #[test]
    fn current_overrides_default_flag() {
        let options = vec![
            opt("low", ReasoningEffort::Low, true),
            opt("medium", ReasoningEffort::Medium, false),
            opt("high", ReasoningEffort::High, false),
        ];
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Medium), true),
            Some("high")
        );
        assert_eq!(
            id_of(&options, Some(ReasoningEffort::Medium), false),
            Some("low")
        );
    }

    #[test]
    fn absent_current_without_default_raise_strongest_lower_weakest() {
        // Documented boundary: no session effort and no default flag → jump
        // to the requested end of the strength scale, independent of list order.
        let weakest_first = opts(&["low", "medium", "high", "xhigh"]);
        assert_eq!(id_of(&weakest_first, None, true), Some("xhigh"));
        assert_eq!(id_of(&weakest_first, None, false), Some("low"));
        let strongest_first = opts(&["xhigh", "high", "medium", "low"]);
        assert_eq!(id_of(&strongest_first, None, true), Some("xhigh"));
        assert_eq!(id_of(&strongest_first, None, false), Some("low"));
    }
}
