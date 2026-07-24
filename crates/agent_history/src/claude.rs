use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    }
    // Keyed by (session id, normalized working directory).
    let mut candidates: BTreeMap<(String, String), Candidate> = BTreeMap::new();
    let mut consider = |session_id: String,
                        working_dir: &str,
                        title: String,
                        activity: SystemTime,
                        from_project_file: bool| {
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
            );
        }
    }

    for entry in project_files.values() {
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
            IndexedSession {
                session_id: candidate.session_id,
                resolved_title: candidate.title,
                fallback_title: None,
                working_dir: candidate.working_dir,
                last_activity_secs,
                last_activity_nanos,
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
