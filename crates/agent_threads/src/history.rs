use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context as _, Result, anyhow};
use collections::HashMap;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use gpui::{App, AsyncApp, Entity, SharedString};
use project::Project;
use rpc::AnyProtoClient;
use util::paths::PathStyle;

use crate::AgentLaunchCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoricalThread {
    pub session_id: SharedString,
    pub title: SharedString,
    pub project_root: PathBuf,
    pub last_activity_at: SystemTime,
}

pub(crate) type HistorySnapshotStream = BoxStream<'static, Result<Vec<HistoricalThread>>>;

pub(crate) async fn load_history_source<Publish>(
    mut indexed: HistorySnapshotStream,
    mut publish: Publish,
) -> Result<()>
where
    Publish: FnMut(Vec<HistoricalThread>),
{
    let mut published_indexed_snapshot = false;
    while let Some(snapshot) = indexed.next().await {
        match snapshot {
            Ok(snapshot) => {
                published_indexed_snapshot = true;
                publish(snapshot);
            }
            Err(error) if published_indexed_snapshot => {
                log::warn!(
                    "agent_threads: indexed history refresh failed after a snapshot: {error:#}"
                );
                return Ok(());
            }
            Err(error) => {
                return Err(error)
                    .context("agent_threads: indexed history failed before any snapshot");
            }
        }
    }
    Ok(())
}

pub(crate) fn local_indexed_history_stream(
    service: agent_history::IndexService,
    kind: agent_history::HistoryKind,
    host: agent_history::HistoryHost,
    project_roots: Vec<PathBuf>,
) -> HistorySnapshotStream {
    let cached_service = service.clone();
    let cached_host = host.clone();
    let cached_roots = project_roots.clone();
    let cached = futures::stream::once(async move {
        cached_service
            .cached_snapshot(kind, &cached_host, &cached_roots)
            .await
            .map(indexed_snapshot_threads)
    })
    .filter_map(futures::future::ready)
    .map(Ok);
    let fresh = futures::stream::once(async move {
        service
            .refresh(kind, &host, &project_roots)
            .await
            .map(indexed_snapshot_threads)
    });
    cached.chain(fresh).boxed()
}

pub(crate) fn remote_indexed_history_stream(
    proto_client: AnyProtoClient,
    kind_id: &'static str,
    history_root: PathBuf,
    project_roots: Vec<PathBuf>,
    path_style: PathStyle,
) -> HistorySnapshotStream {
    let request: Result<proto::StreamAgentThreadHistory> = (|| {
        Ok(proto::StreamAgentThreadHistory {
            project_id: proto::REMOTE_SERVER_PROJECT_ID,
            kind: kind_id.to_string(),
            normalized_history_root: path_for_style(&history_root, path_style)?,
            project_roots: project_roots
                .iter()
                .map(|root| path_for_style(root, path_style))
                .collect::<Result<Vec<_>>>()?,
        })
    })();
    futures::stream::once(async move {
        let request = request?;
        proto_client.request_stream(request).await
    })
    .try_flatten()
    .map(|snapshot| snapshot.and_then(proto_snapshot_threads))
    .boxed()
}

pub(crate) fn history_index_cache_root() -> PathBuf {
    paths::home_dir()
        .join(".flint")
        .join("cache")
        .join("agent_threads")
}

struct GlobalAgentHistoryIndex(agent_history::IndexService);

impl gpui::Global for GlobalAgentHistoryIndex {}

/// Returns the app-wide history index service, creating it on first use.
/// Shared by the agent threads panel and the background session-discovery
/// loop so both hit the same on-disk cache instead of scanning independently.
pub(crate) fn global_history_index(
    fs: &Arc<dyn fs::Fs>,
    cx: &mut App,
) -> agent_history::IndexService {
    if let Some(index) = cx.try_global::<GlobalAgentHistoryIndex>() {
        index.0.clone()
    } else {
        let index = agent_history::IndexService::new(fs.clone(), history_index_cache_root());
        cx.set_global(GlobalAgentHistoryIndex(index.clone()));
        index
    }
}

/// A rendered handoff excerpt returned to the client, independent of whether it
/// was produced locally or on a remote host.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentTranscriptExcerpt {
    pub markdown: String,
    pub degraded: bool,
    pub possibly_incomplete: bool,
    pub malformed_count: u32,
    pub unknown_count: u32,
    pub included_turns: u32,
    pub omitted_turns: u32,
}

impl From<agent_history::TranscriptExcerpt> for AgentTranscriptExcerpt {
    fn from(excerpt: agent_history::TranscriptExcerpt) -> Self {
        Self {
            markdown: excerpt.markdown,
            degraded: excerpt.degraded,
            possibly_incomplete: excerpt.possibly_incomplete,
            malformed_count: excerpt.malformed_count as u32,
            unknown_count: excerpt.unknown_count as u32,
            included_turns: excerpt.included_turns as u32,
            omitted_turns: excerpt.omitted_turns as u32,
        }
    }
}

/// Extracts a handoff excerpt for a local project directly through the host
/// index. `Ok(None)` means the transcript was read but nothing trustworthy
/// survived (the caller must not write a handoff).
pub(crate) async fn local_extract_transcript(
    service: agent_history::IndexService,
    kind: agent_history::HistoryKind,
    host: agent_history::HistoryHost,
    session_id: String,
    working_dir: Option<String>,
) -> Result<Option<AgentTranscriptExcerpt>> {
    match service
        .extract(
            kind,
            &host,
            &session_id,
            working_dir.as_deref(),
            &agent_history::DEFAULT_BUDGET,
        )
        .await?
    {
        agent_history::Extraction::Excerpt(excerpt) => Ok(Some(excerpt.into())),
        agent_history::Extraction::Refused(_) => Ok(None),
    }
}

/// Extracts a handoff excerpt for a remote project by asking the host to resolve
/// the session and parse it there; the client never reads the remote transcript
/// itself. `Ok(None)` mirrors the local refusal case.
///
/// Not yet called: the handoff panel action currently gates cross-agent
/// handoff to local projects, since writing the handoff document also needs to
/// happen on the target's host and there is no remote write path yet. Kept
/// landed and tested so remote support is a wiring change, not a new design.
#[allow(dead_code)]
pub(crate) async fn remote_extract_transcript(
    proto_client: AnyProtoClient,
    kind_id: &'static str,
    history_root: PathBuf,
    session_id: String,
    working_dir: Option<String>,
    path_style: PathStyle,
) -> Result<Option<AgentTranscriptExcerpt>> {
    let working_dir = working_dir
        .map(|dir| normalize_path_for_style(&dir, path_style))
        .filter(|dir| !dir.is_empty());
    let response = proto_client
        .request(proto::ExtractAgentTranscript {
            project_id: proto::REMOTE_SERVER_PROJECT_ID,
            kind: kind_id.to_string(),
            normalized_history_root: path_for_style(&history_root, path_style)?,
            session_id,
            working_dir,
        })
        .await?;
    if !response.found {
        return Ok(None);
    }
    Ok(Some(AgentTranscriptExcerpt {
        markdown: response.markdown,
        degraded: response.degraded,
        possibly_incomplete: response.possibly_incomplete,
        malformed_count: response.malformed_count,
        unknown_count: response.unknown_count,
        included_turns: response.included_turns,
        omitted_turns: response.omitted_turns,
    }))
}

pub(crate) fn indexed_snapshot_threads(snapshot: agent_history::Snapshot) -> Vec<HistoricalThread> {
    snapshot
        .entries
        .into_iter()
        .map(|entry| HistoricalThread {
            session_id: entry.session_id.into(),
            title: entry.title.into(),
            project_root: entry.project_root,
            last_activity_at: entry.last_activity_at,
        })
        .collect()
}

fn proto_snapshot_threads(
    snapshot: proto::AgentThreadHistorySnapshot,
) -> Result<Vec<HistoricalThread>> {
    let cached = proto::agent_thread_history_snapshot::Freshness::Cached as i32;
    let fresh = proto::agent_thread_history_snapshot::Freshness::Fresh as i32;
    anyhow::ensure!(
        snapshot.freshness == cached || snapshot.freshness == fresh,
        "remote history snapshot has invalid freshness"
    );
    snapshot
        .entries
        .into_iter()
        .map(|entry| {
            let timestamp = entry
                .last_activity_at
                .context("remote history entry has no activity timestamp")?;
            let last_activity_at = SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::new(timestamp.seconds, timestamp.nanos))
                .context("remote history activity timestamp is out of range")?;
            Ok(HistoricalThread {
                session_id: entry.session_id.into(),
                title: entry.title.into(),
                project_root: PathBuf::from(entry.project_root),
                last_activity_at,
            })
        })
        .collect()
}

fn path_for_style(path: &Path, path_style: PathStyle) -> Result<String> {
    let path = path
        .to_str()
        .with_context(|| format!("path contains invalid UTF-8: {path:?}"))?;
    Ok(normalize_path_for_style(path, path_style))
}

fn normalize_path_for_style(path: &str, path_style: PathStyle) -> String {
    let path = match path_style {
        PathStyle::Posix => path.replace('\\', "/"),
        PathStyle::Windows => path.replace('/', "\\"),
    };
    path_style.normalize(&path)
}

/// Builds the command used to resume a historical thread. `resume_command`
/// is used unconditionally (independent of how the thread was discovered),
/// unlike the scan step this trait used to also own -- see the removal of
/// `scan()` in the "remove the legacy agent-history scanner" change.
pub trait AgentHistoryProvider: Send + Sync {
    /// Builds the command used to resume `thread`, starting from the
    /// configured launch command's `command`/`env` (its `args` are for
    /// fresh sessions and are intentionally not reused here) plus any
    /// selected `extra_args` from the kind's `resume_options`.
    fn resume_command(
        &self,
        base: &AgentLaunchCommand,
        thread: &HistoricalThread,
        extra_args: &[String],
    ) -> AgentLaunchCommand;
}

/// Resolves just the base config directory (e.g. `~/.claude`) for an agent.
/// Used to set up filesystem watching on local projects, where only the
/// directory to watch is needed.
pub async fn resolve_history_base_dir(
    project: &Entity<Project>,
    env_var_name: &str,
    env_child: Option<&str>,
    default_dir_name: &str,
    cx: &mut AsyncApp,
) -> Result<PathBuf> {
    let (environment, anchor_path, path_style) = project.read_with(cx, |project, cx| {
        (
            project.environment().clone(),
            first_worktree_root(project, cx),
            project.path_style(cx),
        )
    });
    let anchor_path = anchor_path.ok_or_else(|| anyhow!("project has no worktrees"))?;

    let env_task = environment.update(cx, |environment, cx| {
        environment.directory_environment(anchor_path, cx)
    });
    let env_map = env_task
        .await
        .ok_or_else(|| anyhow!("couldn't resolve the project's environment"))?;

    base_dir_from_env(
        &env_map,
        env_var_name,
        env_child,
        default_dir_name,
        path_style,
    )
}

/// Picks `$<env_var_name>` when set, otherwise the platform home directory
/// joined with `<default_dir_name>`.
/// Pulled out of `resolve_history_host` so it's testable without needing a
/// real `Project`/environment resolution round trip.
fn base_dir_from_env(
    env_map: &HashMap<String, String>,
    env_var_name: &str,
    env_child: Option<&str>,
    default_dir_name: &str,
    path_style: PathStyle,
) -> Result<PathBuf> {
    if let Some(override_dir) = env_map.get(env_var_name) {
        let override_dir = normalize_path_for_style(override_dir, path_style);
        if let Some(env_child) = env_child {
            path_style.join_path(
                override_dir,
                normalize_path_for_style(env_child, path_style),
            )
        } else {
            Ok(PathBuf::from(override_dir))
        }
    } else {
        let home = env_map
            .get("HOME")
            .or_else(|| {
                path_style
                    .is_windows()
                    .then(|| env_map.get("USERPROFILE"))
                    .flatten()
            })
            .ok_or_else(|| anyhow!("no home directory in the project's resolved environment"))?;
        path_style.join_path(home, normalize_path_for_style(default_dir_name, path_style))
    }
}

fn first_worktree_root(project: &Project, cx: &App) -> Option<Arc<std::path::Path>> {
    project
        .visible_worktrees(cx)
        .next()
        .map(|worktree| worktree.read(cx).abs_path())
}

pub fn project_worktree_roots(project: &Project, cx: &App) -> Vec<PathBuf> {
    project
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        .collect()
}

/// The union of `project_worktree_roots` with every worktree (main and
/// linked) of every repository this project currently knows about -- the
/// design doc's "project group's worktree roots" for historical scanning.
///
/// Wider on purpose: a session retied away from this workspace still has
/// its on-disk history under its *original* worktree, since the process
/// cwd never moves (see `AgentThreadStore::commit_retie`). Scanning only
/// this workspace's own visible roots would never surface that session's
/// history here even when its tie now points at this project group, so
/// each panel scans every worktree of its repo(s) and lets effective-tie
/// filtering (not root selection) decide which rows actually belong to it.
/// Deliberately does *not* go through `MultiWorkspace`/`Window` to find
/// sibling workspaces: `Repository::linked_worktrees()` already reflects
/// every worktree on disk for this repo, independent of which of them
/// currently have an open Flint workspace, which is the correct, wider
/// scope for "could plausibly have this repo's agent thread history".
pub fn project_group_worktree_roots(project: &Project, cx: &App) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = project_worktree_roots(project, cx);
    for repo in project.repositories(cx).values() {
        let repo = repo.read(cx);
        roots.push(repo.work_directory_abs_path.to_path_buf());
        roots.extend(
            repo.linked_worktrees()
                .iter()
                .map(|worktree| worktree.path.clone()),
        );
    }
    roots.sort();
    roots.dedup();
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::Fs as _;
    use futures::FutureExt as _;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use project::FakeFs;
    use proto::EnvelopedMessage as _;
    use rpc::{ProtoClient, ProtoMessageHandlerSet};
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct HistoryStreamProtoClient {
        request: Mutex<Option<proto::StreamAgentThreadHistory>>,
        stream_request_count: AtomicUsize,
        unary_request_count: AtomicUsize,
        handlers: parking_lot::Mutex<ProtoMessageHandlerSet>,
    }

    impl Default for HistoryStreamProtoClient {
        fn default() -> Self {
            Self {
                request: Mutex::new(None),
                stream_request_count: AtomicUsize::new(0),
                unary_request_count: AtomicUsize::new(0),
                handlers: parking_lot::Mutex::new(ProtoMessageHandlerSet::default()),
            }
        }
    }

    impl ProtoClient for HistoryStreamProtoClient {
        fn request(
            &self,
            _envelope: proto::Envelope,
            request_type: &'static str,
        ) -> futures::future::BoxFuture<'static, Result<proto::Envelope>> {
            self.unary_request_count.fetch_add(1, Ordering::SeqCst);
            let request_type = request_type.to_string();
            async move { anyhow::bail!("unexpected unary request: {request_type}") }.boxed()
        }

        fn request_stream(
            &self,
            envelope: proto::Envelope,
            request_type: &'static str,
        ) -> futures::future::BoxFuture<
            'static,
            Result<futures::stream::BoxStream<'static, Result<proto::Envelope>>>,
        > {
            self.stream_request_count.fetch_add(1, Ordering::SeqCst);
            let request = proto::StreamAgentThreadHistory::from_envelope(envelope);
            if let Ok(mut recorded) = self.request.lock() {
                *recorded = request;
            }
            let result = if request_type == proto::StreamAgentThreadHistory::NAME {
                Ok(proto::AgentThreadHistorySnapshot {
                    freshness: proto::agent_thread_history_snapshot::Freshness::Fresh as i32,
                    generation: 1,
                    entries: vec![proto::AgentThreadHistoryEntry {
                        session_id: "remote-session".to_string(),
                        title: "Remote thread".to_string(),
                        project_root: "/work/project".to_string(),
                        last_activity_at: Some(SystemTime::UNIX_EPOCH.into()),
                    }],
                }
                .into_envelope(0, None, None))
            } else {
                Err(anyhow!("unexpected stream request: {request_type}"))
            };
            async move { Ok(futures::stream::iter([result]).boxed()) }.boxed()
        }

        fn send(&self, _envelope: proto::Envelope, _message_type: &'static str) -> Result<()> {
            Ok(())
        }

        fn send_response(
            &self,
            _envelope: proto::Envelope,
            _message_type: &'static str,
        ) -> Result<()> {
            Ok(())
        }

        fn message_handler_set(&self) -> &parking_lot::Mutex<ProtoMessageHandlerSet> {
            &self.handlers
        }

        fn is_via_collab(&self) -> bool {
            false
        }

        fn has_wsl_interop(&self) -> bool {
            false
        }
    }

    #[test]
    fn base_dir_uses_override_when_set() {
        let mut env = HashMap::default();
        env.insert("CODEX_HOME".to_string(), "/custom/codex-home".to_string());
        env.insert("HOME".to_string(), "/home/alice".to_string());

        let base_dir =
            base_dir_from_env(&env, "CODEX_HOME", None, ".codex", PathStyle::Posix).unwrap();

        assert_eq!(base_dir, PathBuf::from("/custom/codex-home"));
    }

    #[test]
    fn base_dir_falls_back_to_home_when_override_unset() {
        let mut env = HashMap::default();
        env.insert("HOME".to_string(), "/home/alice".to_string());

        let base_dir =
            base_dir_from_env(&env, "CODEX_HOME", None, ".codex", PathStyle::Posix).unwrap();

        assert_eq!(base_dir, PathBuf::from("/home/alice/.codex"));
    }

    #[test]
    fn base_dir_appends_agent_child_to_xdg_override() {
        let mut env = HashMap::default();
        env.insert("XDG_DATA_HOME".to_string(), "/custom/share".to_string());

        let base_dir = base_dir_from_env(
            &env,
            "XDG_DATA_HOME",
            Some("opencode"),
            ".local/share/opencode",
            PathStyle::Posix,
        )
        .expect("OpenCode XDG history directory");

        assert_eq!(base_dir, PathBuf::from("/custom/share/opencode"));
    }

    #[test]
    fn base_dir_errors_when_home_and_override_both_unset() {
        let env = HashMap::default();

        let result = base_dir_from_env(&env, "CODEX_HOME", None, ".codex", PathStyle::Posix);

        assert!(result.is_err());
    }

    #[test]
    fn base_dir_uses_project_path_style_when_falling_back_to_home() {
        let mut env = HashMap::default();
        env.insert("HOME".to_string(), "/home/alice".to_string());

        let base_dir =
            base_dir_from_env(&env, "CODEX_HOME", None, ".codex", PathStyle::Posix).unwrap();

        assert_eq!(base_dir.to_string_lossy(), "/home/alice/.codex");
    }

    #[test]
    fn base_dir_does_not_use_the_client_platform_separator() {
        let mut env = HashMap::default();
        env.insert("HOME".to_string(), "C:\\Users\\alice".to_string());

        let base_dir =
            base_dir_from_env(&env, "CODEX_HOME", None, ".codex", PathStyle::Windows).unwrap();

        assert_eq!(base_dir.to_string_lossy(), "C:\\Users\\alice\\.codex");
    }

    #[test]
    fn windows_agent_base_dirs_fall_back_to_user_profile() {
        let mut env = HashMap::default();
        env.insert("USERPROFILE".to_string(), "C:\\Users\\alice".to_string());

        for (environment_variable, environment_child, default_directory, expected) in [
            ("CODEX_HOME", None, ".codex", "C:\\Users\\alice\\.codex"),
            (
                "CLAUDE_CONFIG_DIR",
                None,
                ".claude",
                "C:\\Users\\alice\\.claude",
            ),
            (
                "PI_CODING_AGENT_DIR",
                None,
                ".pi/agent",
                "C:\\Users\\alice\\.pi\\agent",
            ),
            (
                "XDG_DATA_HOME",
                Some("opencode"),
                ".local/share/opencode",
                "C:\\Users\\alice\\.local\\share\\opencode",
            ),
        ] {
            let base_dir = base_dir_from_env(
                &env,
                environment_variable,
                environment_child,
                default_directory,
                PathStyle::Windows,
            )
            .unwrap();

            assert_eq!(base_dir.to_string_lossy(), expected);
        }
    }

    #[test]
    fn posix_base_dir_does_not_use_user_profile() {
        let mut env = HashMap::default();
        env.insert("USERPROFILE".to_string(), "/home/alice".to_string());

        let result = base_dir_from_env(&env, "CODEX_HOME", None, ".codex", PathStyle::Posix);

        assert!(result.is_err());
    }

    #[gpui::test]
    async fn indexed_snapshot_is_published(_cx: &mut TestAppContext) {
        let indexed_thread = HistoricalThread {
            session_id: SharedString::from("indexed"),
            title: SharedString::from("Indexed thread"),
            project_root: PathBuf::from("/work/project"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };
        let indexed = futures::stream::iter([Ok(vec![indexed_thread.clone()])]).boxed();
        let mut published = Vec::new();

        load_history_source(indexed, |threads| published.push(threads))
            .await
            .unwrap();

        assert_eq!(published, vec![vec![indexed_thread]]);
    }

    #[gpui::test]
    async fn indexed_error_before_a_snapshot_propagates(_cx: &mut TestAppContext) {
        let indexed = futures::stream::iter([Err(anyhow!("index unavailable"))]).boxed();
        let mut published: Vec<Vec<HistoricalThread>> = Vec::new();

        let result = load_history_source(indexed, |threads| published.push(threads)).await;

        assert!(result.is_err());
        assert!(published.is_empty());
    }

    #[gpui::test]
    async fn indexed_error_after_a_cached_snapshot_keeps_cached_data(_cx: &mut TestAppContext) {
        let cached_thread = HistoricalThread {
            session_id: SharedString::from("cached"),
            title: SharedString::from("Cached thread"),
            project_root: PathBuf::from("/work/project"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };
        let indexed = futures::stream::iter([
            Ok(vec![cached_thread.clone()]),
            Err(anyhow!("refresh failed")),
        ])
        .boxed();
        let mut published = Vec::new();

        load_history_source(indexed, |threads| published.push(threads))
            .await
            .unwrap();

        assert_eq!(published, vec![vec![cached_thread]]);
    }

    #[gpui::test]
    async fn cached_then_fresh_snapshots_replace_the_visible_history(_cx: &mut TestAppContext) {
        let cached_thread = HistoricalThread {
            session_id: SharedString::from("cached"),
            title: SharedString::from("Cached title"),
            project_root: PathBuf::from("/work/project"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };
        let fresh_thread = HistoricalThread {
            title: SharedString::from("Fresh title"),
            ..cached_thread.clone()
        };
        let indexed = futures::stream::iter([
            Ok(vec![cached_thread.clone()]),
            Ok(vec![fresh_thread.clone()]),
        ])
        .boxed();
        let mut published = Vec::new();

        load_history_source(indexed, |threads| published.push(threads))
            .await
            .unwrap();

        assert_eq!(published, vec![vec![cached_thread], vec![fresh_thread]]);
    }

    #[gpui::test]
    async fn local_index_stream_builds_a_fresh_host_owned_snapshot(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/home/user/.codex/sessions/2026/07/24"))
            .await
            .unwrap();
        fs.insert_file(
            "/home/user/.codex/sessions/2026/07/24/rollout-2026-07-24T10-00-00-aaa.jsonl",
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"aaa\",\"cwd\":\"/work/project\",\"timestamp\":\"2026-07-24T10:00:00.000Z\"}}\n",
                "{\"payload\":{\"type\":\"user_message\",\"message\":\"first task\"}}\n"
            )
            .as_bytes()
            .to_vec(),
        )
        .await;
        let service = agent_history::IndexService::new(
            fs.clone(),
            PathBuf::from("/home/user/.flint/cache/agent_threads"),
        );
        let host = agent_history::HistoryHost {
            fs: Arc::new(agent_history::LocalHistoryFs(fs)),
            base_dir: PathBuf::from("/home/user/.codex"),
            path_style: PathStyle::Posix,
        };
        let mut snapshots = local_indexed_history_stream(
            service,
            agent_history::HistoryKind::Codex,
            host,
            vec![PathBuf::from("/work/project")],
        );

        let fresh = snapshots
            .next()
            .await
            .expect("fresh snapshot")
            .expect("fresh scan should succeed");

        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].session_id, "aaa");
        assert_eq!(fresh[0].title, "first task");
        assert_eq!(fresh[0].project_root, PathBuf::from("/work/project"));
        assert!(snapshots.next().await.is_none());
    }

    #[gpui::test]
    async fn local_index_stream_emits_cached_then_refreshed_snapshots(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let rollout_dir = Path::new("/home/user/.codex/sessions/2026/07/24");
        let rollout_path = rollout_dir.join("rollout-2026-07-24T10-00-00-aaa.jsonl");
        fs.create_dir(rollout_dir).await.unwrap();
        fs.insert_file(
            &rollout_path,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"aaa\",\"cwd\":\"/work/project\",\"timestamp\":\"2026-07-24T10:00:00.000Z\"}}\n",
                "{\"payload\":{\"type\":\"user_message\",\"message\":\"cached title\"}}\n"
            )
            .as_bytes()
            .to_vec(),
        )
        .await;
        let service = agent_history::IndexService::new(
            fs.clone(),
            PathBuf::from("/home/user/.flint/cache/agent_threads"),
        );
        let host = agent_history::HistoryHost {
            fs: Arc::new(agent_history::LocalHistoryFs(fs.clone())),
            base_dir: PathBuf::from("/home/user/.codex"),
            path_style: PathStyle::Posix,
        };
        let roots = vec![PathBuf::from("/work/project")];
        service
            .refresh(agent_history::HistoryKind::Codex, &host, &roots)
            .await
            .unwrap();
        fs.insert_file(
            &rollout_path,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"aaa\",\"cwd\":\"/work/project\",\"timestamp\":\"2026-07-24T10:00:00.000Z\"}}\n",
                "{\"payload\":{\"type\":\"user_message\",\"message\":\"fresh title\"}}\n"
            )
            .as_bytes()
            .to_vec(),
        )
        .await;

        let mut snapshots =
            local_indexed_history_stream(service, agent_history::HistoryKind::Codex, host, roots);
        let cached = snapshots.next().await.unwrap().unwrap();
        let fresh = snapshots.next().await.unwrap().unwrap();

        assert_eq!(cached[0].title, "cached title");
        assert_eq!(fresh[0].title, "fresh title");
        assert!(snapshots.next().await.is_none());
    }

    #[test]
    fn remote_index_snapshot_conversion_preserves_host_paths_and_time() {
        let last_activity_at = SystemTime::UNIX_EPOCH + Duration::from_secs(42);
        let snapshot = proto::AgentThreadHistorySnapshot {
            freshness: proto::agent_thread_history_snapshot::Freshness::Fresh as i32,
            generation: 3,
            entries: vec![proto::AgentThreadHistoryEntry {
                session_id: "remote-session".to_string(),
                title: "Remote thread".to_string(),
                project_root: r"C:\work\project".to_string(),
                last_activity_at: Some(last_activity_at.into()),
            }],
        };

        let threads = proto_snapshot_threads(snapshot).unwrap();

        assert_eq!(
            threads,
            vec![HistoricalThread {
                session_id: SharedString::from("remote-session"),
                title: SharedString::from("Remote thread"),
                project_root: PathBuf::from(r"C:\work\project"),
                last_activity_at,
            }]
        );
    }

    #[gpui::test]
    async fn remote_index_fetch_is_one_projection_rpc_with_no_per_file_requests(
        _cx: &mut TestAppContext,
    ) {
        let client = Arc::new(HistoryStreamProtoClient::default());
        let mut snapshots = remote_indexed_history_stream(
            AnyProtoClient::new(client.clone()),
            "codex",
            PathBuf::from("/home/user/.codex"),
            vec![PathBuf::from("/work/project")],
            PathStyle::Posix,
        );

        let threads = snapshots
            .next()
            .await
            .expect("remote snapshot")
            .expect("remote index request should succeed");

        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].session_id, "remote-session");
        assert!(snapshots.next().await.is_none());
        assert_eq!(client.stream_request_count.load(Ordering::SeqCst), 1);
        assert_eq!(client.unary_request_count.load(Ordering::SeqCst), 0);
        let request = client
            .request
            .lock()
            .expect("request lock")
            .clone()
            .expect("history request");
        assert_eq!(request.kind, "codex");
        assert_eq!(request.normalized_history_root, "/home/user/.codex");
        assert_eq!(request.project_roots, vec!["/work/project"]);
    }

    struct ExtractProtoClient {
        request: Mutex<Option<proto::ExtractAgentTranscript>>,
        response: proto::AgentTranscriptExcerpt,
        handlers: parking_lot::Mutex<ProtoMessageHandlerSet>,
    }

    impl ExtractProtoClient {
        fn new(response: proto::AgentTranscriptExcerpt) -> Self {
            Self {
                request: Mutex::new(None),
                response,
                handlers: parking_lot::Mutex::new(ProtoMessageHandlerSet::default()),
            }
        }
    }

    impl ProtoClient for ExtractProtoClient {
        fn request(
            &self,
            envelope: proto::Envelope,
            request_type: &'static str,
        ) -> futures::future::BoxFuture<'static, Result<proto::Envelope>> {
            let recorded = proto::ExtractAgentTranscript::from_envelope(envelope);
            if let Ok(mut request) = self.request.lock() {
                *request = recorded;
            }
            let result = if request_type == proto::ExtractAgentTranscript::NAME {
                Ok(self.response.clone().into_envelope(0, None, None))
            } else {
                Err(anyhow!("unexpected unary request: {request_type}"))
            };
            async move { result }.boxed()
        }

        fn request_stream(
            &self,
            _envelope: proto::Envelope,
            request_type: &'static str,
        ) -> futures::future::BoxFuture<
            'static,
            Result<futures::stream::BoxStream<'static, Result<proto::Envelope>>>,
        > {
            let request_type = request_type.to_string();
            async move { anyhow::bail!("unexpected stream request: {request_type}") }.boxed()
        }

        fn send(&self, _envelope: proto::Envelope, _message_type: &'static str) -> Result<()> {
            Ok(())
        }

        fn send_response(
            &self,
            _envelope: proto::Envelope,
            _message_type: &'static str,
        ) -> Result<()> {
            Ok(())
        }

        fn message_handler_set(&self) -> &parking_lot::Mutex<ProtoMessageHandlerSet> {
            &self.handlers
        }

        fn is_via_collab(&self) -> bool {
            false
        }

        fn has_wsl_interop(&self) -> bool {
            false
        }
    }

    #[gpui::test]
    async fn local_extract_transcript_returns_rendered_excerpt(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let rollout_dir = Path::new("/home/user/.codex/sessions/2026/07/24");
        let rollout_path = rollout_dir.join("rollout-2026-07-24T10-00-00-aaa.jsonl");
        fs.create_dir(rollout_dir).await.unwrap();
        fs.insert_file(
            &rollout_path,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"aaa\",\"cwd\":\"/work/project\",\"timestamp\":\"2026-07-24T10:00:00.000Z\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"resume this task\"}]}}\n"
            )
            .as_bytes()
            .to_vec(),
        )
        .await;
        let service = agent_history::IndexService::new(
            fs.clone(),
            PathBuf::from("/home/user/.flint/cache/agent_threads"),
        );
        let host = agent_history::HistoryHost {
            fs: Arc::new(agent_history::LocalHistoryFs(fs)),
            base_dir: PathBuf::from("/home/user/.codex"),
            path_style: PathStyle::Posix,
        };

        let excerpt = local_extract_transcript(
            service,
            agent_history::HistoryKind::Codex,
            host,
            "aaa".to_string(),
            Some("/work/project".to_string()),
        )
        .await
        .expect("extraction should succeed")
        .expect("expected an excerpt");
        assert!(excerpt.markdown.contains("**User:** resume this task"));
    }

    #[gpui::test]
    async fn remote_extract_transcript_sends_request_and_parses_response(_cx: &mut TestAppContext) {
        let response = proto::AgentTranscriptExcerpt {
            found: true,
            markdown: "**User:** remote task".to_string(),
            degraded: true,
            possibly_incomplete: false,
            malformed_count: 0,
            unknown_count: 2,
            included_turns: 3,
            omitted_turns: 1,
        };
        let client = Arc::new(ExtractProtoClient::new(response));
        let excerpt = remote_extract_transcript(
            AnyProtoClient::new(client.clone()),
            "codex",
            PathBuf::from("/home/user/.codex"),
            "remote-session".to_string(),
            Some("/work/project".to_string()),
            PathStyle::Posix,
        )
        .await
        .expect("request should succeed")
        .expect("expected an excerpt");

        assert_eq!(excerpt.markdown, "**User:** remote task");
        assert!(excerpt.degraded);
        assert_eq!(excerpt.unknown_count, 2);

        let request = client
            .request
            .lock()
            .expect("request lock")
            .clone()
            .expect("extract request");
        assert_eq!(request.kind, "codex");
        assert_eq!(request.normalized_history_root, "/home/user/.codex");
        assert_eq!(request.session_id, "remote-session");
        assert_eq!(request.working_dir.as_deref(), Some("/work/project"));
    }

    #[gpui::test]
    async fn remote_extract_transcript_maps_not_found_to_none(_cx: &mut TestAppContext) {
        let response = proto::AgentTranscriptExcerpt {
            found: false,
            ..Default::default()
        };
        let client = Arc::new(ExtractProtoClient::new(response));
        let excerpt = remote_extract_transcript(
            AnyProtoClient::new(client),
            "codex",
            PathBuf::from("/home/user/.codex"),
            "missing".to_string(),
            None,
            PathStyle::Posix,
        )
        .await
        .expect("request should succeed");
        assert!(excerpt.is_none());
    }

    // `ProjectEnvironment::get_cli_environment` always returns an empty map
    // in test builds (see `crates/project/src/environment.rs`), so a
    // `Project::test()`-backed project deterministically hits the "no HOME"
    // failure path below -- the same path a real connection failure (e.g. a
    // dropped remote session) would hit, since both surface as
    // `directory_environment` failing to produce a usable env map.
    #[gpui::test]
    async fn resolve_history_base_dir_surfaces_unresolvable_environment(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
        let fs = FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [Path::new("/root")], cx).await;

        let result = cx
            .update(|cx| {
                cx.spawn(async move |cx| {
                    resolve_history_base_dir(&project, "CODEX_HOME", None, ".codex", cx).await
                })
            })
            .await;

        assert!(result.is_err());
    }
}
