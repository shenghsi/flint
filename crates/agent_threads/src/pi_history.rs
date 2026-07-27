use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use collections::HashMap;
use gpui::SharedString;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AgentLaunchCommand;
use crate::history::{
    AgentHistoryHost, AgentHistoryProvider, HistoricalThread, paths_equal_for_style,
};

const MAX_PROJECT_HISTORY_FILES_SCANNED: usize = 200;
const MAX_TITLE_CHARS: usize = 60;

pub struct PiHistoryProvider;

#[derive(Clone, Serialize, Deserialize)]
struct PiSessionSummary {
    session_id: String,
    title: String,
    project_root: PathBuf,
    last_activity_at: SystemTime,
}

#[async_trait]
impl AgentHistoryProvider for PiHistoryProvider {
    async fn scan(
        &self,
        host: &AgentHistoryHost,
        project_roots: &[PathBuf],
    ) -> Result<Vec<HistoricalThread>> {
        let sessions_directory = host.join(&host.base_dir, "sessions")?;
        let mut latest_by_session = HashMap::<String, HistoricalThread>::default();
        for project_root in project_roots {
            let project_directory =
                host.join(&sessions_directory, encoded_project_directory(project_root))?;
            for file_path in recent_session_files(host, &project_directory).await {
                let Some(summary) = read_session_summary(host, &file_path).await else {
                    continue;
                };
                if !paths_equal_for_style(&summary.project_root, project_root, host.path_style) {
                    continue;
                }
                let thread = HistoricalThread {
                    session_id: SharedString::from(summary.session_id.clone()),
                    title: SharedString::from(summary.title),
                    project_root: summary.project_root,
                    last_activity_at: summary.last_activity_at,
                };
                let is_newer = latest_by_session
                    .get(&summary.session_id)
                    .is_none_or(|existing| thread.last_activity_at >= existing.last_activity_at);
                if is_newer {
                    latest_by_session.insert(summary.session_id, thread);
                }
            }
        }
        let mut threads = latest_by_session.into_values().collect::<Vec<_>>();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_activity_at));
        Ok(threads)
    }

    fn resume_command(
        &self,
        base: &AgentLaunchCommand,
        thread: &HistoricalThread,
        extra_args: &[String],
    ) -> AgentLaunchCommand {
        let mut args = vec!["--session".to_string(), thread.session_id.to_string()];
        args.extend(extra_args.iter().cloned());
        AgentLaunchCommand {
            command: base.command.clone(),
            args,
            env: base.env.clone(),
            cwd: Some(thread.project_root.clone()),
            initialization_command: base.initialization_command.clone(),
            hidden: base.hidden,
            default_launch_option: base.default_launch_option.clone(),
        }
    }
}

fn encoded_project_directory(project_root: &Path) -> String {
    let project_root = project_root.to_string_lossy();
    let project_root = project_root
        .strip_prefix('/')
        .or_else(|| project_root.strip_prefix('\\'))
        .unwrap_or(&project_root);
    let encoded = project_root.replace(['/', '\\', ':'], "-");
    format!("--{encoded}--")
}

async fn recent_session_files(host: &AgentHistoryHost, directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = host.fs.read_dir(directory).await else {
        return Vec::new();
    };
    let mut files = entries
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect::<Vec<_>>();
    files.sort_unstable_by(|left, right| right.cmp(left));
    files.truncate(MAX_PROJECT_HISTORY_FILES_SCANNED);
    files
}

async fn read_session_summary(
    host: &AgentHistoryHost,
    file_path: &Path,
) -> Option<PiSessionSummary> {
    let summary = host
        .parse_file_persistent(file_path, |content| parse_session_summary(content))
        .await
        .ok()?;
    summary.as_ref().clone()
}

fn parse_session_summary(content: &str) -> Option<PiSessionSummary> {
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
                project_root = entry.get("cwd").and_then(Value::as_str).map(PathBuf::from);
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
    Some(PiSessionSummary {
        session_id,
        title: session_name
            .or(first_user_message)
            .unwrap_or_else(|| "Pi session".to_string()),
        project_root,
        last_activity_at: last_activity_at.max(header_timestamp),
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    use fs::Fs as _;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use project::FakeFs;
    use util::paths::PathStyle;

    use crate::history::{HistoryParseCache, LocalHistoryFs};

    use super::*;

    #[gpui::test]
    async fn scan_discovers_valid_session_metadata(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let session_directory = PathBuf::from("/pi-home/sessions/--root--");
        fs.create_dir(&session_directory)
            .await
            .expect("create session dir");
        let content = [
            serde_json::json!({
                "type": "session",
                "version": 3,
                "id": "session-a",
                "timestamp": "2026-07-22T01:00:00Z",
                "cwd": "/root"
            }),
            serde_json::json!({
                "type": "message",
                "id": "message-a",
                "parentId": null,
                "timestamp": "2026-07-22T01:01:00Z",
                "message": { "role": "user", "content": "Fix the parser" }
            }),
            serde_json::json!({
                "type": "session_info",
                "id": "info-a",
                "parentId": "message-a",
                "timestamp": "2026-07-22T01:02:00Z",
                "name": "Parser repair"
            }),
        ]
        .map(|entry| entry.to_string())
        .join("\n");
        fs.insert_file(
            session_directory.join("session.jsonl"),
            content.into_bytes(),
        )
        .await;
        let host = AgentHistoryHost {
            fs: Arc::new(LocalHistoryFs(fs)),
            base_dir: PathBuf::from("/pi-home"),
            cache: Arc::new(HistoryParseCache::default()),
            path_style: PathStyle::Posix,
        };

        let threads = PiHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .expect("scan Pi history");

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].session_id.as_ref(), "session-a");
        assert_eq!(threads[0].title.as_ref(), "Parser repair");
        assert_eq!(threads[0].project_root, PathBuf::from("/root"));
        assert_eq!(
            threads[0].last_activity_at,
            chrono::DateTime::parse_from_rfc3339("2026-07-22T01:02:00Z")
                .map(|timestamp| {
                    UNIX_EPOCH + Duration::from_millis(timestamp.timestamp_millis() as u64)
                })
                .expect("valid timestamp")
        );
    }

    #[test]
    fn resume_uses_session_id_and_original_project_root() {
        let base = AgentLaunchCommand {
            command: Some("custom-pi".to_string()),
            args: vec!["--new-only".to_string()],
            env: [("PI_CODING_AGENT_DIR".to_string(), "/pi-home".to_string())]
                .into_iter()
                .collect(),
            initialization_command: Some("source ~/.profile".to_string()),
            hidden: true,
            ..Default::default()
        };
        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("Pi session"),
            project_root: PathBuf::from("/root"),
            last_activity_at: UNIX_EPOCH,
        };

        let command = PiHistoryProvider.resume_command(
            &base,
            &thread,
            &["--extension".to_string(), "review.ts".to_string()],
        );

        assert_eq!(command.command.as_deref(), Some("custom-pi"));
        assert_eq!(
            command.args,
            ["--session", "session-a", "--extension", "review.ts"]
        );
        assert_eq!(command.cwd.as_deref(), Some(Path::new("/root")));
        assert_eq!(
            command.env.get("PI_CODING_AGENT_DIR").map(String::as_str),
            Some("/pi-home")
        );
        assert_eq!(
            command.initialization_command.as_deref(),
            Some("source ~/.profile")
        );
        assert!(command.hidden);
    }

    #[test]
    fn parser_skips_malformed_lines_and_uses_user_text_blocks() {
        let content = [
            "not json".to_string(),
            serde_json::json!({
                "type": "session",
                "id": "session-a",
                "timestamp": "2026-07-22T01:00:00Z",
                "cwd": "/root"
            })
            .to_string(),
            serde_json::json!({
                "type": "message",
                "timestamp": "2026-07-22T01:01:00Z",
                "message": {
                    "role": "user",
                    "content": [
                        { "type": "image", "data": "ignored" },
                        { "type": "text", "text": "Fix" },
                        { "type": "text", "text": "the parser" }
                    ]
                }
            })
            .to_string(),
        ]
        .join("\n");

        let summary = parse_session_summary(&content).expect("valid entries should be parsed");

        assert_eq!(summary.title, "Fix the parser");
    }

    #[test]
    fn project_directory_encoding_matches_pi_on_windows() {
        assert_eq!(
            encoded_project_directory(Path::new(r"C:\Users\dev\project")),
            "--C--Users-dev-project--"
        );
    }
}
