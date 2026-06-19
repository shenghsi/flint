use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;
use collections::HashMap;
use gpui::SharedString;
use serde::Deserialize;

use crate::AgentLaunchCommand;
use crate::history::{AgentHistoryHost, AgentHistoryProvider, HistoricalThread};

const MAX_TITLE_CHARS: usize = 60;

pub struct ClaudeHistoryProvider;

#[derive(Deserialize)]
struct HistoryRecord {
    #[serde(rename = "sessionId")]
    session_id: String,
    project: String,
    display: String,
    timestamp: u64,
}

#[async_trait]
impl AgentHistoryProvider for ClaudeHistoryProvider {
    async fn scan(
        &self,
        host: &AgentHistoryHost,
        project_roots: &[PathBuf],
    ) -> Result<Vec<HistoricalThread>> {
        let history_path = host.base_dir.join("history.jsonl");
        let content = match host.fs.load(&history_path).await {
            Ok(content) => content,
            Err(_) => return Ok(Vec::new()),
        };

        let mut latest_by_session: HashMap<String, HistoryRecord> = HashMap::default();
        for line in content.lines() {
            let Ok(record) = serde_json::from_str::<HistoryRecord>(line) else {
                continue;
            };
            let is_newer = latest_by_session
                .get(&record.session_id)
                .is_none_or(|existing| record.timestamp >= existing.timestamp);
            if is_newer {
                latest_by_session.insert(record.session_id.clone(), record);
            }
        }

        Ok(latest_by_session
            .into_values()
            .filter_map(|record| {
                let project_root = PathBuf::from(&record.project);
                if !project_roots.iter().any(|root| root == &project_root) {
                    return None;
                }
                Some(HistoricalThread {
                    session_id: SharedString::from(record.session_id),
                    title: SharedString::from(truncate_title(&record.display)),
                    project_root,
                    last_activity_at: UNIX_EPOCH + Duration::from_millis(record.timestamp),
                })
            })
            .collect())
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
        }
    }
}

fn truncate_title(display: &str) -> String {
    let trimmed = display.trim();
    if trimmed.chars().count() <= MAX_TITLE_CHARS {
        trimmed.to_string()
    } else {
        let truncated: String = trimmed.chars().take(MAX_TITLE_CHARS).collect();
        format!("{truncated}…")
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

    fn jsonl_line(session_id: &str, project: &str, display: &str, timestamp: u64) -> String {
        serde_json::json!({
            "sessionId": session_id,
            "project": project,
            "display": display,
            "timestamp": timestamp,
        })
        .to_string()
    }

    async fn host_with_history(cx: &TestAppContext, content: &str) -> AgentHistoryHost {
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(std::path::Path::new("/claude-home")).await.unwrap();
        fs.insert_file("/claude-home/history.jsonl", content.as_bytes().to_vec())
            .await;
        AgentHistoryHost {
            fs: fs as Arc<dyn fs::Fs>,
            base_dir: PathBuf::from("/claude-home"),
        }
    }

    #[gpui::test]
    async fn scan_keeps_the_latest_entry_per_session(cx: &mut TestAppContext) {
        let content = [
            jsonl_line("session-a", "/root", "first prompt", 100),
            jsonl_line("session-a", "/root", "second prompt", 200),
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
            jsonl_line("session-a", "/root_a", "in root a", 100),
            jsonl_line("session-b", "/root_b", "in root b", 100),
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
    async fn scan_returns_empty_when_no_history_file_exists(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(std::path::Path::new("/claude-home")).await.unwrap();
        let host = AgentHistoryHost {
            fs: fs as Arc<dyn fs::Fs>,
            base_dir: PathBuf::from("/claude-home"),
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

        let title = truncate_title(&long_display);

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
