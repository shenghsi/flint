use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
