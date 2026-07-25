//! Cross-agent handoff transcript extraction.
//!
//! Each provider classifies its private on-disk session records into a shared
//! [`RawEvent`] stream; one kind-agnostic pass in this module pairs tool calls
//! with their results, bounds the result to a budget of logical turns, and
//! renders provider-neutral Markdown. See
//! `docs/superpowers/specs/2026-07-25-cross-agent-handoff-design.md`.

use collections::HashMap;
use serde_json::Value;

/// Size limits for a rendered excerpt. Selection budgets over normalized turns,
/// not raw records, so provider-specific duplication and noise (Codex
/// `reasoning`/`token_count`, Claude metadata records) cannot exhaust it.
#[derive(Clone, Copy, Debug)]
pub struct ExcerptBudget {
    /// Total rendered document cap, including the head turn and all metadata.
    pub max_total_bytes: usize,
    /// Maximum number of logical turns (excludes coalesced noise).
    pub max_turns: usize,
    /// Per tool call/result rendered size; the output tail is kept so an exit
    /// code or trailing error survives.
    pub max_tool_bytes: usize,
    /// The first user turn is always kept, truncated to this size.
    pub max_head_bytes: usize,
}

pub const DEFAULT_BUDGET: ExcerptBudget = ExcerptBudget {
    max_total_bytes: 28 * 1024,
    max_turns: 60,
    max_tool_bytes: 400,
    max_head_bytes: 4 * 1024,
};

/// A normalized transcript record produced by a provider classifier. Providers
/// map their private record shapes onto this; everything downstream is
/// kind-agnostic.
#[derive(Clone, Debug, PartialEq)]
pub enum RawEvent {
    /// A user-authored turn (already stripped of synthetic/command artifacts).
    User(String),
    /// An assistant text turn (thinking/reasoning excluded by the classifier).
    Assistant(String),
    /// An assistant tool invocation. `detail` is a compact command/path/args
    /// summary, not the full arguments.
    ToolCall {
        call_id: Option<String>,
        name: String,
        detail: String,
    },
    /// A tool result, paired to its call by `call_id` when present.
    ToolResult {
        call_id: Option<String>,
        output: String,
        is_error: bool,
    },
    /// A semantic checkpoint (compaction, branch summary, Claude
    /// `file-history-*`) preserved as a concise marker.
    Checkpoint(String),
    /// A recognized but non-conversational record (Codex `token_count`,
    /// `reasoning`; Pi `model_change`). Coalesced; consumes no turn budget.
    Noise,
}

/// The output of a provider classifier: the normalized stream plus diagnostics
/// that decide whether the excerpt is trustworthy.
#[derive(Clone, Debug, Default)]
pub struct Classified {
    pub events: Vec<RawEvent>,
    /// Lines that failed to parse as JSON, or records missing required fields.
    pub malformed_count: usize,
    /// Records whose type/shape the classifier does not recognize at all. A
    /// non-zero count marks the excerpt degraded, since a drifted format may be
    /// silently dropping real content.
    pub unknown_count: usize,
}

/// A rendered, bounded excerpt ready to embed in a handoff document.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptExcerpt {
    pub markdown: String,
    /// True when unknown records were seen (possible undetected content loss).
    pub degraded: bool,
    pub malformed_count: usize,
    pub unknown_count: usize,
    /// The source file may have been mid-write (live source); the last turn(s)
    /// might be truncated.
    pub possibly_incomplete: bool,
    pub included_turns: usize,
    pub omitted_turns: usize,
}

/// The reason extraction produced no usable excerpt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractionRefusal {
    /// No trustworthy user or assistant turn survived classification, so any
    /// document would be empty or misleading. The caller must not write a
    /// handoff and must tell the user.
    NoUsableContent,
}

/// A logical turn after tool call/result pairing.
enum Turn {
    User(String),
    Assistant(String),
    Checkpoint(String),
    Tool {
        name: String,
        detail: String,
        output: Option<String>,
        is_error: bool,
        resolved: bool,
    },
}

impl Turn {
    fn is_conversation(&self) -> bool {
        matches!(self, Turn::User(text) | Turn::Assistant(text) if !text.trim().is_empty())
    }
}

/// Pairs, bounds, and renders a classified stream into a provider-neutral
/// excerpt, or refuses when nothing trustworthy remains.
pub fn select_and_render(
    classified: Classified,
    budget: &ExcerptBudget,
    possibly_incomplete: bool,
) -> Result<TranscriptExcerpt, ExtractionRefusal> {
    let Classified {
        events,
        malformed_count,
        unknown_count,
    } = classified;

    let (turns, omitted_noise) = pair_turns(events);

    if !turns.iter().any(Turn::is_conversation) {
        return Err(ExtractionRefusal::NoUsableContent);
    }

    let rendered: Vec<String> = turns.iter().map(|turn| render_turn(turn, budget)).collect();

    // The first user turn is the task framing and is always kept.
    let head_index = turns
        .iter()
        .position(|turn| matches!(turn, Turn::User(text) if !text.trim().is_empty()));

    let mut selected = vec![false; turns.len()];
    let mut total_bytes = 0usize;
    let mut turn_count = 0usize;

    if let Some(head_index) = head_index {
        let head = truncate_head(&rendered[head_index], budget.max_head_bytes).0;
        total_bytes += head.len();
        turn_count += 1;
        selected[head_index] = true;
    }

    // Fill the remainder newest-first so the most recent turns -- including any
    // trailing unresolved/failed tool call -- are always present, then restore
    // chronological order for rendering.
    let tail_start = head_index.map(|index| index + 1).unwrap_or(0);
    for index in (tail_start..turns.len()).rev() {
        if turn_count >= budget.max_turns {
            break;
        }
        let cost = rendered[index].len() + 1;
        if total_bytes + cost > budget.max_total_bytes {
            break;
        }
        total_bytes += cost;
        turn_count += 1;
        selected[index] = true;
    }

    let included_turns = turn_count;
    let omitted_turns = selected.iter().filter(|included| !**included).count();

    let markdown = render_document(
        &turns,
        &rendered,
        &selected,
        head_index,
        budget,
        omitted_turns,
        omitted_noise,
        unknown_count,
        malformed_count,
        possibly_incomplete,
    );

    Ok(TranscriptExcerpt {
        markdown,
        degraded: unknown_count > 0,
        malformed_count,
        unknown_count,
        possibly_incomplete,
        included_turns,
        omitted_turns,
    })
}

fn pair_turns(events: Vec<RawEvent>) -> (Vec<Turn>, usize) {
    let mut turns: Vec<Turn> = Vec::new();
    let mut pending: HashMap<String, usize> = HashMap::default();
    let mut omitted_noise = 0usize;

    for event in events {
        match event {
            RawEvent::User(text) if text.trim().is_empty() => {}
            RawEvent::Assistant(text) if text.trim().is_empty() => {}
            RawEvent::User(text) => turns.push(Turn::User(text)),
            RawEvent::Assistant(text) => turns.push(Turn::Assistant(text)),
            RawEvent::Checkpoint(text) => turns.push(Turn::Checkpoint(text)),
            RawEvent::Noise => omitted_noise += 1,
            RawEvent::ToolCall {
                call_id,
                name,
                detail,
            } => {
                let index = turns.len();
                turns.push(Turn::Tool {
                    name,
                    detail,
                    output: None,
                    is_error: false,
                    resolved: false,
                });
                if let Some(call_id) = call_id {
                    pending.insert(call_id, index);
                }
            }
            RawEvent::ToolResult {
                call_id,
                output,
                is_error,
            } => {
                let matched = call_id
                    .as_ref()
                    .and_then(|call_id| pending.remove(call_id))
                    .and_then(|index| match &mut turns[index] {
                        Turn::Tool {
                            output: existing,
                            is_error: existing_error,
                            resolved,
                            ..
                        } => {
                            *existing = Some(output.clone());
                            *existing_error = is_error;
                            *resolved = true;
                            Some(())
                        }
                        _ => None,
                    });
                if matched.is_none() {
                    turns.push(Turn::Tool {
                        name: "result".to_string(),
                        detail: String::new(),
                        output: Some(output),
                        is_error,
                        resolved: true,
                    });
                }
            }
        }
    }

    (turns, omitted_noise)
}

fn render_turn(turn: &Turn, budget: &ExcerptBudget) -> String {
    match turn {
        Turn::User(text) => format!("**User:** {}", text.trim()),
        Turn::Assistant(text) => format!("**Agent:** {}", text.trim()),
        Turn::Checkpoint(text) => format!("_[checkpoint: {}]_", text.trim()),
        Turn::Tool {
            name,
            detail,
            output,
            is_error,
            resolved,
        } => {
            let mut rendered = String::new();
            let status = if !*resolved {
                " (unresolved)"
            } else if *is_error {
                " (error)"
            } else {
                ""
            };
            if detail.trim().is_empty() {
                rendered.push_str(&format!("**Tool `{name}`{status}**"));
            } else {
                rendered.push_str(&format!("**Tool `{name}`{status}:** {}", detail.trim()));
            }
            if let Some(output) = output {
                let output = output.trim();
                if !output.is_empty() {
                    let (tail, _) = truncate_tail(output, budget.max_tool_bytes);
                    rendered.push_str(&format!("\n**Result:** {tail}"));
                }
            }
            rendered
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_document(
    turns: &[Turn],
    rendered: &[String],
    selected: &[bool],
    head_index: Option<usize>,
    budget: &ExcerptBudget,
    omitted_turns: usize,
    omitted_noise: usize,
    unknown_count: usize,
    malformed_count: usize,
    possibly_incomplete: bool,
) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut previous_selected: Option<usize> = None;

    for index in 0..turns.len() {
        if !selected[index] {
            continue;
        }
        // A run of skipped turns between two kept turns becomes one gap marker.
        if let Some(previous) = previous_selected {
            let gap = index - previous - 1;
            if gap > 0 {
                blocks.push(format!("_[{gap} earlier turns omitted]_"));
            }
        }
        if Some(index) == head_index {
            let (head, _) = truncate_head(&rendered[index], budget.max_head_bytes);
            blocks.push(head);
        } else {
            blocks.push(rendered[index].clone());
        }
        previous_selected = Some(index);
    }

    let _ = omitted_turns;
    let mut notes: Vec<String> = Vec::new();
    if omitted_noise > 0 {
        notes.push(format!("{omitted_noise} internal records omitted"));
    }
    if unknown_count > 0 {
        notes.push(format!("{unknown_count} unrecognized records"));
    }
    if malformed_count > 0 {
        notes.push(format!("{malformed_count} malformed records"));
    }
    if possibly_incomplete {
        notes.push("excerpt may be incomplete (source still active)".to_string());
    }
    if !notes.is_empty() {
        blocks.push(format!("_[extraction notes: {}]_", notes.join("; ")));
    }

    blocks.join("\n\n")
}

/// Produces a compact one-line summary of a tool call's input for use as a
/// [`RawEvent::ToolCall`] detail. Accepts a JSON object/string; surfaces a few
/// common command/path fields, otherwise falls back to the raw serialization.
/// The selection layer bounds the final rendered size.
pub(crate) fn summarize_tool_input(input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    let parsed = match input {
        Value::String(text) => {
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.clone()))
        }
        other => other.clone(),
    };
    if let Value::String(text) = &parsed {
        return one_line(text);
    }
    if let Value::Object(map) = &parsed {
        // `command` may be a string or an argv array.
        if let Some(command) = map.get("command") {
            if let Some(text) = command.as_str() {
                return one_line(text);
            }
            if let Some(items) = command.as_array() {
                let joined = items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                if !joined.is_empty() {
                    return one_line(&joined);
                }
            }
        }
        for key in [
            "cmd",
            "path",
            "file_path",
            "pattern",
            "query",
            "description",
        ] {
            if let Some(text) = map.get(key).and_then(Value::as_str) {
                return one_line(text);
            }
        }
    }
    one_line(&parsed.to_string())
}

pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncates keeping the head, on a UTF-8 boundary, appending an ellipsis.
fn truncate_head(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &text[..end]), true)
}

/// Truncates keeping the tail, on a UTF-8 boundary, prepending an ellipsis. Used
/// for tool output so the exit/error at the end survives.
fn truncate_tail(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    (format!("…{}", &text[start..]), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> RawEvent {
        RawEvent::User(text.to_string())
    }
    fn assistant(text: &str) -> RawEvent {
        RawEvent::Assistant(text.to_string())
    }
    fn call(id: &str, name: &str, detail: &str) -> RawEvent {
        RawEvent::ToolCall {
            call_id: Some(id.to_string()),
            name: name.to_string(),
            detail: detail.to_string(),
        }
    }
    fn result(id: &str, output: &str, is_error: bool) -> RawEvent {
        RawEvent::ToolResult {
            call_id: Some(id.to_string()),
            output: output.to_string(),
            is_error,
        }
    }

    fn classified(events: Vec<RawEvent>) -> Classified {
        Classified {
            events,
            malformed_count: 0,
            unknown_count: 0,
        }
    }

    #[test]
    fn refuses_when_no_conversation_survives() {
        let result = select_and_render(
            classified(vec![RawEvent::Noise, RawEvent::Noise]),
            &DEFAULT_BUDGET,
            false,
        );
        assert_eq!(result, Err(ExtractionRefusal::NoUsableContent));
    }

    #[test]
    fn renders_head_and_pairs_tool_call_with_result() {
        let excerpt = select_and_render(
            classified(vec![
                user("Fix the parser"),
                assistant("Looking now"),
                call("c1", "bash", "cargo test"),
                result("c1", "ok\nexit 0", false),
            ]),
            &DEFAULT_BUDGET,
            false,
        )
        .unwrap();

        assert!(excerpt.markdown.starts_with("**User:** Fix the parser"));
        assert!(excerpt.markdown.contains("**Tool `bash`:** cargo test"));
        assert!(excerpt.markdown.contains("**Result:** ok\nexit 0"));
        assert!(!excerpt.degraded);
        assert_eq!(excerpt.omitted_turns, 0);
    }

    #[test]
    fn unresolved_tool_call_is_marked() {
        let excerpt = select_and_render(
            classified(vec![user("go"), call("c1", "bash", "sleep 100")]),
            &DEFAULT_BUDGET,
            false,
        )
        .unwrap();
        assert!(
            excerpt
                .markdown
                .contains("**Tool `bash` (unresolved):** sleep 100")
        );
    }

    #[test]
    fn failed_tool_result_is_marked_and_output_tail_kept() {
        let budget = ExcerptBudget {
            max_tool_bytes: 12,
            ..DEFAULT_BUDGET
        };
        let excerpt = select_and_render(
            classified(vec![
                user("build"),
                call("c1", "bash", "make"),
                result("c1", "compiling...\nfatal error E42", true),
            ]),
            &budget,
            false,
        )
        .unwrap();
        assert!(excerpt.markdown.contains("**Tool `bash` (error):**"));
        // The tail (the error), not the head, must survive truncation.
        assert!(excerpt.markdown.contains("error E42"));
        assert!(excerpt.markdown.contains('…'));
    }

    #[test]
    fn noise_does_not_consume_turn_budget_and_is_noted() {
        let mut events = vec![user("start")];
        for _ in 0..50 {
            events.push(RawEvent::Noise);
        }
        events.push(assistant("done"));
        let excerpt = select_and_render(classified(events), &DEFAULT_BUDGET, false).unwrap();
        assert!(excerpt.markdown.contains("**User:** start"));
        assert!(excerpt.markdown.contains("**Agent:** done"));
        assert!(excerpt.markdown.contains("50 internal records omitted"));
    }

    #[test]
    fn unknown_records_set_degraded() {
        let excerpt = select_and_render(
            Classified {
                events: vec![user("hi"), assistant("hello")],
                malformed_count: 1,
                unknown_count: 2,
            },
            &DEFAULT_BUDGET,
            false,
        )
        .unwrap();
        assert!(excerpt.degraded);
        assert!(excerpt.markdown.contains("2 unrecognized records"));
        assert!(excerpt.markdown.contains("1 malformed records"));
    }

    #[test]
    fn possibly_incomplete_is_flagged_in_notes() {
        let excerpt =
            select_and_render(classified(vec![user("hi")]), &DEFAULT_BUDGET, true).unwrap();
        assert!(excerpt.possibly_incomplete);
        assert!(excerpt.markdown.contains("source still active"));
    }

    #[test]
    fn head_always_kept_and_middle_turns_omitted_under_tight_budget() {
        let budget = ExcerptBudget {
            max_total_bytes: 120,
            max_turns: 3,
            ..DEFAULT_BUDGET
        };
        let mut events = vec![user("the original task framing")];
        for index in 0..20 {
            events.push(assistant(&format!("assistant turn number {index}")));
        }
        let excerpt = select_and_render(classified(events), &budget, false).unwrap();
        // The first user turn survives even though it is the oldest.
        assert!(excerpt.markdown.contains("the original task framing"));
        // The most recent assistant turn survives (newest-first fill).
        assert!(excerpt.markdown.contains("assistant turn number 19"));
        // A gap marker records the omitted middle.
        assert!(excerpt.markdown.contains("earlier turns omitted"));
        assert!(excerpt.omitted_turns > 0);
    }

    #[test]
    fn char_boundary_truncation_does_not_panic_on_multibyte() {
        let budget = ExcerptBudget {
            max_head_bytes: 5,
            ..DEFAULT_BUDGET
        };
        // Each emoji is 4 bytes; a naive byte slice at 5 would split one.
        let excerpt =
            select_and_render(classified(vec![user("😀😀😀😀")]), &budget, false).unwrap();
        assert!(excerpt.markdown.contains('…'));
    }
}
