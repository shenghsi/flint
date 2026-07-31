use std::path::Path;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlez::connection::Connection;

use crate::transcript::{Classified, RawEvent, select_and_render, summarize_tool_input};
use crate::{
    ExcerptBudget, Extraction, FileIdentity, HistoryHost, HistoryKind, HistoryProvider,
    IndexedSession, ProviderRefresh,
};

const DATABASE_FILE_NAME: &str = "opencode.db";

pub struct OpenCodeHistoryProvider;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OpenCodeSourceState {
    database: FileIdentity,
    wal: Option<FileIdentity>,
}

#[async_trait]
impl HistoryProvider for OpenCodeHistoryProvider {
    fn kind(&self) -> HistoryKind {
        HistoryKind::OpenCode
    }

    async fn refresh(
        &self,
        host: &HistoryHost,
        _previous: Option<&Value>,
    ) -> Result<ProviderRefresh> {
        let database_path = host.join(&host.base_dir, DATABASE_FILE_NAME)?;
        let local_path = host.fs.local_path(&database_path).with_context(|| {
            format!(
                "OpenCode history database is not host-local: {}",
                database_path.display()
            )
        })?;
        let sessions = read_sessions(&local_path)?;
        Ok(ProviderRefresh {
            source_state: serde_json::to_value(source_state(host, &database_path).await?)?,
            sessions,
        })
    }

    async fn extract(
        &self,
        host: &HistoryHost,
        session: &IndexedSession,
        budget: &ExcerptBudget,
    ) -> Result<Extraction> {
        let database_path = host.join(&host.base_dir, DATABASE_FILE_NAME)?;
        let before = source_state(host, &database_path).await?;
        let local_path = host.fs.local_path(&database_path).with_context(|| {
            format!(
                "OpenCode history database is not host-local: {}",
                database_path.display()
            )
        })?;
        let classified = read_transcript(&local_path, &session.session_id)?;
        let after = source_state(host, &database_path).await?;
        Ok(
            match select_and_render(classified, budget, before != after) {
                Ok(excerpt) => Extraction::Excerpt(excerpt),
                Err(reason) => Extraction::Refused(reason),
            },
        )
    }
}

async fn source_state(host: &HistoryHost, database_path: &Path) -> Result<OpenCodeSourceState> {
    let database = host
        .fs
        .metadata(database_path)
        .await?
        .context("OpenCode history database does not exist")?;
    let wal_path = database_path.with_file_name(format!("{DATABASE_FILE_NAME}-wal"));
    let wal = host.fs.metadata(&wal_path).await?;
    Ok(OpenCodeSourceState { database, wal })
}

fn read_sessions(path: &Path) -> Result<Vec<IndexedSession>> {
    let connection = Connection::open_read_only(path)?;
    let mut select = connection.select::<(String, String, String, i64)>(
        "SELECT id, title, directory, time_updated
         FROM session
         WHERE parent_id IS NULL
         ORDER BY time_updated DESC",
    )?;
    select()?
        .into_iter()
        .map(|(session_id, title, working_dir, updated_ms)| {
            let updated_ms = u64::try_from(updated_ms)
                .with_context(|| format!("OpenCode session {session_id:?} has a negative time"))?;
            Ok(IndexedSession {
                session_id,
                resolved_title: normalize_title(title),
                fallback_title: None,
                working_dir,
                last_activity_secs: updated_ms / 1000,
                last_activity_nanos: ((updated_ms % 1000) * 1_000_000) as u32,
                source_path: None,
                source_identity: None,
            })
        })
        .collect()
}

fn normalize_title(title: String) -> String {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        "OpenCode session".to_string()
    } else {
        title.chars().take(60).collect()
    }
}

fn read_transcript(path: &Path, session_id: &str) -> Result<Classified> {
    let connection = Connection::open_read_only(path)?;
    let mut select = connection.select_bound::<String, (String, String, Option<String>)>(
        "SELECT message.id, message.data, part.data
         FROM message
         LEFT JOIN part ON part.message_id = message.id
         WHERE message.session_id = ?
         ORDER BY message.time_created, message.id, part.time_created, part.id",
    )?;
    let rows = select(session_id.to_string())?;
    Ok(classify_rows(rows))
}

fn classify_rows(rows: Vec<(String, String, Option<String>)>) -> Classified {
    let mut classified = Classified::default();
    let mut current_message_id = None;
    let mut current_role = None;
    let mut current_text = Vec::new();

    let flush_text = |role: Option<&str>, text: &mut Vec<String>, classified: &mut Classified| {
        if text.is_empty() {
            return;
        }
        let text = std::mem::take(text).join("\n");
        match role {
            Some("user") => classified.events.push(RawEvent::User(text)),
            Some("assistant") => classified.events.push(RawEvent::Assistant(text)),
            _ => classified.unknown_count += 1,
        }
    };

    for (message_id, message_data, part_data) in rows {
        if current_message_id.as_deref() != Some(message_id.as_str()) {
            flush_text(current_role.as_deref(), &mut current_text, &mut classified);
            current_message_id = Some(message_id);
            current_role = match serde_json::from_str::<Value>(&message_data) {
                Ok(message) => message
                    .get("role")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                Err(_) => {
                    classified.malformed_count += 1;
                    None
                }
            };
        }
        let Some(part_data) = part_data else {
            continue;
        };
        let Ok(part) = serde_json::from_str::<Value>(&part_data) else {
            classified.malformed_count += 1;
            continue;
        };
        classify_part(&part, &mut current_text, &mut classified);
    }
    flush_text(current_role.as_deref(), &mut current_text, &mut classified);
    classified
}

fn classify_part(part: &Value, text: &mut Vec<String>, classified: &mut Classified) {
    match part.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(value) = part.get("text").and_then(Value::as_str)
                && !value.trim().is_empty()
            {
                text.push(value.to_string());
            }
        }
        Some("tool") => {
            let call_id = part
                .get("callID")
                .and_then(Value::as_str)
                .map(str::to_string);
            let name = part
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            let state = part.get("state");
            classified.events.push(RawEvent::ToolCall {
                call_id: call_id.clone(),
                name,
                detail: summarize_tool_input(state.and_then(|state| state.get("input"))),
            });
            match state
                .and_then(|state| state.get("status"))
                .and_then(Value::as_str)
            {
                Some("completed") => classified.events.push(RawEvent::ToolResult {
                    call_id,
                    output: state
                        .and_then(|state| state.get("output"))
                        .map(value_text)
                        .unwrap_or_default(),
                    is_error: false,
                }),
                Some("error") => classified.events.push(RawEvent::ToolResult {
                    call_id,
                    output: state
                        .and_then(|state| state.get("error"))
                        .map(value_text)
                        .unwrap_or_default(),
                    is_error: true,
                }),
                Some("pending") | Some("running") => {}
                Some(_) | None => classified.unknown_count += 1,
            }
        }
        Some("compaction") => classified
            .events
            .push(RawEvent::Checkpoint("compaction".to_string())),
        Some("reasoning") | Some("step-start") | Some("step-finish") | Some("file")
        | Some("snapshot") | Some("patch") | Some("agent") | Some("subtask") => {
            classified.events.push(RawEvent::Noise)
        }
        Some(_) | None => classified.unknown_count += 1,
    }
}

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}
#[cfg(test)]
mod tests {
    use super::OpenCodeHistoryProvider;
    use crate::{
        ExcerptBudget, Extraction, FileIdentity, HistoryFs, HistoryHost, HistoryKind,
        HistoryProvider, IndexService, LocalHistoryFs,
    };
    use async_trait::async_trait;
    use gpui::TestAppContext;
    use sqlez::connection::Connection;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use util::paths::PathStyle;

    struct ChangingIdentityFs {
        database_path: PathBuf,
        database_reads: AtomicUsize,
    }

    #[async_trait]
    impl HistoryFs for ChangingIdentityFs {
        async fn read_dir(&self, _path: &std::path::Path) -> anyhow::Result<Vec<PathBuf>> {
            anyhow::bail!("directory reads are not expected")
        }

        async fn load(&self, _path: &std::path::Path) -> anyhow::Result<String> {
            anyhow::bail!("file loads are not expected")
        }

        async fn metadata(&self, path: &std::path::Path) -> anyhow::Result<Option<FileIdentity>> {
            if path != self.database_path {
                return Ok(None);
            }
            let read = self.database_reads.fetch_add(1, Ordering::SeqCst);
            Ok(Some(FileIdentity {
                modified_at_secs: read as u64,
                modified_at_nanos: 0,
                length: 1,
            }))
        }

        fn local_path(&self, path: &std::path::Path) -> Option<PathBuf> {
            Some(path.to_path_buf())
        }
    }

    fn create_fixture_database(path: &std::path::Path) -> anyhow::Result<()> {
        let connection = Connection::open_file(path.to_string_lossy().as_ref());
        connection.exec(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                directory TEXT NOT NULL,
                title TEXT NOT NULL,
                parent_id TEXT,
                time_updated INTEGER NOT NULL
            )",
        )?()?;
        connection.exec(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                data TEXT NOT NULL
            )",
        )?()?;
        connection.exec(
            "CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                data TEXT NOT NULL
            )",
        )?()?;

        let mut insert_session = connection.exec_bound::<(&str, &str, &str, Option<&str>, i64)>(
            "INSERT INTO session (id, directory, title, parent_id, time_updated)
             VALUES (?, ?, ?, ?, ?)",
        )?;
        insert_session((
            "ses_project",
            "/work/project",
            "Fix the parser",
            None,
            1_722_470_400_123,
        ))?;
        insert_session((
            "ses_other",
            "/work/other",
            "Other work",
            None,
            1_722_470_300_000,
        ))?;
        insert_session((
            "ses_child",
            "/work/project",
            "Background child",
            Some("ses_project"),
            1_722_470_500_000,
        ))?;

        let mut insert_message = connection.exec_bound::<(&str, &str, i64, &str)>(
            "INSERT INTO message (id, session_id, time_created, data) VALUES (?, ?, ?, ?)",
        )?;
        insert_message(("msg_user", "ses_project", 1, r#"{"role":"user"}"#))?;
        insert_message(("msg_assistant", "ses_project", 2, r#"{"role":"assistant"}"#))?;

        let mut insert_part = connection.exec_bound::<(&str, &str, &str, i64, &str)>(
            "INSERT INTO part (id, message_id, session_id, time_created, data)
             VALUES (?, ?, ?, ?, ?)",
        )?;
        insert_part((
            "prt_user",
            "msg_user",
            "ses_project",
            1,
            r#"{"type":"text","text":"Fix the parser"}"#,
        ))?;
        insert_part((
            "prt_assistant",
            "msg_assistant",
            "ses_project",
            2,
            r#"{"type":"text","text":"I will inspect it."}"#,
        ))?;
        insert_part((
            "prt_tool",
            "msg_assistant",
            "ses_project",
            3,
            r#"{"type":"tool","callID":"call_1","tool":"bash","state":{"status":"completed","input":{"command":"cargo test"},"output":"ok"}}"#,
        ))?;
        Ok(())
    }

    fn history_host(cx: &mut TestAppContext, base_dir: PathBuf) -> HistoryHost {
        let fs = fs::RealFs::new(None, cx.executor());
        HistoryHost {
            fs: Arc::new(LocalHistoryFs(Arc::new(fs))),
            base_dir,
            path_style: PathStyle::Posix,
        }
    }

    #[gpui::test]
    async fn refresh_indexes_root_sessions_from_opencode_database(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let database_path = directory.path().join("opencode.db");
        create_fixture_database(&database_path).expect("OpenCode fixture database");
        let host = history_host(cx, directory.path().to_path_buf());

        let refresh = OpenCodeHistoryProvider
            .refresh(&host, None)
            .await
            .expect("OpenCode history refresh");

        assert_eq!(OpenCodeHistoryProvider.kind(), HistoryKind::OpenCode);
        assert_eq!(refresh.sessions.len(), 2);
        let project = refresh
            .sessions
            .iter()
            .find(|session| session.session_id == "ses_project")
            .expect("project session should be indexed");
        assert_eq!(project.resolved_title, "Fix the parser");
        assert_eq!(project.working_dir, "/work/project");
        assert_eq!(project.last_activity_secs, 1_722_470_400);
        assert_eq!(project.last_activity_nanos, 123_000_000);
        assert!(
            refresh
                .sessions
                .iter()
                .all(|session| session.session_id != "ses_child")
        );
    }

    #[gpui::test]
    async fn extraction_reads_opencode_messages_and_parts(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let database_path = directory.path().join("opencode.db");
        create_fixture_database(&database_path).expect("OpenCode fixture database");
        let host = history_host(cx, directory.path().to_path_buf());

        let session = OpenCodeHistoryProvider
            .refresh(&host, None)
            .await
            .expect("OpenCode history refresh")
            .sessions
            .into_iter()
            .find(|session| session.session_id == "ses_project")
            .expect("project session should be indexed");
        let extraction = OpenCodeHistoryProvider
            .extract(
                &host,
                &session,
                &ExcerptBudget {
                    max_total_bytes: 4096,
                    max_turns: 20,
                    max_tool_bytes: 400,
                    max_head_bytes: 1024,
                },
            )
            .await
            .expect("OpenCode transcript extraction");

        let Extraction::Excerpt(excerpt) = extraction else {
            panic!("OpenCode transcript should be extractable");
        };
        assert!(excerpt.markdown.contains("**User:** Fix the parser"));
        assert!(excerpt.markdown.contains("**Agent:** I will inspect it."));
        assert!(excerpt.markdown.contains("**Tool `bash`:** cargo test"));
        assert!(excerpt.markdown.contains("**Result:** ok"));
    }

    #[gpui::test]
    async fn extraction_marks_database_changes_as_possibly_incomplete() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let database_path = directory.path().join("opencode.db");
        create_fixture_database(&database_path).expect("OpenCode fixture database");
        let host = HistoryHost {
            fs: Arc::new(ChangingIdentityFs {
                database_path,
                database_reads: AtomicUsize::new(0),
            }),
            base_dir: directory.path().to_path_buf(),
            path_style: PathStyle::Posix,
        };
        let session = crate::IndexedSession {
            session_id: "ses_project".to_string(),
            resolved_title: "Fix the parser".to_string(),
            fallback_title: None,
            working_dir: "/work/project".to_string(),
            last_activity_secs: 0,
            last_activity_nanos: 0,
            source_path: None,
            source_identity: None,
        };

        let extraction = OpenCodeHistoryProvider
            .extract(
                &host,
                &session,
                &ExcerptBudget {
                    max_total_bytes: 4096,
                    max_turns: 20,
                    max_tool_bytes: 400,
                    max_head_bytes: 1024,
                },
            )
            .await
            .expect("OpenCode transcript extraction");
        let Extraction::Excerpt(excerpt) = extraction else {
            panic!("OpenCode transcript should be extractable");
        };
        assert!(excerpt.possibly_incomplete);
    }

    #[gpui::test]
    async fn index_service_extracts_only_indexed_opencode_sessions(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let database_path = directory.path().join("opencode.db");
        create_fixture_database(&database_path).expect("OpenCode fixture database");
        let host = history_host(cx, directory.path().to_path_buf());
        let cache_fs = fs::FakeFs::new(cx.executor());
        let service = IndexService::new(cache_fs, PathBuf::from("/cache/agent_threads"));

        let extraction = service
            .extract(
                HistoryKind::OpenCode,
                &host,
                "ses_project",
                Some("/work/project"),
                &ExcerptBudget {
                    max_total_bytes: 4096,
                    max_turns: 20,
                    max_tool_bytes: 400,
                    max_head_bytes: 1024,
                },
            )
            .await
            .expect("indexed OpenCode transcript extraction");
        assert!(matches!(extraction, Extraction::Excerpt(_)));

        let result = service
            .extract(
                HistoryKind::OpenCode,
                &host,
                "ses_missing",
                None,
                &ExcerptBudget {
                    max_total_bytes: 4096,
                    max_turns: 20,
                    max_tool_bytes: 400,
                    max_head_bytes: 1024,
                },
            )
            .await;
        let error = match result {
            Ok(_) => panic!("unindexed OpenCode session should be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("no indexed session"));
    }
}
