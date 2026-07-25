use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transcript::{Classified, RawEvent, one_line, summarize_tool_input};
use crate::{
    FileIdentity, HistoryHost, HistoryKind, HistoryProvider, IndexedSession, ProviderRefresh,
    normalize_path_for_style,
};

const MAX_TITLE_CHARS: usize = 60;
const MAX_PROJECT_HISTORY_FILES_SCANNED: usize = 200;

pub struct ClaudeHistoryProvider;

#[async_trait]
impl HistoryProvider for ClaudeHistoryProvider {
    fn kind(&self) -> HistoryKind {
        HistoryKind::Claude
    }

    async fn refresh(
        &self,
        host: &HistoryHost,
        previous: Option<&Value>,
    ) -> Result<ProviderRefresh> {
        let previous = previous
            .and_then(|value| serde_json::from_value::<ClaudeSourceState>(value.clone()).ok())
            .unwrap_or_default();

        let history = refresh_history_file(host, previous.history.as_ref()).await;
        let project_files = refresh_project_files(host, &previous.project_files).await;

        let sessions = build_sessions(host, history.as_ref(), &project_files);
        let source_state = serde_json::to_value(ClaudeSourceState {
            history,
            project_files,
        })?;
        Ok(ProviderRefresh {
            source_state,
            sessions,
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
struct ClaudeSourceState {
    history: Option<HistoryFileState>,
    project_files: BTreeMap<String, ProjectFileEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct HistoryFileState {
    identity: FileIdentity,
    records: Vec<StoredHistoryRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredHistoryRecord {
    session_id: String,
    project: String,
    title: String,
    timestamp_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct ProjectFileEntry {
    identity: FileIdentity,
    /// The parent directory name (`projects/<dir>/`), used to reproduce the
    /// legacy scanner which only surfaces a file when the requested root's
    /// encoded name equals this directory.
    dir_name: String,
    /// `None` records a malformed/titleless file so it is not reloaded until it
    /// changes.
    summary: Option<StoredSummary>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredSummary {
    session_id: String,
    title: Option<String>,
    last_activity_secs: u64,
    last_activity_nanos: u32,
    working_directories: Vec<String>,
}

async fn refresh_history_file(
    host: &HistoryHost,
    previous: Option<&HistoryFileState>,
) -> Option<HistoryFileState> {
    let history_path = host.join(&host.base_dir, "history.jsonl").ok()?;
    let identity = host.fs.metadata(&history_path).await.ok().flatten()?;
    if let Some(previous) = previous {
        if previous.identity == identity {
            return Some(previous.clone());
        }
    }
    let content = host.fs.load(&history_path).await.ok()?;
    let records = content
        .lines()
        .filter_map(|line| serde_json::from_str::<HistoryRecord>(line).ok())
        .filter_map(|record| {
            let title = normalize_title(&record.display)?;
            Some(StoredHistoryRecord {
                session_id: record.session_id,
                project: record.project,
                title,
                timestamp_ms: record.timestamp,
            })
        })
        .collect();
    Some(HistoryFileState { identity, records })
}

async fn refresh_project_files(
    host: &HistoryHost,
    previous: &BTreeMap<String, ProjectFileEntry>,
) -> BTreeMap<String, ProjectFileEntry> {
    let mut entries = BTreeMap::new();
    let Ok(projects_dir) = host.join(&host.base_dir, "projects") else {
        return entries;
    };
    for project_dir in host.fs.read_dir(&projects_dir).await.unwrap_or_default() {
        let dir_name = project_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let mut files = host
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
        files.sort_unstable_by(|left, right| right.cmp(left));
        files.truncate(MAX_PROJECT_HISTORY_FILES_SCANNED);
        for path in files {
            let Some(key) = path.to_str().map(str::to_string) else {
                continue;
            };
            let Some(identity) = host.fs.metadata(&path).await.ok().flatten() else {
                continue;
            };
            let summary = match previous.get(&key) {
                Some(entry) if entry.identity == identity => entry.summary.clone(),
                _ => {
                    let default_session_id = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or_default()
                        .to_string();
                    parse_project_summary(
                        &host.fs.load(&path).await.unwrap_or_default(),
                        default_session_id,
                    )
                }
            };
            entries.insert(
                key,
                ProjectFileEntry {
                    identity,
                    dir_name: dir_name.clone(),
                    summary,
                },
            );
        }
    }
    entries
}

/// Builds one index row per `(session, working-directory)` candidate, merging
/// the global `history.jsonl` records with per-project history files exactly as
/// the legacy scanner would per requested root: newest activity wins, with a
/// project-history file winning ties over the global history file.
fn build_sessions(
    host: &HistoryHost,
    history: Option<&HistoryFileState>,
    project_files: &BTreeMap<String, ProjectFileEntry>,
) -> Vec<IndexedSession> {
    struct Candidate {
        session_id: String,
        working_dir: String,
        title: String,
        activity: SystemTime,
        /// The per-project transcript file backing this row, when the candidate
        /// came from one. A history-only candidate has no transcript to extract.
        source: Option<(String, FileIdentity)>,
    }
    // Keyed by (session id, normalized working directory).
    let mut candidates: BTreeMap<(String, String), Candidate> = BTreeMap::new();
    let mut consider = |session_id: String,
                        working_dir: &str,
                        title: String,
                        activity: SystemTime,
                        from_project_file: bool,
                        source: Option<(String, FileIdentity)>| {
        let normalized_dir = normalize_path_for_style(working_dir, host.path_style);
        let key = (session_id.clone(), normalized_dir.clone());
        let replace = match candidates.get(&key) {
            None => true,
            // A project-history file wins ties over the global history file,
            // matching the legacy merge which applies project threads with `>=`.
            Some(existing) if from_project_file => activity >= existing.activity,
            Some(existing) => activity > existing.activity,
        };
        if replace {
            candidates.insert(
                key,
                Candidate {
                    session_id,
                    working_dir: normalized_dir,
                    title,
                    activity,
                    source,
                },
            );
        }
    };

    if let Some(history) = history {
        for record in &history.records {
            consider(
                record.session_id.clone(),
                &record.project,
                record.title.clone(),
                UNIX_EPOCH + Duration::from_millis(record.timestamp_ms),
                false,
                None,
            );
        }
    }

    for (path, entry) in project_files {
        let Some(summary) = &entry.summary else {
            continue;
        };
        let Some(title) = &summary.title else {
            continue;
        };
        let activity =
            UNIX_EPOCH + Duration::new(summary.last_activity_secs, summary.last_activity_nanos);
        for working_dir in &summary.working_directories {
            // The legacy scanner only opens `projects/<enc(root)>/`, so a
            // working directory surfaces only when its encoded name equals this
            // file's directory (usually the session's origin directory).
            if project_history_dir_name(working_dir) != entry.dir_name {
                continue;
            }
            consider(
                summary.session_id.clone(),
                working_dir,
                title.clone(),
                activity,
                true,
                Some((path.clone(), entry.identity)),
            );
        }
    }

    candidates
        .into_values()
        .map(|candidate| {
            let (last_activity_secs, last_activity_nanos) = candidate
                .activity
                .duration_since(UNIX_EPOCH)
                .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
                .unwrap_or((0, 0));
            let (source_path, source_identity) = match candidate.source {
                Some((path, identity)) => (Some(path), Some(identity)),
                None => (None, None),
            };
            IndexedSession {
                session_id: candidate.session_id,
                resolved_title: candidate.title,
                fallback_title: None,
                working_dir: candidate.working_dir,
                last_activity_secs,
                last_activity_nanos,
                source_path,
                source_identity,
            }
        })
        .collect()
}

fn parse_project_summary(content: &str, default_session_id: String) -> Option<StoredSummary> {
    let mut session_id = default_session_id;
    let mut title = None;
    let mut last_activity_at = UNIX_EPOCH;
    let mut working_directories: Vec<String> = Vec::new();

    for line in content.lines() {
        let Ok(record) = serde_json::from_str::<ProjectHistoryLine>(line) else {
            continue;
        };
        if let Some(record_session_id) = record.session_id {
            session_id = record_session_id;
        }
        if let Some(cwd) = record.cwd {
            if !working_directories.contains(&cwd) {
                working_directories.push(cwd);
            }
        }
        if let Some(timestamp) = record.timestamp.and_then(|timestamp| {
            chrono::DateTime::parse_from_rfc3339(&timestamp)
                .ok()
                .map(|timestamp| {
                    UNIX_EPOCH + Duration::from_millis(timestamp.timestamp_millis().max(0) as u64)
                })
        }) {
            last_activity_at = last_activity_at.max(timestamp);
        }
        if title.is_none() && record.is_meta != Some(true) {
            if let Some(message) = record.message {
                if message.role.as_deref() == Some("user") {
                    title = message_content_title(&message.content);
                }
            }
        }
    }

    let (last_activity_secs, last_activity_nanos) = last_activity_at
        .duration_since(UNIX_EPOCH)
        .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
        .unwrap_or((0, 0));
    Some(StoredSummary {
        session_id,
        title,
        last_activity_secs,
        last_activity_nanos,
        working_directories,
    })
}

/// Top-level record `type`s that are known Claude metadata, not conversation.
/// Whitelisting `user`/`assistant` and mapping these to noise keeps the ~dozen
/// non-conversational record types from leaking into a handoff while still
/// flagging genuinely unknown (drifted) types as degraded.
const CLAUDE_METADATA_TYPES: &[&str] = &[
    "system",
    "attachment",
    "last-prompt",
    "mode",
    "ai-title",
    "permission-mode",
    "queue-operation",
    "summary",
];

/// Classifies a top-level Claude transcript file into the shared event stream.
///
/// Only `user`/`assistant` records carry conversation. `user` records that are
/// solely `tool_result` blocks (the majority in a tool-heavy session) classify
/// as tool results, not user text; assistant `thinking` blocks are excluded;
/// `file-history-*` records are preserved as checkpoints. Subagent files are
/// never opened by the caller, so sidechain records are not expected here.
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
        let record_type = record.get("type").and_then(Value::as_str);
        match record_type {
            Some("user") => classify_user_record(&record, &mut classified),
            Some("assistant") => classify_assistant_record(&record, &mut classified),
            Some("file-history-snapshot") | Some("file-history-delta") => classified
                .events
                .push(RawEvent::Checkpoint("file history".to_string())),
            Some(other) if CLAUDE_METADATA_TYPES.contains(&other) => {
                classified.events.push(RawEvent::Noise)
            }
            _ => classified.unknown_count += 1,
        }
    }
    classified
}

fn classify_user_record(record: &Value, classified: &mut Classified) {
    // Synthetic command-driven user records are not real prompts.
    if record.get("isMeta").and_then(Value::as_bool) == Some(true) {
        classified.events.push(RawEvent::Noise);
        return;
    }
    let Some(content) = record
        .get("message")
        .and_then(|message| message.get("content"))
    else {
        classified.malformed_count += 1;
        return;
    };
    if let Some(text) = content.as_str() {
        if is_excluded_user_text(text) {
            classified.events.push(RawEvent::Noise);
        } else {
            classified.events.push(RawEvent::User(one_line(text)));
        }
        return;
    }
    let Some(blocks) = content.as_array() else {
        classified.malformed_count += 1;
        return;
    };
    let mut user_text = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("tool_result") => classified.events.push(RawEvent::ToolResult {
                call_id: block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                output: tool_result_text(block.get("content")),
                is_error: block.get("is_error").and_then(Value::as_bool) == Some(true),
            }),
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    user_text.push(text.to_string());
                }
            }
            _ => {}
        }
    }
    let joined = user_text.join("\n");
    if !joined.trim().is_empty() && !is_excluded_user_text(&joined) {
        classified.events.push(RawEvent::User(joined));
    }
}

fn classify_assistant_record(record: &Value, classified: &mut Classified) {
    let Some(content) = record
        .get("message")
        .and_then(|message| message.get("content"))
    else {
        classified.malformed_count += 1;
        return;
    };
    if let Some(text) = content.as_str() {
        classified
            .events
            .push(RawEvent::Assistant(text.to_string()));
        return;
    }
    let Some(blocks) = content.as_array() else {
        classified.malformed_count += 1;
        return;
    };
    // Flush buffered assistant text before each tool call so ordering (text then
    // the call it introduces) is preserved.
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
            Some("tool_use") => {
                flush(&mut pending_text, classified);
                classified.events.push(RawEvent::ToolCall {
                    call_id: block.get("id").and_then(Value::as_str).map(str::to_string),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    detail: summarize_tool_input(block.get("input")),
                });
            }
            // Thinking/redacted-thinking are known and excluded.
            Some("thinking") | Some("redacted_thinking") | Some("image") => {
                classified.events.push(RawEvent::Noise)
            }
            _ => classified.unknown_count += 1,
        }
    }
    flush(&mut pending_text, classified);
}

/// A `tool_result` block's `content` is a string or an array of `{type:text}`
/// (and other) blocks; join the text.
fn tool_result_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(blocks) = content.as_array() {
        return blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
    }
    content.to_string()
}

fn is_excluded_user_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with('/') || is_local_command_artifact(trimmed)
}

fn message_content_title(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return normalize_title(text);
    }
    let parts = content
        .as_array()?
        .iter()
        .filter_map(|item| {
            item.get("text")
                .or_else(|| item.get("content"))
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>()
        .join(" ");
    normalize_title(&parts)
}

fn project_history_dir_name(project_root: &str) -> String {
    let project_root = project_root.replace('\\', "/");
    project_root
        .chars()
        .map(|character| match character {
            '/' | '\\' => '-',
            _ => character,
        })
        .collect()
}

/// Prefixes of the synthetic user messages Claude Code records when a session is
/// driven by a local/slash command. These are not real prompts and must not be
/// used as a thread title.
const LOCAL_COMMAND_ARTIFACT_PREFIXES: &[&str] = &["<local-command-", "<command-", "<bash-"];

fn is_local_command_artifact(title: &str) -> bool {
    LOCAL_COMMAND_ARTIFACT_PREFIXES
        .iter()
        .any(|prefix| title.starts_with(prefix))
}

fn normalize_title(display: &str) -> Option<String> {
    let title = display.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() || title.starts_with('/') || is_local_command_artifact(&title) {
        None
    } else if title.chars().count() <= MAX_TITLE_CHARS {
        Some(title)
    } else {
        Some(format!(
            "{}…",
            title.chars().take(MAX_TITLE_CHARS).collect::<String>()
        ))
    }
}

#[derive(Clone, Deserialize)]
struct HistoryRecord {
    #[serde(rename = "sessionId")]
    session_id: String,
    project: String,
    display: String,
    timestamp: u64,
}

#[derive(Deserialize)]
struct ProjectHistoryLine {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    message: Option<ProjectHistoryMessage>,
}

#[derive(Deserialize)]
struct ProjectHistoryMessage {
    role: Option<String>,
    content: Value,
}

#[cfg(test)]
mod transcript_tests {
    use super::*;
    use crate::transcript::RawEvent;

    fn user(content: Value) -> String {
        serde_json::json!({ "type": "user", "message": { "role": "user", "content": content } })
            .to_string()
    }
    fn assistant(content: Value) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": { "role": "assistant", "content": content }
        })
        .to_string()
    }

    #[test]
    fn tool_result_only_user_row_is_a_result_not_user_text() {
        let content = user(serde_json::json!([
            { "type": "tool_result", "tool_use_id": "toolu_1", "content": "command output" }
        ]));
        let classified = classify_transcript(&content);
        assert_eq!(
            classified.events,
            vec![RawEvent::ToolResult {
                call_id: Some("toolu_1".to_string()),
                output: "command output".to_string(),
                is_error: false,
            }]
        );
    }

    #[test]
    fn assistant_thinking_excluded_text_and_tool_use_ordered() {
        let content = assistant(serde_json::json!([
            { "type": "thinking", "thinking": "secret reasoning" },
            { "type": "text", "text": "I'll run the tests" },
            { "type": "tool_use", "id": "toolu_2", "name": "Bash", "input": { "command": "cargo test" } }
        ]));
        let classified = classify_transcript(&content);
        assert_eq!(
            classified.events,
            vec![
                RawEvent::Noise,
                RawEvent::Assistant("I'll run the tests".to_string()),
                RawEvent::ToolCall {
                    call_id: Some("toolu_2".to_string()),
                    name: "Bash".to_string(),
                    detail: "cargo test".to_string(),
                },
            ]
        );
    }

    #[test]
    fn plain_user_text_and_string_content_classified() {
        let content = [
            user(serde_json::json!("switch to main and pull")),
            user(serde_json::json!([{ "type": "text", "text": "then run tests" }])),
        ]
        .join("\n");
        let classified = classify_transcript(&content);
        assert_eq!(
            classified.events,
            vec![
                RawEvent::User("switch to main and pull".to_string()),
                RawEvent::User("then run tests".to_string()),
            ]
        );
    }

    #[test]
    fn synthetic_and_slash_user_records_are_excluded() {
        let content = [
            serde_json::json!({
                "type": "user", "isMeta": true,
                "message": { "role": "user", "content": "<local-command-caveat>x</local-command-caveat>" }
            })
            .to_string(),
            user(serde_json::json!("/clear")),
            user(serde_json::json!("<command-name>/tui</command-name>")),
        ]
        .join("\n");
        let classified = classify_transcript(&content);
        assert_eq!(classified.events, vec![RawEvent::Noise; 3]);
    }

    #[test]
    fn metadata_types_are_noise_and_file_history_is_checkpoint() {
        let content = [
            serde_json::json!({ "type": "system" }).to_string(),
            serde_json::json!({ "type": "ai-title", "title": "x" }).to_string(),
            serde_json::json!({ "type": "file-history-snapshot" }).to_string(),
            serde_json::json!({ "type": "brand-new-type" }).to_string(),
        ]
        .join("\n");
        let classified = classify_transcript(&content);
        assert_eq!(
            classified.events,
            vec![
                RawEvent::Noise,
                RawEvent::Noise,
                RawEvent::Checkpoint("file history".to_string()),
            ]
        );
        assert_eq!(classified.unknown_count, 1);
    }
}
