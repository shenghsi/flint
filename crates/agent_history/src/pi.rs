use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transcript::{Classified, RawEvent, summarize_tool_input};
use crate::{
    FileIdentity, HistoryHost, HistoryKind, HistoryProvider, IndexedSession, ProviderRefresh,
};

const MAX_PROJECT_HISTORY_FILES_SCANNED: usize = 200;
const MAX_TITLE_CHARS: usize = 60;
const DEFAULT_TITLE: &str = "Pi session";

pub struct PiHistoryProvider;

#[async_trait]
impl HistoryProvider for PiHistoryProvider {
    fn kind(&self) -> HistoryKind {
        HistoryKind::Pi
    }

    async fn refresh(
        &self,
        host: &HistoryHost,
        previous: Option<&Value>,
    ) -> Result<ProviderRefresh> {
        let previous = previous
            .and_then(|value| serde_json::from_value::<PiSourceState>(value.clone()).ok())
            .unwrap_or_default();

        let sessions_dir = host.join(&host.base_dir, "sessions")?;
        let mut files = BTreeMap::new();
        for project_dir in host.fs.read_dir(&sessions_dir).await.unwrap_or_default() {
            let dir_name = project_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let mut project_files = host
                .fs
                .read_dir(&project_dir)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "jsonl")
                })
                .collect::<Vec<_>>();
            project_files.sort_unstable_by(|left, right| right.cmp(left));
            project_files.truncate(MAX_PROJECT_HISTORY_FILES_SCANNED);
            for path in project_files {
                let Some(key) = path.to_str().map(str::to_string) else {
                    continue;
                };
                let Some(identity) = host.fs.metadata(&path).await.ok().flatten() else {
                    continue;
                };
                let summary = match previous.files.get(&key) {
                    Some(entry) if entry.identity == identity => entry.summary.clone(),
                    _ => parse_session_summary(&host.fs.load(&path).await.unwrap_or_default()),
                };
                files.insert(
                    key,
                    SessionFileEntry {
                        identity,
                        dir_name: dir_name.clone(),
                        summary,
                    },
                );
            }
        }

        let mut sessions = Vec::new();
        for entry in files.values() {
            let Some(summary) = &entry.summary else {
                continue;
            };
            // The legacy scanner only opens `sessions/<enc(root)>/`, so a file
            // surfaces only when its recorded project root encodes to this
            // file's directory.
            if encoded_project_directory(&summary.project_root) != entry.dir_name {
                continue;
            }
            sessions.push(IndexedSession {
                session_id: summary.session_id.clone(),
                resolved_title: summary.title.clone(),
                fallback_title: None,
                working_dir: summary.project_root.clone(),
                last_activity_secs: summary.last_activity_secs,
                last_activity_nanos: summary.last_activity_nanos,
            });
        }

        let source_state = serde_json::to_value(PiSourceState { files })?;
        Ok(ProviderRefresh {
            source_state,
            sessions,
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
struct PiSourceState {
    files: BTreeMap<String, SessionFileEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SessionFileEntry {
    identity: FileIdentity,
    dir_name: String,
    summary: Option<PiSummary>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PiSummary {
    session_id: String,
    title: String,
    project_root: String,
    last_activity_secs: u64,
    last_activity_nanos: u32,
}

fn encoded_project_directory(project_root: &str) -> String {
    let project_root = project_root
        .strip_prefix('/')
        .or_else(|| project_root.strip_prefix('\\'))
        .unwrap_or(project_root);
    let encoded = project_root.replace(['/', '\\', ':'], "-");
    format!("--{encoded}--")
}

fn parse_session_summary(content: &str) -> Option<PiSummary> {
    let mut session_id = None;
    let mut project_root = None;
    let mut header_timestamp = None;
    let mut last_activity_at = UNIX_EPOCH;
    let mut session_name = None;
    let mut first_user_message = None;

    for line in content.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let entry_type = entry.get("type").and_then(Value::as_str);
        let timestamp = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp);
        if let Some(timestamp) = timestamp {
            last_activity_at = last_activity_at.max(timestamp);
        }
        match entry_type {
            Some("session") => {
                session_id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string);
                project_root = entry.get("cwd").and_then(Value::as_str).map(str::to_string);
                header_timestamp = timestamp;
            }
            Some("session_info") => {
                if let Some(name) = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(normalize_title)
                {
                    session_name = Some(name);
                }
            }
            Some("message") if first_user_message.is_none() => {
                first_user_message = user_message_title(&entry);
            }
            _ => {}
        }
    }

    let session_id = session_id?;
    let project_root = project_root?;
    let header_timestamp = header_timestamp?;
    let last_activity_at = last_activity_at.max(header_timestamp);
    let (last_activity_secs, last_activity_nanos) = last_activity_at
        .duration_since(UNIX_EPOCH)
        .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
        .unwrap_or((0, 0));
    Some(PiSummary {
        session_id,
        title: session_name
            .or(first_user_message)
            .unwrap_or_else(|| DEFAULT_TITLE.to_string()),
        project_root,
        last_activity_secs,
        last_activity_nanos,
    })
}

/// Classifies a Pi session into the shared event stream.
///
/// Pi's file is an append-only tree keyed by `id`/`parentId`, so the active
/// conversation is the parent chain from the last valid entry back to the root;
/// abandoned branches are discarded. Tool calls are `toolCall` blocks on
/// assistant messages; tool results are separate `role: "toolResult"` messages.
pub(crate) fn classify_transcript(content: &str) -> Classified {
    let mut classified = Classified::default();

    // Parse every line, preserving order and indexing by id.
    let mut entries: Vec<Value> = Vec::new();
    let mut index_by_id: BTreeMap<String, usize> = BTreeMap::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            classified.malformed_count += 1;
            continue;
        };
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            index_by_id.insert(id.to_string(), entries.len());
        }
        entries.push(value);
    }

    // Walk the active branch: from the last entry that carries an id, follow
    // `parentId` to the root, then restore chronological order.
    let Some(leaf) = entries
        .iter()
        .rev()
        .find(|entry| entry.get("id").and_then(Value::as_str).is_some())
    else {
        return classified;
    };
    let mut chain: Vec<&Value> = Vec::new();
    let mut current = Some(leaf);
    let mut guard = entries.len() + 1;
    while let Some(entry) = current {
        chain.push(entry);
        guard -= 1;
        if guard == 0 {
            break;
        }
        current = entry
            .get("parentId")
            .and_then(Value::as_str)
            .and_then(|parent| index_by_id.get(parent))
            .and_then(|index| entries.get(*index));
    }
    chain.reverse();

    for entry in chain {
        classify_entry(entry, &mut classified);
    }
    classified
}

fn classify_entry(entry: &Value, classified: &mut Classified) {
    match entry.get("type").and_then(Value::as_str) {
        Some("message") => classify_message(entry, classified),
        Some("session") | Some("model_change") | Some("thinking_level_change") => {
            classified.events.push(RawEvent::Noise)
        }
        Some("compaction") | Some("summary") | Some("branch_summary") => classified
            .events
            .push(RawEvent::Checkpoint("compaction".to_string())),
        _ => classified.unknown_count += 1,
    }
}

fn classify_message(entry: &Value, classified: &mut Classified) {
    let Some(message) = entry.get("message") else {
        classified.malformed_count += 1;
        return;
    };
    match message.get("role").and_then(Value::as_str) {
        Some("user") => {
            let text = message_text(message);
            if !text.trim().is_empty() {
                classified.events.push(RawEvent::User(text));
            }
        }
        Some("assistant") => classify_assistant_blocks(message, classified),
        Some("toolResult") => classified.events.push(RawEvent::ToolResult {
            call_id: message
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(str::to_string),
            output: message_text(message),
            is_error: message.get("isError").and_then(Value::as_bool) == Some(true),
        }),
        _ => classified.unknown_count += 1,
    }
}

fn classify_assistant_blocks(message: &Value, classified: &mut Classified) {
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        classified
            .events
            .push(RawEvent::Assistant(text.to_string()));
        return;
    }
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        // Some assistant messages carry blocks under `blocks`.
        if let Some(blocks) = message.get("blocks").and_then(Value::as_array) {
            classify_blocks(blocks, classified);
        } else {
            classified.malformed_count += 1;
        }
        return;
    };
    classify_blocks(blocks, classified);
}

fn classify_blocks(blocks: &[Value], classified: &mut Classified) {
    let mut pending_text: Vec<String> = Vec::new();
    let flush = |pending_text: &mut Vec<String>, classified: &mut Classified| {
        let joined = pending_text.join("\n");
        pending_text.clear();
        if !joined.trim().is_empty() {
            classified.events.push(RawEvent::Assistant(joined));
        }
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    pending_text.push(text.to_string());
                }
            }
            Some("toolCall") => {
                flush(&mut pending_text, classified);
                classified.events.push(RawEvent::ToolCall {
                    call_id: block
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    name: block
                        .get("name")
                        .or_else(|| block.get("toolName"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    detail: summarize_tool_input(block.get("arguments")),
                });
            }
            Some("thinking") | Some("redacted_thinking") => classified.events.push(RawEvent::Noise),
            _ => classified.unknown_count += 1,
        }
    }
    flush(&mut pending_text, classified);
}

/// Extracts message text from either a string `content`, a `content` array of
/// `{type:text}` blocks, or a `blocks` array.
fn message_text(message: &Value) -> String {
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        return text.to_string();
    }
    for key in ["content", "blocks"] {
        if let Some(blocks) = message.get(key).and_then(Value::as_array) {
            let text = blocks
                .iter()
                .filter(|block| {
                    block
                        .get("type")
                        .and_then(Value::as_str)
                        .map(|kind| kind == "text")
                        .unwrap_or(true)
                })
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return text;
            }
        }
    }
    String::new()
}

fn parse_timestamp(timestamp: &str) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|timestamp| {
            UNIX_EPOCH + Duration::from_millis(timestamp.timestamp_millis().max(0) as u64)
        })
}

fn user_message_title(entry: &Value) -> Option<String> {
    let message = entry.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = message.get("content")?;
    if let Some(content) = content.as_str() {
        return normalize_title(content);
    }
    let text = content
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    normalize_title(&text)
}

fn normalize_title(title: &str) -> Option<String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return None;
    }
    if title.chars().count() <= MAX_TITLE_CHARS {
        return Some(title);
    }
    Some(format!(
        "{}…",
        title.chars().take(MAX_TITLE_CHARS).collect::<String>()
    ))
}

#[cfg(test)]
mod transcript_tests {
    use super::*;
    use crate::transcript::RawEvent;

    fn line(value: Value) -> String {
        value.to_string()
    }

    #[test]
    fn walks_parent_chain_and_discards_abandoned_branch() {
        // Tree: root user u1 -> assistant a1 (branch A, edited away)
        //                    -> assistant a2 (branch B, the live leaf)
        // a2 is last in file order, so its chain (u1 -> a2) is the active one
        // and a1 must be discarded.
        let content = [
            line(serde_json::json!({ "type": "session", "id": "s" })),
            line(serde_json::json!({
                "type": "message", "id": "u1", "parentId": "s",
                "message": { "role": "user", "content": "the real question" }
            })),
            line(serde_json::json!({
                "type": "message", "id": "a1", "parentId": "u1",
                "message": { "role": "assistant", "content": "abandoned answer" }
            })),
            line(serde_json::json!({
                "type": "message", "id": "a2", "parentId": "u1",
                "message": { "role": "assistant", "content": "the kept answer" }
            })),
        ]
        .join("\n");

        let classified = classify_transcript(&content);
        assert_eq!(
            classified.events,
            vec![
                RawEvent::Noise, // session header
                RawEvent::User("the real question".to_string()),
                RawEvent::Assistant("the kept answer".to_string()),
            ]
        );
    }

    #[test]
    fn tool_call_block_and_tool_result_message_pair() {
        let content = [
            line(serde_json::json!({
                "type": "message", "id": "u1", "parentId": null,
                "message": { "role": "user", "content": "build it" }
            })),
            line(serde_json::json!({
                "type": "message", "id": "a1", "parentId": "u1",
                "message": { "role": "assistant", "blocks": [
                    { "type": "thinking" },
                    { "type": "text", "text": "running build" },
                    { "type": "toolCall", "toolCallId": "t1", "name": "bash", "arguments": { "command": "make" } }
                ]}
            })),
            line(serde_json::json!({
                "type": "message", "id": "r1", "parentId": "a1",
                "message": { "role": "toolResult", "toolCallId": "t1", "isError": true, "content": "make: fatal" }
            })),
        ]
        .join("\n");

        let classified = classify_transcript(&content);
        assert_eq!(
            classified.events,
            vec![
                RawEvent::User("build it".to_string()),
                RawEvent::Noise, // thinking
                RawEvent::Assistant("running build".to_string()),
                RawEvent::ToolCall {
                    call_id: Some("t1".to_string()),
                    name: "bash".to_string(),
                    detail: "make".to_string(),
                },
                RawEvent::ToolResult {
                    call_id: Some("t1".to_string()),
                    output: "make: fatal".to_string(),
                    is_error: true,
                },
            ]
        );
    }

    #[test]
    fn model_change_is_noise_unknown_type_counted() {
        let content = [
            line(serde_json::json!({ "type": "model_change", "id": "m1", "parentId": null })),
            line(serde_json::json!({
                "type": "message", "id": "u1", "parentId": "m1",
                "message": { "role": "user", "content": "hi" }
            })),
            line(serde_json::json!({ "type": "brand_new", "id": "x1", "parentId": "u1" })),
        ]
        .join("\n");

        let classified = classify_transcript(&content);
        assert_eq!(
            classified.events,
            vec![RawEvent::Noise, RawEvent::User("hi".to_string())]
        );
        assert_eq!(classified.unknown_count, 1);
    }
}
