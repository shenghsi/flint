use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use async_trait::async_trait;
use collections::{HashMap, HashSet};
use gpui::SharedString;
use serde::Deserialize;
use serde_json::Value;

use crate::AgentLaunchCommand;
#[cfg(test)]
use crate::history::LocalHistoryFs;
use crate::history::{AgentHistoryHost, AgentHistoryProvider, HistoricalThread};

/// Bounds the cost of matching sessions to projects regardless of total
/// history depth: only the most recent rollout files are opened to read
/// their `cwd`.
const MAX_ROLLOUT_FILES_SCANNED: usize = 200;
const MAX_TITLE_CHARS: usize = 60;

pub struct CodexHistoryProvider;

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

#[async_trait]
impl AgentHistoryProvider for CodexHistoryProvider {
    async fn scan(
        &self,
        host: &AgentHistoryHost,
        project_roots: &[PathBuf],
    ) -> Result<Vec<HistoricalThread>> {
        let sessions_dir = host.base_dir.join("sessions");
        let mut rollout_files = list_rollout_files(host, &sessions_dir).await;
        // Filenames embed an ISO timestamp (rollout-YYYY-MM-DDTHH-MM-SS-<id>.jsonl),
        // so descending lexical order is descending chronological order.
        rollout_files.sort_unstable_by(|left, right| right.cmp(left));
        rollout_files.truncate(MAX_ROLLOUT_FILES_SCANNED);

        let mut titles = load_session_titles(host).await;
        for (session_id, title) in load_history_titles(host).await {
            titles.entry(session_id).or_insert(title);
        }

        let mut threads = Vec::new();
        for file_path in rollout_files {
            let Some((id, cwd, timestamp)) = read_session_meta(host, &file_path).await else {
                continue;
            };
            let project_root = PathBuf::from(&cwd);
            if !project_roots.iter().any(|root| root == &project_root) {
                continue;
            }
            let title = if let Some(title) = titles.get(&id).cloned() {
                title
            } else {
                read_session_title(host, &file_path)
                    .await
                    .unwrap_or_else(|| "Codex session".to_string())
            };
            threads.push(HistoricalThread {
                session_id: SharedString::from(id),
                title: SharedString::from(title),
                project_root,
                last_activity_at: timestamp,
            });
        }
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_activity_at));
        Ok(threads)
    }

    fn resume_command(
        &self,
        base: &AgentLaunchCommand,
        thread: &HistoricalThread,
        extra_args: &[String],
    ) -> AgentLaunchCommand {
        let mut args = vec!["resume".to_string(), thread.session_id.to_string()];
        args.extend(extra_args.iter().cloned());
        AgentLaunchCommand {
            command: base.command.clone(),
            args,
            env: base.env.clone(),
            cwd: Some(thread.project_root.clone()),
            hidden: base.hidden,
            default_launch_option: base.default_launch_option.clone(),
        }
    }
}

async fn list_rollout_files(host: &AgentHistoryHost, sessions_dir: &Path) -> Vec<PathBuf> {
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

async fn list_dir(host: &AgentHistoryHost, dir: &Path) -> Vec<PathBuf> {
    let Ok(paths) = host.fs.read_dir(dir).await else {
        return Vec::new();
    };
    paths
}

async fn read_session_meta(
    host: &AgentHistoryHost,
    file_path: &Path,
) -> Option<(String, String, SystemTime)> {
    let reader = host.fs.open_sync(file_path).await.ok()?;
    let mut buf_reader = std::io::BufReader::new(reader);
    let mut first_line = String::new();
    buf_reader.read_line(&mut first_line).ok()?;

    let parsed: SessionMetaLine = serde_json::from_str(first_line.trim()).ok()?;
    if parsed.kind != "session_meta" {
        return None;
    }
    let timestamp = chrono::DateTime::parse_from_rfc3339(&parsed.payload.timestamp)
        .map(|dt| {
            SystemTime::UNIX_EPOCH
                + std::time::Duration::from_millis(dt.timestamp_millis().max(0) as u64)
        })
        .unwrap_or(SystemTime::UNIX_EPOCH);
    Some((parsed.payload.id, parsed.payload.cwd, timestamp))
}

async fn load_session_titles(host: &AgentHistoryHost) -> HashMap<String, String> {
    let index_path = host.base_dir.join("session_index.jsonl");
    let Ok(content) = host.fs.load(&index_path).await else {
        return HashMap::default();
    };
    let mut titles = HashMap::default();
    for line in content.lines() {
        if let Ok(entry) = serde_json::from_str::<SessionIndexEntry>(line) {
            if let Some(thread_name) = entry.thread_name.and_then(normalize_title) {
                titles.insert(entry.id, thread_name);
            }
        }
    }
    titles
}

async fn load_history_titles(host: &AgentHistoryHost) -> HashMap<String, String> {
    let history_path = host.base_dir.join("history.jsonl");
    let Ok(content) = host.fs.load(&history_path).await else {
        return HashMap::default();
    };
    let mut latest_titles: HashMap<String, (u64, String)> = HashMap::default();
    for line in content.lines() {
        let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) else {
            continue;
        };
        let Some(title) = normalize_title(entry.text) else {
            continue;
        };
        let existing_is_newer = latest_titles
            .get(&entry.session_id)
            .is_some_and(|(timestamp, _)| *timestamp > entry.ts);
        if !existing_is_newer {
            latest_titles.insert(entry.session_id, (entry.ts, title));
        }
    }
    latest_titles
        .into_iter()
        .map(|(session_id, (_, title))| (session_id, title))
        .collect()
}

async fn read_session_title(host: &AgentHistoryHost, file_path: &Path) -> Option<String> {
    let reader = host.fs.open_sync(file_path).await.ok()?;
    let buf_reader = std::io::BufReader::new(reader);
    let mut seen_user_messages = HashSet::default();
    for line in buf_reader.lines().map_while(|line| line.ok()) {
        let Ok(parsed) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(payload) = parsed.get("payload") else {
            continue;
        };
        if let Some(title) = user_message_title(payload, &mut seen_user_messages) {
            return Some(title);
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use fs::Fs as _;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use project::FakeFs;
    use std::sync::Arc;
    use std::time::Duration;

    fn session_meta_line(id: &str, cwd: &str, timestamp: &str) -> String {
        serde_json::json!({
            "timestamp": timestamp,
            "type": "session_meta",
            "payload": {
                "id": id,
                "timestamp": timestamp,
                "cwd": cwd,
                "originator": "codex_exec",
            },
        })
        .to_string()
    }

    fn session_index_line(id: &str, thread_name: &str) -> String {
        serde_json::json!({
            "id": id,
            "thread_name": thread_name,
            "updated_at": "2026-06-18T12:00:00.000Z",
        })
        .to_string()
    }

    fn history_line(id: &str, text: &str, timestamp: u64) -> String {
        serde_json::json!({
            "session_id": id,
            "text": text,
            "ts": timestamp,
        })
        .to_string()
    }

    fn user_message_line(message: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-06-18T20:23:15.000Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": message,
            },
        })
        .to_string()
    }

    async fn host_with_fixture(
        cx: &TestAppContext,
        rollout_files: &[(&str, String)],
        session_index: Option<&str>,
        history: Option<&str>,
    ) -> AgentHistoryHost {
        let fs = FakeFs::new(cx.executor());
        for (relative_path, content) in rollout_files {
            let full_path = PathBuf::from("/codex-home/sessions").join(relative_path);
            fs.create_dir(full_path.parent().unwrap()).await.unwrap();
            fs.insert_file(&full_path, content.as_bytes().to_vec())
                .await;
        }
        fs.create_dir(Path::new("/codex-home")).await.unwrap();
        if let Some(index_content) = session_index {
            fs.insert_file(
                "/codex-home/session_index.jsonl",
                index_content.as_bytes().to_vec(),
            )
            .await;
        }
        if let Some(history_content) = history {
            fs.insert_file(
                "/codex-home/history.jsonl",
                history_content.as_bytes().to_vec(),
            )
            .await;
        }
        AgentHistoryHost {
            fs: Arc::new(LocalHistoryFs(fs)),
            base_dir: PathBuf::from("/codex-home"),
        }
    }

    #[gpui::test]
    async fn scan_joins_session_meta_with_title_index(cx: &mut TestAppContext) {
        let rollout = session_meta_line("session-a", "/root", "2026-06-18T20:23:14.000Z");
        let host = host_with_fixture(
            cx,
            &[(
                "2026/06/18/rollout-2026-06-18T20-23-14-session-a.jsonl",
                rollout,
            )],
            Some(&session_index_line("session-a", "Fix the bug")),
            None,
        )
        .await;

        let threads = CodexHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].session_id.as_ref(), "session-a");
        assert_eq!(threads[0].title.as_ref(), "Fix the bug");
        assert_eq!(threads[0].project_root, PathBuf::from("/root"));
        assert_eq!(
            threads[0].last_activity_at,
            chrono::DateTime::parse_from_rfc3339("2026-06-18T20:23:14.000Z")
                .map(|dt| SystemTime::UNIX_EPOCH
                    + Duration::from_millis(dt.timestamp_millis() as u64))
                .unwrap()
        );
    }

    #[gpui::test]
    async fn scan_falls_back_to_history_title_when_missing_from_index(cx: &mut TestAppContext) {
        let rollout = session_meta_line("session-a", "/root", "2026-06-18T20:23:14.000Z");
        let host = host_with_fixture(
            cx,
            &[(
                "2026/06/18/rollout-2026-06-18T20-23-14-session-a.jsonl",
                rollout,
            )],
            None,
            Some(&history_line(
                "session-a",
                "Implement agent threads panel",
                200,
            )),
        )
        .await;

        let threads = CodexHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title.as_ref(), "Implement agent threads panel");
    }

    #[gpui::test]
    async fn scan_falls_back_to_rollout_user_message_when_indexes_are_missing(
        cx: &mut TestAppContext,
    ) {
        let rollout = [
            session_meta_line("session-a", "/root", "2026-06-18T20:23:14.000Z"),
            user_message_line("Summarize this agent session"),
        ]
        .join("\n");
        let host = host_with_fixture(
            cx,
            &[(
                "2026/06/18/rollout-2026-06-18T20-23-14-session-a.jsonl",
                rollout,
            )],
            None,
            None,
        )
        .await;

        let threads = CodexHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title.as_ref(), "Summarize this agent session");
    }

    #[gpui::test]
    async fn scan_filters_by_cwd(cx: &mut TestAppContext) {
        let rollout_a = session_meta_line("session-a", "/root_a", "2026-06-18T20:23:14.000Z");
        let rollout_b = session_meta_line("session-b", "/root_b", "2026-06-18T20:24:14.000Z");
        let host = host_with_fixture(
            cx,
            &[
                (
                    "2026/06/18/rollout-2026-06-18T20-23-14-session-a.jsonl",
                    rollout_a,
                ),
                (
                    "2026/06/18/rollout-2026-06-18T20-24-14-session-b.jsonl",
                    rollout_b,
                ),
            ],
            None,
            None,
        )
        .await;

        let threads = CodexHistoryProvider
            .scan(&host, &[PathBuf::from("/root_a")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].session_id.as_ref(), "session-a");
    }

    #[gpui::test]
    async fn scan_orders_most_recent_rollout_first(cx: &mut TestAppContext) {
        let older = session_meta_line("session-older", "/root", "2026-06-18T20:23:14.000Z");
        let newer = session_meta_line("session-newer", "/root", "2026-06-18T21:00:00.000Z");
        let host = host_with_fixture(
            cx,
            &[
                (
                    "2026/06/18/rollout-2026-06-18T20-23-14-session-older.jsonl",
                    older,
                ),
                (
                    "2026/06/18/rollout-2026-06-18T21-00-00-session-newer.jsonl",
                    newer,
                ),
            ],
            None,
            None,
        )
        .await;

        let threads = CodexHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].session_id.as_ref(), "session-newer");
        assert_eq!(threads[1].session_id.as_ref(), "session-older");
    }

    #[gpui::test]
    async fn scan_skips_files_with_no_session_meta_first_line(cx: &mut TestAppContext) {
        let host = host_with_fixture(
            cx,
            &[(
                "2026/06/18/rollout-2026-06-18T20-23-14-bad.jsonl",
                "not json".to_string(),
            )],
            None,
            None,
        )
        .await;

        let threads = CodexHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert!(threads.is_empty());
    }

    #[gpui::test]
    async fn scan_returns_empty_when_sessions_dir_missing(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/codex-home")).await.unwrap();
        let host = AgentHistoryHost {
            fs: Arc::new(LocalHistoryFs(fs)),
            base_dir: PathBuf::from("/codex-home"),
        };

        let threads = CodexHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert!(threads.is_empty());
    }

    #[test]
    fn resume_command_uses_resume_subcommand_and_session_id() {
        let base = AgentLaunchCommand {
            command: Some("codex".to_string()),
            args: vec!["--ignored-fresh-session-arg".to_string()],
            env: HashMap::default(),
            cwd: None,
            hidden: false,
            default_launch_option: None,
        };
        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("title"),
            project_root: PathBuf::from("/root"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };

        let resumed = CodexHistoryProvider.resume_command(&base, &thread, &[]);

        assert_eq!(resumed.command, Some("codex".to_string()));
        assert_eq!(resumed.args, vec!["resume", "session-a"]);
        assert_eq!(resumed.cwd, Some(PathBuf::from("/root")));
    }

    #[test]
    fn resume_command_appends_extra_args_for_resume_options() {
        let base = AgentLaunchCommand::default();
        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("title"),
            project_root: PathBuf::from("/root"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };

        let resumed = CodexHistoryProvider.resume_command(
            &base,
            &thread,
            &["--dangerously-bypass-approvals-and-sandbox".to_string()],
        );

        assert_eq!(
            resumed.args,
            vec![
                "resume",
                "session-a",
                "--dangerously-bypass-approvals-and-sandbox"
            ]
        );
    }
}
