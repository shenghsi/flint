use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use collections::HashSet;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transcript::{Classified, RawEvent, summarize_tool_input};
use crate::{
    FileIdentity, HistoryHost, HistoryKind, HistoryProvider, IndexedSession, ProviderRefresh,
};

/// Bounds match cost regardless of total history depth: only the most recent
/// rollout files are opened to read their `cwd`. Matches the legacy scanner.
const MAX_ROLLOUT_FILES_SCANNED: usize = 200;
const MAX_TITLE_CHARS: usize = 60;
const DEFAULT_TITLE: &str = "Codex session";

pub struct CodexHistoryProvider;

#[async_trait]
impl HistoryProvider for CodexHistoryProvider {
    fn kind(&self) -> HistoryKind {
        HistoryKind::Codex
    }

    async fn refresh(
        &self,
        host: &HistoryHost,
        previous: Option<&Value>,
    ) -> Result<ProviderRefresh> {
        let previous = previous
            .and_then(|value| serde_json::from_value::<CodexSourceState>(value.clone()).ok())
            .unwrap_or_default();

        let sessions_dir = host.join(&host.base_dir, "sessions")?;
        let mut rollout_files = list_rollout_files(host, &sessions_dir).await;
        // Filenames embed an ISO timestamp, so descending lexical order is
        // descending chronological order.
        rollout_files.sort_unstable_by(|left, right| right.cmp(left));
        rollout_files.truncate(MAX_ROLLOUT_FILES_SCANNED);

        let mut rollouts = BTreeMap::new();
        let mut ordered_keys = Vec::new();
        for path in &rollout_files {
            let Some(key) = path.to_str().map(str::to_string) else {
                continue;
            };
            let Some(identity) = host.fs.metadata(path).await? else {
                // Vanished between listing and metadata; skip this refresh.
                continue;
            };
            let summary = match previous.rollouts.get(&key) {
                Some(entry) if entry.identity == identity => entry.summary.clone(),
                // New or changed (or a changed previously-malformed) file: parse.
                _ => parse_summary(&host.fs.load(path).await.unwrap_or_default()),
            };
            rollouts.insert(key.clone(), RolloutEntry { identity, summary });
            ordered_keys.push(key);
        }

        let session_index = refresh_title_source(
            host,
            &host.join(&host.base_dir, "session_index.jsonl")?,
            previous.session_index.as_ref(),
            parse_session_index_titles,
        )
        .await;
        let history = refresh_title_source(
            host,
            &host.join(&host.base_dir, "history.jsonl")?,
            previous.history.as_ref(),
            parse_history_titles,
        )
        .await;

        let mut sessions = Vec::new();
        for key in &ordered_keys {
            let Some(entry) = rollouts.get(key) else {
                continue;
            };
            let Some(summary) = &entry.summary else {
                continue;
            };
            let resolved_title = session_index
                .as_ref()
                .and_then(|source| source.titles.get(&summary.id).cloned())
                .or_else(|| {
                    history
                        .as_ref()
                        .and_then(|source| source.titles.get(&summary.id).cloned())
                })
                .or_else(|| summary.fallback_title.clone())
                .unwrap_or_else(|| DEFAULT_TITLE.to_string());
            sessions.push(IndexedSession {
                session_id: summary.id.clone(),
                resolved_title,
                fallback_title: summary.fallback_title.clone(),
                working_dir: summary.cwd.clone(),
                last_activity_secs: summary.timestamp_secs,
                last_activity_nanos: summary.timestamp_nanos,
            });
        }

        let source_state = serde_json::to_value(CodexSourceState {
            rollouts,
            session_index,
            history,
        })?;
        Ok(ProviderRefresh {
            source_state,
            sessions,
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
struct CodexSourceState {
    rollouts: BTreeMap<String, RolloutEntry>,
    session_index: Option<TitleSource>,
    history: Option<TitleSource>,
}

#[derive(Clone, Serialize, Deserialize)]
struct RolloutEntry {
    identity: FileIdentity,
    /// `None` records a malformed file so later refreshes don't reload it until
    /// its identity changes.
    summary: Option<Summary>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Summary {
    id: String,
    cwd: String,
    timestamp_secs: u64,
    timestamp_nanos: u32,
    fallback_title: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct TitleSource {
    identity: FileIdentity,
    titles: BTreeMap<String, String>,
}

async fn refresh_title_source(
    host: &HistoryHost,
    path: &Path,
    previous: Option<&TitleSource>,
    parse: impl Fn(&str) -> BTreeMap<String, String>,
) -> Option<TitleSource> {
    let identity = host.fs.metadata(path).await.ok().flatten()?;
    if let Some(previous) = previous {
        if previous.identity == identity {
            return Some(previous.clone());
        }
    }
    let content = host.fs.load(path).await.ok()?;
    Some(TitleSource {
        identity,
        titles: parse(&content),
    })
}

async fn list_rollout_files(host: &HistoryHost, sessions_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for year_path in list_dir(host, sessions_dir).await {
        for month_path in list_dir(host, &year_path).await {
            for day_path in list_dir(host, &month_path).await {
                for file_path in list_dir(host, &day_path).await {
                    if file_path.extension().is_some_and(|ext| ext == "jsonl") {
                        files.push(file_path);
                    }
                }
            }
        }
    }
    files
}

async fn list_dir(host: &HistoryHost, dir: &Path) -> Vec<PathBuf> {
    host.fs.read_dir(dir).await.unwrap_or_default()
}

fn parse_summary(content: &str) -> Option<Summary> {
    let mut lines = content.lines();
    let parsed: SessionMetaLine = serde_json::from_str(lines.next()?.trim()).ok()?;
    if parsed.kind != "session_meta" {
        return None;
    }
    let timestamp = chrono::DateTime::parse_from_rfc3339(&parsed.payload.timestamp)
        .map(|date_time| {
            UNIX_EPOCH + Duration::from_millis(date_time.timestamp_millis().max(0) as u64)
        })
        .unwrap_or(UNIX_EPOCH);
    let (timestamp_secs, timestamp_nanos) = timestamp
        .duration_since(UNIX_EPOCH)
        .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
        .unwrap_or((0, 0));
    let mut seen_user_messages = HashSet::default();
    let fallback_title = lines.find_map(|line| {
        let parsed = serde_json::from_str::<Value>(line).ok()?;
        let payload = parsed.get("payload")?;
        user_message_title(payload, &mut seen_user_messages)
    });
    Some(Summary {
        id: parsed.payload.id,
        cwd: parsed.payload.cwd,
        timestamp_secs,
        timestamp_nanos,
        fallback_title,
    })
}

/// Classifies a Codex rollout into the shared transcript event stream.
///
/// `response_item` records are the canonical user/assistant/tool stream.
/// `event_msg` records (including `user_message`/`agent_message`) duplicate that
/// content and are treated as noise; only lifecycle/metadata value is lost,
/// which is intentional. `reasoning` records are excluded (they may be
/// encrypted and are not assistant text), and the `developer` role is not user
/// content.
pub(crate) fn classify_transcript(content: &str) -> Classified {
    let mut classified = Classified::default();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            classified.malformed_count += 1;
            continue;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("response_item") => {
                classify_response_item(record.get("payload"), &mut classified);
            }
            // Canonical content lives in `response_item`; these are metadata,
            // duplicated text, or lifecycle events -- known, non-conversational.
            Some("event_msg") | Some("session_meta") | Some("turn_context")
            | Some("world_state") => classified.events.push(RawEvent::Noise),
            Some(_) | None => classified.unknown_count += 1,
        }
    }
    classified
}

fn classify_response_item(payload: Option<&Value>, classified: &mut Classified) {
    let Some(payload) = payload else {
        classified.malformed_count += 1;
        return;
    };
    match payload.get("type").and_then(Value::as_str) {
        Some("message") => {
            let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
            let text = message_text(payload);
            match role {
                "user" => classified.events.push(RawEvent::User(text)),
                "assistant" => classified.events.push(RawEvent::Assistant(text)),
                // `developer` (instructions) and any other role are not
                // conversation.
                _ => classified.events.push(RawEvent::Noise),
            }
        }
        Some("function_call") => classified.events.push(RawEvent::ToolCall {
            call_id: payload
                .get("call_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            name: payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string(),
            detail: summarize_tool_input(payload.get("arguments")),
        }),
        Some("custom_tool_call") => classified.events.push(RawEvent::ToolCall {
            call_id: payload
                .get("call_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            name: payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string(),
            detail: summarize_tool_input(payload.get("input")),
        }),
        Some("function_call_output") | Some("custom_tool_call_output") => {
            let (output, is_error) = tool_output(payload.get("output"));
            classified.events.push(RawEvent::ToolResult {
                call_id: payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                output,
                is_error,
            });
        }
        // Reasoning is known and deliberately excluded, not unknown.
        Some("reasoning") => classified.events.push(RawEvent::Noise),
        Some(_) | None => classified.unknown_count += 1,
    }
}

/// Joins the `text` of a message's content blocks regardless of block type name
/// (`input_text`, `output_text`, `text`).
fn message_text(payload: &Value) -> String {
    let Some(blocks) = payload.get("content").and_then(Value::as_array) else {
        return payload
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    };
    blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extracts a tool result's text and error state. Codex exec outputs are often a
/// JSON object carrying the text plus `metadata.exit_code`; a non-zero exit is
/// an error. A plain-string output is used as-is.
fn tool_output(output: Option<&Value>) -> (String, bool) {
    let Some(output) = output else {
        return (String::new(), false);
    };
    let value = match output {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(parsed) if parsed.is_object() => parsed,
            _ => return (text.clone(), false),
        },
        other => other.clone(),
    };
    if let Value::Object(map) = &value {
        let text = map
            .get("output")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string());
        let is_error = map
            .get("metadata")
            .and_then(|metadata| metadata.get("exit_code"))
            .and_then(Value::as_i64)
            .map(|code| code != 0)
            .unwrap_or(false);
        return (text, is_error);
    }
    (value.to_string(), false)
}

fn parse_session_index_titles(content: &str) -> BTreeMap<String, String> {
    let mut titles = BTreeMap::new();
    for line in content.lines() {
        if let Ok(entry) = serde_json::from_str::<SessionIndexEntry>(line) {
            if let Some(thread_name) = entry.thread_name.and_then(normalize_title) {
                titles.insert(entry.id, thread_name);
            }
        }
    }
    titles
}

fn parse_history_titles(content: &str) -> BTreeMap<String, String> {
    let mut latest: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for line in content.lines() {
        let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) else {
            continue;
        };
        let Some(title) = normalize_title(entry.text) else {
            continue;
        };
        let existing_is_newer = latest
            .get(&entry.session_id)
            .is_some_and(|(timestamp, _)| *timestamp > entry.ts);
        if !existing_is_newer {
            latest.insert(entry.session_id, (entry.ts, title));
        }
    }
    latest
        .into_iter()
        .map(|(session_id, (_, title))| (session_id, title))
        .collect()
}

fn user_message_title(payload: &Value, seen_user_messages: &mut HashSet<String>) -> Option<String> {
    if payload.get("type").and_then(Value::as_str) == Some("user_message") {
        let message = payload.get("message").and_then(Value::as_str)?;
        return normalize_title(message.to_string());
    }

    if payload.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = payload.get("content")?.as_array()?;
    let mut parts = Vec::new();
    for item in content {
        let Some(text) = item
            .get("text")
            .or_else(|| item.get("content"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if seen_user_messages.insert(text.to_string()) {
            parts.push(text);
        }
    }
    normalize_title(parts.join(" "))
}

fn normalize_title(text: String) -> Option<String> {
    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return None;
    }
    if title.chars().count() <= MAX_TITLE_CHARS {
        Some(title)
    } else {
        Some(format!(
            "{}…",
            title.chars().take(MAX_TITLE_CHARS).collect::<String>()
        ))
    }
}

#[derive(Deserialize)]
struct SessionMetaLine {
    #[serde(rename = "type")]
    kind: String,
    payload: SessionMetaPayload,
}

#[derive(Deserialize)]
struct SessionMetaPayload {
    id: String,
    cwd: String,
    timestamp: String,
}

#[derive(Deserialize)]
struct SessionIndexEntry {
    id: String,
    thread_name: Option<String>,
}

#[derive(Deserialize)]
struct HistoryEntry {
    session_id: String,
    text: String,
    ts: u64,
}

#[cfg(test)]
mod transcript_tests {
    use super::*;
    use crate::transcript::RawEvent;

    fn response_item(payload: Value) -> String {
        serde_json::json!({ "type": "response_item", "payload": payload }).to_string()
    }

    #[test]
    fn message_roles_are_classified_and_developer_is_not_user() {
        let content = [
            response_item(serde_json::json!({
                "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "Fix the parser" }]
            })),
            response_item(serde_json::json!({
                "type": "message", "role": "assistant",
                "content": [{ "type": "output_text", "text": "On it" }]
            })),
            response_item(serde_json::json!({
                "type": "message", "role": "developer",
                "content": [{ "type": "input_text", "text": "<permissions instructions>" }]
            })),
        ]
        .join("\n");

        let classified = classify_transcript(&content);
        assert_eq!(
            classified.events,
            vec![
                RawEvent::User("Fix the parser".to_string()),
                RawEvent::Assistant("On it".to_string()),
                RawEvent::Noise,
            ]
        );
        assert_eq!(classified.unknown_count, 0);
    }

    #[test]
    fn both_tool_call_forms_and_exit_code_error_are_classified() {
        let content = [
            response_item(serde_json::json!({
                "type": "function_call",
                "name": "shell",
                "arguments": "{\"command\":[\"cargo\",\"test\"]}",
                "call_id": "call_1"
            })),
            response_item(serde_json::json!({
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "{\"output\":\"boom\",\"metadata\":{\"exit_code\":1}}"
            })),
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": "path/to/file.rs",
                "call_id": "call_2"
            })),
        ]
        .join("\n");

        let classified = classify_transcript(&content);
        assert_eq!(
            classified.events,
            vec![
                RawEvent::ToolCall {
                    call_id: Some("call_1".to_string()),
                    name: "shell".to_string(),
                    detail: "cargo test".to_string(),
                },
                RawEvent::ToolResult {
                    call_id: Some("call_1".to_string()),
                    output: "boom".to_string(),
                    is_error: true,
                },
                RawEvent::ToolCall {
                    call_id: Some("call_2".to_string()),
                    name: "apply_patch".to_string(),
                    detail: "path/to/file.rs".to_string(),
                },
            ]
        );
    }

    #[test]
    fn reasoning_and_event_msg_are_noise_not_unknown() {
        let content = [
            response_item(serde_json::json!({ "type": "reasoning", "summary": [] })),
            serde_json::json!({
                "type": "event_msg",
                "payload": { "type": "token_count", "total": 10 }
            })
            .to_string(),
            serde_json::json!({
                "type": "event_msg",
                "payload": { "type": "agent_message", "message": "duplicate text" }
            })
            .to_string(),
        ]
        .join("\n");

        let classified = classify_transcript(&content);
        assert_eq!(classified.events, vec![RawEvent::Noise; 3]);
        assert_eq!(classified.unknown_count, 0);
    }

    #[test]
    fn malformed_and_unknown_records_are_counted() {
        let content = [
            "not json at all".to_string(),
            serde_json::json!({ "type": "brand_new_top_level" }).to_string(),
            response_item(serde_json::json!({ "type": "brand_new_payload" })),
        ]
        .join("\n");

        let classified = classify_transcript(&content);
        assert_eq!(classified.malformed_count, 1);
        assert_eq!(classified.unknown_count, 2);
        assert!(classified.events.is_empty());
    }
}
