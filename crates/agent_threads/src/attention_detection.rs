//! Best-effort classification of what's currently happening in a live agent
//! thread's terminal, ported from herdr's manifest-based rule engine
//! (https://github.com/herdrdev/herdr, `src/detect/manifest.rs` +
//! `src/detect/manifests/*.toml`). Herdr drives this from a ~300ms
//! poll-and-diff loop over every pane (`pane.rs`'s `detection_content_seq`),
//! re-scanning only when new PTY output actually arrived since the last
//! scan. `store.rs` mirrors that trigger shape using `terminal::Event`:
//! `Wakeup` (fires whenever new output arrives, already debounced a few ms
//! by `Terminal::subscribe`) is further debounced there and reclassifies on
//! settling, while `Bell` reclassifies immediately, since Claude/Codex/
//! OpenCode use it as a deliberate "look at me" signal.
//!
//! A miss here (`AttentionState::Unknown`, or no bundled manifest for the
//! agent kind at all) is expected: not every screen matches a known
//! pattern, and Pi in particular has no manifest richer than a `Working`
//! rule and one blocked prompt (see `attention_manifests/pi.toml`) --
//! herdr's own manifest for it is just as thin. How a miss is handled is
//! `store.rs`'s call, not this module's: it differs by which
//! `terminal::Event` triggered the classification (see its
//! `reclassify_attention`).

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

/// How many trailing non-empty screen lines to classify against. Bounds the
/// snapshot `store.rs` takes (`Terminal::last_n_non_empty_lines`) --
/// generous enough for every region below except `top_non_empty_lines`,
/// which needs the true top of the scrollback and isn't supported (see the
/// manifest files' doc comments).
pub(crate) const SCREEN_TAIL_LINE_COUNT: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttentionState {
    /// Agent is actively working; PTY liveness alone can't tell this apart
    /// from "idle at a prompt with no more output coming," which is why
    /// this is a real classifiable state here (unlike Flint's original,
    /// bell-only design, where "working" only ever meant "the bell hasn't
    /// rung yet").
    Working,
    /// Agent finished, prompt visible, nothing pending.
    Idle,
    /// No rule in the agent's manifest matched, or the agent has no
    /// bundled manifest at all.
    Unknown,
    /// Agent is blocked on a permission/question prompt.
    Blocked,
}

pub(crate) struct DetectionInput<'a> {
    pub(crate) screen_tail: &'a str,
    pub(crate) osc_title: &'a str,
}

/// Classifies a single bell-time snapshot for `kind_id` (an
/// `AgentKindDefinition::id`, e.g. `"claude"`). Rules are evaluated in
/// manifest order; among matches, the highest `priority` wins, with the
/// first-declared rule winning ties -- both matching herdr's own
/// resolution (`evaluate_loaded_manifest` in its `manifest.rs`).
pub(crate) fn classify(kind_id: &str, input: DetectionInput<'_>) -> AttentionState {
    let Some(manifest) = manifests().get(kind_id) else {
        return AttentionState::Unknown;
    };

    let mut matched: Option<&CompiledRule> = None;
    for rule in &manifest.rules {
        let region_text = resolve_region(&rule.region, input.screen_tail, input.osc_title);
        if !gate_matches(&rule.gate, &region_text) {
            continue;
        }
        match matched {
            Some(previous) if previous.priority >= rule.priority => {}
            _ => matched = Some(rule),
        }
    }

    matched.map_or(AttentionState::Unknown, |rule| rule.state)
}

const BUNDLED_MANIFESTS: &[(&str, &str)] = &[
    ("claude", include_str!("attention_manifests/claude.toml")),
    ("codex", include_str!("attention_manifests/codex.toml")),
    (
        "opencode",
        include_str!("attention_manifests/opencode.toml"),
    ),
    ("pi", include_str!("attention_manifests/pi.toml")),
];

fn manifests() -> &'static HashMap<&'static str, CompiledManifest> {
    static MANIFESTS: LazyLock<HashMap<&'static str, CompiledManifest>> = LazyLock::new(|| {
        let mut manifests = HashMap::default();
        for (kind_id, source) in BUNDLED_MANIFESTS {
            match compile_manifest(source) {
                Ok(manifest) => {
                    manifests.insert(*kind_id, manifest);
                }
                Err(error) => {
                    log::error!(
                        "agent_threads: failed to compile attention manifest for {kind_id}: {error}"
                    );
                }
            }
        }
        manifests
    });
    &MANIFESTS
}

struct CompiledManifest {
    rules: Vec<CompiledRule>,
}

struct CompiledRule {
    state: AttentionState,
    priority: i32,
    region: String,
    gate: CompiledGate,
}

#[derive(Default)]
struct CompiledGate {
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not_gate: Vec<CompiledGate>,
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default)]
    rules: Vec<RawRule>,
}

// Doesn't use `#[serde(flatten)]` for the gate fields below, matching
// herdr's own `ManifestRule`/`ManifestGate` split -- a rule carries its own
// top-level `contains`/`regex`/etc. fields directly (see the manifest TOML
// files) rather than nesting them under a `gate` key, so this struct
// duplicates `RawGate`'s fields instead of embedding it.
#[derive(Debug, Deserialize)]
struct RawRule {
    #[allow(dead_code)]
    id: String,
    state: RawState,
    #[serde(default)]
    priority: i32,
    #[serde(default = "default_region")]
    region: String,
    #[serde(default)]
    all: Vec<RawGate>,
    #[serde(default)]
    any: Vec<RawGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<RawGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawGate {
    #[serde(default)]
    all: Vec<RawGate>,
    #[serde(default)]
    any: Vec<RawGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<RawGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RawState {
    Working,
    Idle,
    Blocked,
}

impl From<RawState> for AttentionState {
    fn from(value: RawState) -> Self {
        match value {
            RawState::Working => AttentionState::Working,
            RawState::Idle => AttentionState::Idle,
            RawState::Blocked => AttentionState::Blocked,
        }
    }
}

fn default_region() -> String {
    "whole_recent".to_string()
}

fn compile_manifest(source: &str) -> Result<CompiledManifest, String> {
    let raw: RawManifest = toml::from_str(source).map_err(|error| error.to_string())?;
    let rules = raw
        .rules
        .into_iter()
        .map(|rule| {
            let rule_id = rule.id.clone();
            let gate = RawGate {
                all: rule.all,
                any: rule.any,
                not_gate: rule.not_gate,
                contains: rule.contains,
                regex: rule.regex,
                line_regex: rule.line_regex,
            };
            Ok(CompiledRule {
                state: rule.state.into(),
                priority: rule.priority,
                region: rule.region,
                gate: compile_gate(gate)
                    .map_err(|error| format!("rule {rule_id} could not be compiled: {error}"))?,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(CompiledManifest { rules })
}

fn compile_gate(gate: RawGate) -> Result<CompiledGate, String> {
    Ok(CompiledGate {
        all: gate
            .all
            .into_iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        any: gate
            .any
            .into_iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        not_gate: gate
            .not_gate
            .into_iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        contains: gate
            .contains
            .into_iter()
            .map(|needle| needle.to_lowercase())
            .collect(),
        regex: gate
            .regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|error| error.to_string()))
            .collect::<Result<_, _>>()?,
        line_regex: gate
            .line_regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|error| error.to_string()))
            .collect::<Result<_, _>>()?,
    })
}

/// Mirrors herdr's `compiled_gate_matches`: every top-level matcher on this
/// gate is an implicit AND, `any` (if present) needs at least one nested
/// gate to match, and matching any `not` gate vetoes the whole gate.
fn gate_matches(gate: &CompiledGate, text: &str) -> bool {
    let lower_text = text.to_lowercase();

    if !gate
        .contains
        .iter()
        .all(|needle| lower_text.contains(needle.as_str()))
    {
        return false;
    }
    if !gate.regex.iter().all(|regex| regex.is_match(text)) {
        return false;
    }
    if !gate
        .line_regex
        .iter()
        .all(|regex| text.lines().any(|line| regex.is_match(line)))
    {
        return false;
    }
    if !gate.all.iter().all(|nested| gate_matches(nested, text)) {
        return false;
    }
    if !gate.any.is_empty() && !gate.any.iter().any(|nested| gate_matches(nested, text)) {
        return false;
    }
    if gate
        .not_gate
        .iter()
        .any(|nested| gate_matches(nested, text))
    {
        return false;
    }
    true
}

fn resolve_region<'a>(region: &str, screen_tail: &'a str, osc_title: &'a str) -> String {
    match region {
        "osc_title" => osc_title.to_string(),
        "whole_recent" => screen_tail.to_string(),
        "after_last_horizontal_rule" => after_last_horizontal_rule(screen_tail).to_string(),
        "after_last_prompt_marker" => after_last_prompt_marker(screen_tail).to_string(),
        "prompt_box_body" => prompt_box_body(screen_tail).unwrap_or("").to_string(),
        _ => {
            if let Some(count) = region_count(region, "bottom_non_empty_lines") {
                bottom_non_empty_lines(screen_tail, count).to_string()
            } else {
                // Unrecognized region (e.g. `osc_progress`, which Flint's
                // terminal doesn't parse, or `top_non_empty_lines`, which
                // needs scrollback this classifier doesn't have -- see the
                // manifest files' doc comments) resolves to nothing, so any
                // rule keyed on it simply never matches, same as herdr's own
                // catch-all for an unknown region spec.
                String::new()
            }
        }
    }
}

fn region_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|count| count.parse::<usize>().ok())
}

fn bottom_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(start_index) = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    slice_from_line_index(content, &lines, start_index)
}

fn after_last_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = lines.iter().rposition(|line| codex_prompt_line(line)) else {
        return content;
    };
    slice_from_line_index(content, &lines, index + 1)
}

fn codex_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn prompt_box_body(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let top = prompt_box_top_border_index(&lines)?;
    let start = line_start_offset(content, &lines, top + 1);
    let end_index = lines[top + 1..]
        .iter()
        .position(|line| is_horizontal_rule(line))
        .map(|relative| top + 1 + relative)
        .unwrap_or(lines.len());
    let end = line_start_offset(content, &lines, end_index);
    Some(&content[start.min(content.len())..end.min(content.len())])
}

fn prompt_box_top_border_index(lines: &[&str]) -> Option<usize> {
    let mut border_count = 0;
    for index in (0..lines.len()).rev() {
        if is_horizontal_rule(lines[index]) {
            border_count += 1;
            if border_count == 2 {
                return Some(index);
            }
        }
    }
    None
}

fn after_last_horizontal_rule(content: &str) -> &str {
    let mut last_rule_end = 0usize;
    let mut offset = 0usize;
    for line in content.lines() {
        let next_offset = offset + line.len() + 1;
        if is_horizontal_rule(line) {
            last_rule_end = next_offset.min(content.len());
        }
        offset = next_offset;
    }
    &content[last_rule_end..]
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let rule_chars = trimmed.chars().take_while(|&ch| ch == '─').count();
    if rule_chars == 0 {
        return false;
    }
    let rule_bytes = trimmed
        .char_indices()
        .nth(rule_chars)
        .map(|(index, _)| index)
        .unwrap_or(trimmed.len());
    let suffix = trimmed[rule_bytes..].trim_start();
    suffix.is_empty() || rule_chars >= 3
}

fn slice_from_line_index<'a>(content: &'a str, lines: &[&str], index: usize) -> &'a str {
    let byte_offset = line_start_offset(content, lines, index);
    &content[byte_offset.min(content.len())..]
}

fn line_start_offset(content: &str, lines: &[&str], index: usize) -> usize {
    lines[..index.min(lines.len())]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>()
        .min(content.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_screen(kind_id: &str, screen_tail: &str) -> AttentionState {
        classify(
            kind_id,
            DetectionInput {
                screen_tail,
                osc_title: "",
            },
        )
    }

    #[test]
    fn unknown_kind_is_unknown() {
        assert_eq!(
            classify_screen("not-a-real-agent", "anything at all"),
            AttentionState::Unknown
        );
    }

    #[test]
    fn plain_output_with_no_pattern_is_unknown() {
        assert_eq!(
            classify_screen("claude", "Building the widget module...\nDone."),
            AttentionState::Unknown
        );
        assert_eq!(
            classify_screen("codex", "Compiling crate foo v0.1.0"),
            AttentionState::Unknown
        );
    }

    #[test]
    fn claude_permission_prompt_is_blocked() {
        let screen =
            "Do you want to proceed?\n❯ 1. Yes\n  2. No, and tell Claude what to do differently";
        assert_eq!(classify_screen("claude", screen), AttentionState::Blocked);
    }

    #[test]
    fn claude_dynamic_workflow_prompt_is_blocked() {
        let screen = "Run a dynamic workflow?\nesc to cancel";
        assert_eq!(classify_screen("claude", screen), AttentionState::Blocked);
    }

    #[test]
    fn claude_legacy_yes_no_prompt_is_blocked() {
        let screen = "Do you want to make this change?\n❯ Yes";
        assert_eq!(classify_screen("claude", screen), AttentionState::Blocked);
    }

    #[test]
    fn claude_bare_prompt_marker_is_not_blocked_by_the_legacy_fallback() {
        // `legacy_no_prompt_blocker`'s `not` clause exists specifically to
        // keep a bare `❯` idle prompt (no accompanying "do you want to"/
        // "would you like to" text) from being misread as a yes/no blocker.
        assert_eq!(classify_screen("claude", "❯"), AttentionState::Unknown);
    }

    #[test]
    fn codex_weak_blocker_phrase_is_blocked() {
        assert_eq!(
            classify_screen("codex", "Do you want to continue? [y/n]"),
            AttentionState::Blocked
        );
    }

    #[test]
    fn codex_strong_blocker_after_prompt_marker_is_blocked() {
        let screen = "› some earlier codex output\nAllow command?";
        assert_eq!(classify_screen("codex", screen), AttentionState::Blocked);
    }

    #[test]
    fn codex_osc_title_action_required_is_blocked_even_with_plain_screen() {
        let state = classify(
            "codex",
            DetectionInput {
                screen_tail: "Compiling crate foo v0.1.0",
                osc_title: "Action Required",
            },
        );
        assert_eq!(state, AttentionState::Blocked);
    }

    #[test]
    fn codex_osc_title_present_and_not_blocked_is_idle() {
        let state = classify(
            "codex",
            DetectionInput {
                screen_tail: "",
                osc_title: "codex: my-project",
            },
        );
        assert_eq!(state, AttentionState::Idle);
    }

    #[test]
    fn opencode_permission_required_is_blocked() {
        assert_eq!(
            classify_screen("opencode", "△ Permission required to run this tool"),
            AttentionState::Blocked
        );
    }

    #[test]
    fn opencode_has_no_idle_rule_so_plain_output_is_unknown() {
        assert_eq!(
            classify_screen("opencode", "Ready for your next request."),
            AttentionState::Unknown
        );
    }

    #[test]
    fn pi_unmatched_output_is_unknown() {
        assert_eq!(
            classify_screen("pi", "some ordinary output that matches no Pi rule"),
            AttentionState::Unknown
        );
    }

    #[test]
    fn pi_working_literal_is_working() {
        assert_eq!(classify_screen("pi", "Working..."), AttentionState::Working);
    }

    #[test]
    fn pi_project_trust_prompt_is_blocked() {
        let screen =
            "Project trust\n/tmp/some-project\n\nSaved decision: none\nCurrent session: untrusted";
        assert_eq!(classify_screen("pi", screen), AttentionState::Blocked);
    }

    #[test]
    fn pi_project_trust_requires_both_signals() {
        // Guards against "Project trust" alone (e.g. mentioned in passing
        // help text) being mistaken for the actual prompt.
        assert_eq!(
            classify_screen("pi", "Project trust is configured per directory."),
            AttentionState::Unknown
        );
    }

    #[test]
    fn claude_busy_spinner_osc_title_is_working() {
        let state = classify(
            "claude",
            DetectionInput {
                screen_tail: "",
                osc_title: "⠋ Thinking",
            },
        );
        assert_eq!(state, AttentionState::Working);
    }

    #[test]
    fn codex_screen_working_fallback_is_working() {
        let screen = "• Working (12s · esc to interrupt)";
        assert_eq!(classify_screen("codex", screen), AttentionState::Working);
    }

    #[test]
    fn codex_osc_title_busy_spinner_beats_idle_fallback() {
        // Regression guard for restoring codex's osc_title_idle `not` clause
        // to also exclude the spinner (dropped when Working wasn't ported
        // yet, restored alongside osc_title_working): a busy OSC title must
        // not be misread as idle just because it's non-empty and isn't
        // "Action Required".
        let state = classify(
            "codex",
            DetectionInput {
                screen_tail: "",
                osc_title: "⠙ codex",
            },
        );
        assert_eq!(state, AttentionState::Working);
    }

    #[test]
    fn opencode_interrupt_hint_is_working() {
        assert_eq!(
            classify_screen("opencode", "Press esc to interrupt"),
            AttentionState::Working
        );
    }

    #[test]
    fn higher_priority_rule_wins_over_a_lower_priority_match() {
        // Claude's `live_blocked_form` (priority 980, blocked) and
        // `legacy_no_prompt_blocker` (priority 300, blocked) can both match
        // the same screen; this only proves priority resolution picks a
        // winner rather than e.g. rule-declaration order by accident, so
        // reuse a screen where the higher-priority rule is the sole match.
        let screen = "───────\nesc to cancel\nenter to confirm";
        assert_eq!(classify_screen("claude", screen), AttentionState::Blocked);
    }

    #[test]
    fn after_last_horizontal_rule_returns_only_the_trailing_section() {
        let content = "above the rule\n───\nbelow the rule";
        assert_eq!(after_last_horizontal_rule(content), "below the rule");
    }

    #[test]
    fn after_last_prompt_marker_returns_only_the_trailing_section() {
        let content = "› previous turn\nsome output\n› latest turn\nnewest output";
        assert_eq!(after_last_prompt_marker(content), "newest output");
    }

    #[test]
    fn prompt_box_body_extracts_text_between_the_last_two_horizontal_rules() {
        let content = "───\n❯ type here\n───";
        // Includes the trailing newline before the closing rule -- the body
        // is "everything from just after the top rule's line up to just
        // before the bottom rule's line," not a trimmed line list.
        assert_eq!(prompt_box_body(content), Some("❯ type here\n"));
    }

    #[test]
    fn bottom_non_empty_lines_skips_blank_lines_and_caps_the_count() {
        let content = "line one\n\nline two\nline three";
        assert_eq!(bottom_non_empty_lines(content, 2), "line two\nline three");
    }
}
