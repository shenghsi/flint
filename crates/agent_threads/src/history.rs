use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use collections::HashMap;
use futures::{Stream, StreamExt};
use gpui::{App, AsyncApp, Entity, SharedString};
use project::Project;
use rpc::AnyProtoClient;

use crate::AgentLaunchCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoricalThread {
    pub session_id: SharedString,
    pub title: SharedString,
    pub project_root: PathBuf,
    pub last_activity_at: SystemTime,
}

/// The filesystem operations agent history scanning needs, abstracted over
/// local and remote projects. `Project::fs()` always refers to the local
/// machine's filesystem even when the project is remote, so remote projects
/// proxy these calls over RPC instead.
#[async_trait]
pub trait HistoryFs: Send + Sync {
    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;
    async fn load(&self, path: &Path) -> Result<String>;
    async fn open_sync(&self, path: &Path) -> Result<Box<dyn io::Read + Send + Sync>>;
    async fn watch(
        &self,
        path: &Path,
        latency: Duration,
    ) -> (
        Pin<Box<dyn Send + Stream<Item = Vec<fs::PathEvent>>>>,
        Arc<dyn fs::Watcher>,
    );
}

pub(crate) struct LocalHistoryFs(pub(crate) Arc<dyn fs::Fs>);

#[async_trait]
impl HistoryFs for LocalHistoryFs {
    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let mut entries = self.0.read_dir(path).await?;
        let mut paths = Vec::new();
        while let Some(entry) = entries.next().await {
            paths.push(entry?);
        }
        Ok(paths)
    }

    async fn load(&self, path: &Path) -> Result<String> {
        self.0.load(path).await
    }

    async fn open_sync(&self, path: &Path) -> Result<Box<dyn io::Read + Send + Sync>> {
        self.0.open_sync(path).await
    }

    async fn watch(
        &self,
        path: &Path,
        latency: Duration,
    ) -> (
        Pin<Box<dyn Send + Stream<Item = Vec<fs::PathEvent>>>>,
        Arc<dyn fs::Watcher>,
    ) {
        self.0.watch(path, latency).await
    }
}

/// Talks to the project's remote host directly over RPC via a captured
/// `AnyProtoClient`, rather than through the `Project` entity: entities can
/// only be accessed from the GPUI foreground thread (`AsyncApp` holds an
/// `Rc`), which conflicts with the `Send` futures `#[async_trait]` requires
/// here. `AnyProtoClient` has no such restriction.
struct RemoteHistoryFs {
    proto_client: AnyProtoClient,
    background_executor: gpui::BackgroundExecutor,
}

#[async_trait]
impl HistoryFs for RemoteHistoryFs {
    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let response = self
            .proto_client
            .request(proto::ListRemoteDirectory {
                dev_server_id: proto::REMOTE_SERVER_PROJECT_ID,
                path: path.to_string_lossy().into_owned(),
                config: None,
            })
            .await?;
        Ok(response
            .entries
            .into_iter()
            .map(|entry| path.join(entry))
            .collect())
    }

    async fn load(&self, path: &Path) -> Result<String> {
        Ok(String::from_utf8(self.load_bytes(path).await?)?)
    }

    async fn open_sync(&self, path: &Path) -> Result<Box<dyn io::Read + Send + Sync>> {
        Ok(Box::new(io::Cursor::new(self.load_bytes(path).await?)))
    }

    /// There's no remote file-watching mechanism for arbitrary paths, so
    /// this polls on `latency` instead of pushing real change events.
    async fn watch(
        &self,
        _path: &Path,
        latency: Duration,
    ) -> (
        Pin<Box<dyn Send + Stream<Item = Vec<fs::PathEvent>>>>,
        Arc<dyn fs::Watcher>,
    ) {
        let executor = self.background_executor.clone();
        let stream = futures::stream::unfold(executor, move |executor| async move {
            executor.timer(latency).await;
            Some((Vec::new(), executor))
        });
        (Box::pin(stream), Arc::new(NoopWatcher))
    }
}

impl RemoteHistoryFs {
    async fn load_bytes(&self, path: &Path) -> Result<Vec<u8>> {
        let response = self
            .proto_client
            .request(proto::ReadRemoteFile {
                dev_server_id: proto::REMOTE_SERVER_PROJECT_ID,
                path: path.to_string_lossy().into_owned(),
            })
            .await?;
        Ok(response.content)
    }
}

struct NoopWatcher;

impl fs::Watcher for NoopWatcher {
    fn add(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    fn remove(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
}

/// The host-resolved filesystem and base config directory (e.g.
/// `~/.claude`) to scan. For a remote project both come from the remote
/// host; for a local project both come from the local machine.
pub struct AgentHistoryHost {
    pub fs: Arc<dyn HistoryFs>,
    pub base_dir: PathBuf,
}

#[async_trait]
pub trait AgentHistoryProvider: Send + Sync {
    /// Scans persisted session history under `host`'s base directory,
    /// returning entries whose working directory matches one of
    /// `project_roots`.
    async fn scan(
        &self,
        host: &AgentHistoryHost,
        project_roots: &[PathBuf],
    ) -> Result<Vec<HistoricalThread>>;

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

/// Resolves the host-appropriate base directory for an agent's config,
/// honoring `env_var_name` (e.g. `CLAUDE_CONFIG_DIR`) when set, falling
/// back to `$HOME/<default_dir_name>`. Branches on local vs. remote
/// transparently via `Project::environment`'s directory environment
/// resolution, so the same code path is correct for both.
pub async fn resolve_history_host(
    project: &Entity<Project>,
    env_var_name: &str,
    default_dir_name: &str,
    cx: &mut AsyncApp,
) -> Result<AgentHistoryHost> {
    let (fs, environment, anchor_path) = project.read_with(cx, |project, cx| {
        let fs: Arc<dyn HistoryFs> = match project.remote_client() {
            Some(remote_client) => Arc::new(RemoteHistoryFs {
                proto_client: remote_client.read(cx).proto_client(),
                background_executor: cx.background_executor().clone(),
            }),
            None => Arc::new(LocalHistoryFs(project.fs().clone())),
        };
        (
            fs,
            project.environment().clone(),
            first_worktree_root(project, cx),
        )
    });
    let anchor_path = anchor_path.ok_or_else(|| anyhow!("project has no worktrees"))?;

    let env_task = environment.update(cx, |environment, cx| {
        environment.directory_environment(anchor_path, cx)
    });
    let env_map = env_task
        .await
        .ok_or_else(|| anyhow!("couldn't resolve the project's environment"))?;

    let base_dir = base_dir_from_env(&env_map, env_var_name, default_dir_name)?;

    Ok(AgentHistoryHost { fs, base_dir })
}

/// Picks `$<env_var_name>` when set, otherwise `$HOME/<default_dir_name>`.
/// Pulled out of `resolve_history_host` so it's testable without needing a
/// real `Project`/environment resolution round trip.
fn base_dir_from_env(
    env_map: &HashMap<String, String>,
    env_var_name: &str,
    default_dir_name: &str,
) -> Result<PathBuf> {
    if let Some(override_dir) = env_map.get(env_var_name) {
        Ok(PathBuf::from(override_dir))
    } else {
        let home = env_map
            .get("HOME")
            .ok_or_else(|| anyhow!("no HOME in the project's resolved environment"))?;
        Ok(PathBuf::from(home).join(default_dir_name))
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use project::FakeFs;
    use std::path::Path;

    #[test]
    fn base_dir_uses_override_when_set() {
        let mut env = HashMap::default();
        env.insert("CODEX_HOME".to_string(), "/custom/codex-home".to_string());
        env.insert("HOME".to_string(), "/home/alice".to_string());

        let base_dir = base_dir_from_env(&env, "CODEX_HOME", ".codex").unwrap();

        assert_eq!(base_dir, PathBuf::from("/custom/codex-home"));
    }

    #[test]
    fn base_dir_falls_back_to_home_when_override_unset() {
        let mut env = HashMap::default();
        env.insert("HOME".to_string(), "/home/alice".to_string());

        let base_dir = base_dir_from_env(&env, "CODEX_HOME", ".codex").unwrap();

        assert_eq!(base_dir, PathBuf::from("/home/alice/.codex"));
    }

    #[test]
    fn base_dir_errors_when_home_and_override_both_unset() {
        let env = HashMap::default();

        let result = base_dir_from_env(&env, "CODEX_HOME", ".codex");

        assert!(result.is_err());
    }

    // `ProjectEnvironment::get_cli_environment` always returns an empty map
    // in test builds (see `crates/project/src/environment.rs`), so a
    // `Project::test()`-backed project deterministically hits the "no HOME"
    // failure path below -- the same path a real connection failure (e.g. a
    // dropped remote session) would hit, since both surface as
    // `directory_environment` failing to produce a usable env map.
    #[gpui::test]
    async fn resolve_history_host_surfaces_unresolvable_environment(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
        let fs = FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [Path::new("/root")], cx).await;

        let result = cx
            .update(|cx| {
                cx.spawn(async move |cx| {
                    resolve_history_host(&project, "CODEX_HOME", ".codex", cx).await
                })
            })
            .await;

        assert!(result.is_err());
    }
}
