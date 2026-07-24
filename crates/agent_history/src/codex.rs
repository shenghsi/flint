use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use collections::HashSet;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
