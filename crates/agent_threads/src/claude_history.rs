use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use collections::HashMap;
use gpui::SharedString;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AgentLaunchCommand;
#[cfg(test)]
use crate::history::LocalHistoryFs;
use crate::history::{
    AgentHistoryHost, AgentHistoryProvider, HistoricalThread, paths_equal_for_style,
};
#[cfg(test)]
use util::paths::PathStyle;

const MAX_TITLE_CHARS: usize = 60;
const MAX_PROJECT_HISTORY_FILES_SCANNED: usize = 200;

pub struct ClaudeHistoryProvider;

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

#[derive(Serialize, Deserialize)]
struct ProjectHistorySummary {
    session_id: String,
    title: Option<String>,
    last_activity_at: std::time::SystemTime,
    working_directories: Vec<PathBuf>,
}

#[async_trait]
impl AgentHistoryProvider for ClaudeHistoryProvider {
    async fn scan(
        &self,
        host: &AgentHistoryHost,
        project_roots: &[PathBuf],
    ) -> Result<Vec<HistoricalThread>> {
        let mut latest_by_session = scan_history_file(host, project_roots).await;
        for thread in scan_project_history_files(host, project_roots).await {
            let session_id = thread.session_id.to_string();
            let is_newer = latest_by_session
                .get(&session_id)
                .is_none_or(|existing| thread.last_activity_at >= existing.last_activity_at);
            if is_newer {
                latest_by_session.insert(session_id, thread);
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
        let mut args = vec!["--resume".to_string(), thread.session_id.to_string()];
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

async fn scan_history_file(
    host: &AgentHistoryHost,
    project_roots: &[PathBuf],
) -> HashMap<String, HistoricalThread> {
    let Ok(history_path) = host.join(&host.base_dir, "history.jsonl") else {
        return HashMap::default();
    };
    let Ok(records) = host
        .parse_file(&history_path, |content| {
            content
                .lines()
                .filter_map(|line| serde_json::from_str::<HistoryRecord>(line).ok())
                .collect::<Vec<_>>()
        })
        .await
    else {
        return HashMap::default();
    };

    let mut latest_by_session = HashMap::default();
    for record in records.iter() {
        let project_root = PathBuf::from(&record.project);
        if !project_roots
            .iter()
            .any(|root| paths_equal_for_style(root, &project_root, host.path_style))
        {
            continue;
        }
        let Some(title) = normalize_title(&record.display) else {
            continue;
        };
        let thread = HistoricalThread {
            session_id: SharedString::from(record.session_id.clone()),
            title: SharedString::from(title),
            project_root,
            last_activity_at: UNIX_EPOCH + Duration::from_millis(record.timestamp),
        };
        let is_newer =
            latest_by_session
                .get(&record.session_id)
                .is_none_or(|existing: &HistoricalThread| {
                    thread.last_activity_at >= existing.last_activity_at
                });
        if is_newer {
            latest_by_session.insert(record.session_id.clone(), thread);
        }
    }
    latest_by_session
}

async fn scan_project_history_files(
    host: &AgentHistoryHost,
    project_roots: &[PathBuf],
) -> Vec<HistoricalThread> {
    let mut threads = Vec::new();
    for project_root in project_roots {
        let Ok(projects_dir) = host.join(&host.base_dir, "projects") else {
            continue;
        };
        let Ok(project_dir) = host.join(projects_dir, project_history_dir_name(project_root))
        else {
            continue;
        };
        for file_path in list_jsonl_files(host, &project_dir).await {
            if let Some(thread) = read_project_history_file(host, &file_path, project_root).await {
                threads.push(thread);
            }
        }
    }
    threads
}

async fn list_jsonl_files(host: &AgentHistoryHost, dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = host.fs.read_dir(dir).await else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for path in entries {
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            paths.push(path);
        }
    }
    paths.sort_unstable_by(|left, right| right.cmp(left));
    paths.truncate(MAX_PROJECT_HISTORY_FILES_SCANNED);
    paths
}

async fn read_project_history_file(
    host: &AgentHistoryHost,
    file_path: &Path,
    project_root: &Path,
) -> Option<HistoricalThread> {
    let default_session_id = file_path.file_stem()?.to_string_lossy().to_string();
    let summary = host
        .parse_file_persistent(file_path, move |content| {
            let mut session_id = default_session_id;
            let mut title = None;
            let mut last_activity_at = UNIX_EPOCH;
            let mut working_directories = Vec::new();

            for line in content.lines() {
                let Ok(record) = serde_json::from_str::<ProjectHistoryLine>(line) else {
                    continue;
                };
                if let Some(record_session_id) = record.session_id {
                    session_id = record_session_id;
                }
                if let Some(cwd) = record.cwd {
                    let cwd = PathBuf::from(cwd);
                    if !working_directories.contains(&cwd) {
                        working_directories.push(cwd);
                    }
                }
                if let Some(timestamp) = record.timestamp.and_then(|timestamp| {
                    chrono::DateTime::parse_from_rfc3339(&timestamp)
                        .ok()
                        .map(|timestamp| {
                            UNIX_EPOCH
                                + Duration::from_millis(timestamp.timestamp_millis().max(0) as u64)
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

            ProjectHistorySummary {
                session_id,
                title,
                last_activity_at,
                working_directories,
            }
        })
        .await
        .ok()?;

    if !summary
        .working_directories
        .iter()
        .any(|cwd| paths_equal_for_style(cwd, project_root, host.path_style))
    {
        return None;
    }

    Some(HistoricalThread {
        session_id: SharedString::from(summary.session_id.clone()),
        title: SharedString::from(summary.title.clone()?),
        project_root: project_root.to_path_buf(),
        last_activity_at: summary.last_activity_at,
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

fn project_history_dir_name(project_root: &Path) -> String {
    let project_root = project_root.to_string_lossy().replace('\\', "/");
    project_root
        .chars()
        .map(|character| match character {
            '/' | '\\' => '-',
            _ => character,
        })
        .collect()
}

/// Prefixes of the synthetic user messages Claude Code records when a session
/// is driven by a local/slash command (e.g. `<local-command-caveat>…` or
/// `<command-name>/clear</command-name>…`). These are not real prompts and must
/// not be used as a thread title.
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

#[cfg(test)]
mod tests {
    use super::*;
    use fs::Fs as _;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use project::FakeFs;
    use std::sync::Arc;

    fn history_line(session_id: &str, project: &str, display: &str, timestamp: u64) -> String {
        serde_json::json!({
            "sessionId": session_id,
            "project": project,
            "display": display,
            "timestamp": timestamp,
        })
        .to_string()
    }

    fn project_history_line(session_id: &str, cwd: &str, content: &str, timestamp: &str) -> String {
        serde_json::json!({
            "sessionId": session_id,
            "cwd": cwd,
            "timestamp": timestamp,
            "message": {
                "role": "user",
                "content": content,
            },
        })
        .to_string()
    }

    fn meta_project_history_line(
        session_id: &str,
        cwd: &str,
        content: &str,
        timestamp: &str,
    ) -> String {
        serde_json::json!({
            "sessionId": session_id,
            "cwd": cwd,
            "timestamp": timestamp,
            "isMeta": true,
            "message": {
                "role": "user",
                "content": content,
            },
        })
        .to_string()
    }

    async fn host_with_history(cx: &TestAppContext, content: &str) -> AgentHistoryHost {
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(std::path::Path::new("/claude-home"))
            .await
            .unwrap();
        fs.insert_file("/claude-home/history.jsonl", content.as_bytes().to_vec())
            .await;
        AgentHistoryHost {
            fs: Arc::new(LocalHistoryFs(fs)),
            base_dir: PathBuf::from("/claude-home"),
            cache: Arc::new(crate::history::HistoryParseCache::default()),
            path_style: PathStyle::Posix,
        }
    }

    async fn host_with_project_history(
        cx: &TestAppContext,
        project_root: &str,
        session_id: &str,
        content: &str,
    ) -> AgentHistoryHost {
        let fs = FakeFs::new(cx.executor());
        let project_dir = PathBuf::from("/claude-home")
            .join("projects")
            .join(project_root.replace('/', "-"));
        fs.create_dir(&project_dir).await.unwrap();
        fs.insert_file(
            project_dir.join(format!("{session_id}.jsonl")),
            content.as_bytes().to_vec(),
        )
        .await;
        AgentHistoryHost {
            fs: Arc::new(LocalHistoryFs(fs)),
            base_dir: PathBuf::from("/claude-home"),
            cache: Arc::new(crate::history::HistoryParseCache::default()),
            path_style: PathStyle::Posix,
        }
    }

    #[gpui::test]
    async fn scan_keeps_the_latest_entry_per_session(cx: &mut TestAppContext) {
        let content = [
            history_line("session-a", "/root", "first prompt", 100),
            history_line("session-a", "/root", "second prompt", 200),
        ]
        .join("\n");
        let host = host_with_history(cx, &content).await;

        let threads = ClaudeHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title.as_ref(), "second prompt");
        assert_eq!(
            threads[0].last_activity_at,
            UNIX_EPOCH + Duration::from_millis(200)
        );
    }

    #[gpui::test]
    async fn scan_filters_by_project_root(cx: &mut TestAppContext) {
        let content = [
            history_line("session-a", "/root_a", "in root a", 100),
            history_line("session-b", "/root_b", "in root b", 100),
        ]
        .join("\n");
        let host = host_with_history(cx, &content).await;

        let threads = ClaudeHistoryProvider
            .scan(&host, &[PathBuf::from("/root_a")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].session_id.as_ref(), "session-a");
    }

    #[gpui::test]
    async fn scan_matches_posix_remote_project_when_client_root_uses_windows_separators(
        cx: &mut TestAppContext,
    ) {
        let content = history_line("session-a", "/home/dev/project", "in remote project", 100);
        let host = host_with_history(cx, &content).await;

        let threads = ClaudeHistoryProvider
            .scan(&host, &[PathBuf::from("\\home\\dev\\project")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].session_id.as_ref(), "session-a");
        assert_eq!(threads[0].project_root, PathBuf::from("/home/dev/project"));
    }

    #[gpui::test]
    async fn scan_orders_history_file_sessions_newest_first(cx: &mut TestAppContext) {
        let content = [
            history_line("session-older", "/root", "older prompt", 100),
            history_line("session-newer", "/root", "newer prompt", 200),
        ]
        .join("\n");
        let host = host_with_history(cx, &content).await;

        let threads = ClaudeHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].session_id.as_ref(), "session-newer");
        assert_eq!(threads[1].session_id.as_ref(), "session-older");
    }

    #[gpui::test]
    async fn scan_reads_project_history_session_titles(cx: &mut TestAppContext) {
        let content = project_history_line(
            "session-a",
            "/root",
            "Explain the release crash",
            "2026-06-18T20:23:14.000Z",
        );
        let host = host_with_project_history(cx, "/root", "session-a", &content).await;

        let threads = ClaudeHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].session_id.as_ref(), "session-a");
        assert_eq!(threads[0].title.as_ref(), "Explain the release crash");
        assert_eq!(
            threads[0].last_activity_at,
            chrono::DateTime::parse_from_rfc3339("2026-06-18T20:23:14.000Z")
                .map(|timestamp| UNIX_EPOCH
                    + Duration::from_millis(timestamp.timestamp_millis() as u64))
                .unwrap()
        );
    }

    #[gpui::test]
    async fn scan_skips_slash_commands_when_deriving_project_history_title(
        cx: &mut TestAppContext,
    ) {
        let content = [
            project_history_line("session-a", "/root", "/clear", "2026-06-18T20:23:14.000Z"),
            project_history_line("session-a", "/root", "/exit", "2026-06-18T20:24:14.000Z"),
            project_history_line(
                "session-a",
                "/root",
                "Fix the release crash",
                "2026-06-18T20:25:14.000Z",
            ),
        ]
        .join("\n");
        let host = host_with_project_history(cx, "/root", "session-a", &content).await;

        let threads = ClaudeHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title.as_ref(), "Fix the release crash");
    }

    #[gpui::test]
    async fn scan_skips_local_command_caveat_and_command_wrapper_for_title(
        cx: &mut TestAppContext,
    ) {
        let content = [
            meta_project_history_line(
                "session-a",
                "/root",
                "<local-command-caveat>Caveat: The messages below were generated by the user \
                 while running local commands. DO NOT respond to these messages or otherwise \
                 consider them in your response unless explicitly told to.</local-command-caveat>",
                "2026-06-18T20:23:14.000Z",
            ),
            project_history_line(
                "session-a",
                "/root",
                "<command-name>/clear</command-name>\n            <command-message>clear\
                 </command-message>\n            <command-args></command-args>",
                "2026-06-18T20:23:15.000Z",
            ),
            project_history_line(
                "session-a",
                "/root",
                "switch to main and pull latest",
                "2026-06-18T20:24:14.000Z",
            ),
        ]
        .join("\n");
        let host = host_with_project_history(cx, "/root", "session-a", &content).await;

        let threads = ClaudeHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title.as_ref(), "switch to main and pull latest");
    }

    #[gpui::test]
    async fn scan_project_history_uses_current_project_and_newest_first(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let project_dir = PathBuf::from("/claude-home")
            .join("projects")
            .join(project_history_dir_name(Path::new("/root")));
        fs.create_dir(&project_dir).await.unwrap();
        fs.insert_file(
            project_dir.join("session-older.jsonl"),
            project_history_line(
                "session-older",
                "/root",
                "older prompt",
                "2026-06-18T20:23:14.000Z",
            )
            .into_bytes(),
        )
        .await;
        fs.insert_file(
            project_dir.join("session-newer.jsonl"),
            project_history_line(
                "session-newer",
                "/root",
                "newer prompt",
                "2026-06-18T21:23:14.000Z",
            )
            .into_bytes(),
        )
        .await;
        fs.insert_file(
            project_dir.join("session-other-root.jsonl"),
            project_history_line(
                "session-other-root",
                "/other-root",
                "other root prompt",
                "2026-06-18T22:23:14.000Z",
            )
            .into_bytes(),
        )
        .await;
        let host = AgentHistoryHost {
            fs: Arc::new(LocalHistoryFs(fs)),
            base_dir: PathBuf::from("/claude-home"),
            cache: Arc::new(crate::history::HistoryParseCache::default()),
            path_style: PathStyle::Posix,
        };

        let threads = ClaudeHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].session_id.as_ref(), "session-newer");
        assert_eq!(threads[1].session_id.as_ref(), "session-older");
    }

    #[gpui::test]
    async fn scan_returns_empty_when_no_history_file_exists(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(std::path::Path::new("/claude-home"))
            .await
            .unwrap();
        let host = AgentHistoryHost {
            fs: Arc::new(LocalHistoryFs(fs)),
            base_dir: PathBuf::from("/claude-home"),
            cache: Arc::new(crate::history::HistoryParseCache::default()),
            path_style: PathStyle::Posix,
        };

        let threads = ClaudeHistoryProvider
            .scan(&host, &[PathBuf::from("/root")])
            .await
            .unwrap();

        assert!(threads.is_empty());
    }

    #[test]
    fn truncate_title_adds_ellipsis_past_the_limit() {
        let long_display = "x".repeat(100);

        let title = normalize_title(&long_display).unwrap();

        assert_eq!(title.chars().count(), MAX_TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn resume_command_uses_resume_flag_and_session_id() {
        let base = AgentLaunchCommand {
            command: Some("claude".to_string()),
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
            last_activity_at: UNIX_EPOCH,
        };

        let resumed = ClaudeHistoryProvider.resume_command(&base, &thread, &[]);

        assert_eq!(resumed.command, Some("claude".to_string()));
        assert_eq!(resumed.args, vec!["--resume", "session-a"]);
        assert_eq!(resumed.cwd, Some(PathBuf::from("/root")));
    }

    #[test]
    fn resume_command_appends_extra_args_for_resume_options() {
        let base = AgentLaunchCommand::default();
        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("title"),
            project_root: PathBuf::from("/root"),
            last_activity_at: UNIX_EPOCH,
        };

        let resumed = ClaudeHistoryProvider.resume_command(
            &base,
            &thread,
            &["--dangerously-skip-permissions".to_string()],
        );

        assert_eq!(
            resumed.args,
            vec!["--resume", "session-a", "--dangerously-skip-permissions"]
        );
    }
}
