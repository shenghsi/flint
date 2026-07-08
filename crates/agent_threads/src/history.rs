use std::any::{Any, TypeId};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use collections::HashMap;
use futures::StreamExt;
use gpui::{App, AsyncApp, Entity, SharedString};
use project::Project;
use rpc::AnyProtoClient;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use util::paths::PathStyle;

use crate::AgentLaunchCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoricalThread {
    pub session_id: SharedString,
    pub title: SharedString,
    pub project_root: PathBuf,
    pub last_activity_at: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryFileIdentity {
    modified_at: fs::MTime,
    length: u64,
}

#[derive(Default)]
pub struct HistoryParseCache {
    files: Mutex<HashMap<PathBuf, CachedParsedFile>>,
    /// When present, per-file parse results are also persisted to a local
    /// JSON file so that a cold start (fresh app launch) can reuse them
    /// instead of re-loading and re-parsing every history file. `None` keeps
    /// the cache purely in-memory (used by tests).
    disk: Option<DiskCache>,
}

struct CachedParsedFile {
    identity: HistoryFileIdentity,
    values: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

const MAX_CACHED_HISTORY_FILES: usize = 512;
const PERSISTED_CACHE_VERSION: u32 = 1;

/// Local-filesystem backing for a [`HistoryParseCache`]. The cache file is
/// always on the local machine even when the history being scanned lives on a
/// remote host, since its only purpose is to avoid re-doing local CPU work and
/// (for remote projects) re-downloading unchanged files.
struct DiskCache {
    fs: Arc<dyn fs::Fs>,
    path: PathBuf,
    /// The last-persisted snapshot, lazily read once and then reused to
    /// satisfy cold-start lookups. `None` until the first read attempt.
    loaded: Mutex<Option<HashMap<PathBuf, PersistedEntry>>>,
    /// Entries touched (reused or freshly parsed) since the last flush. A
    /// flush writes exactly these and rotates them into `loaded`, which prunes
    /// entries for history files that are no longer being scanned.
    pending: Mutex<HashMap<PathBuf, PersistedEntry>>,
}

#[derive(Clone)]
struct PersistedEntry {
    identity: HistoryFileIdentity,
    value: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct PersistedCacheFile {
    version: u32,
    entries: Vec<PersistedCacheEntry>,
}

#[derive(Serialize, Deserialize)]
struct PersistedCacheEntry {
    path: PathBuf,
    modified_at_secs: u64,
    modified_at_nanos: u32,
    length: u64,
    value: serde_json::Value,
}

/// The filesystem operations agent history scanning needs, abstracted over
/// local and remote projects. `Project::fs()` always refers to the local
/// machine's filesystem even when the project is remote, so remote projects
/// proxy these calls over RPC instead.
#[async_trait]
pub trait HistoryFs: Send + Sync {
    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;
    async fn load(&self, path: &Path) -> Result<String>;
    async fn metadata(&self, path: &Path) -> Result<Option<HistoryFileIdentity>>;
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

    async fn metadata(&self, path: &Path) -> Result<Option<HistoryFileIdentity>> {
        Ok(self
            .0
            .metadata(path)
            .await?
            .map(|metadata| HistoryFileIdentity {
                modified_at: metadata.mtime,
                length: metadata.len,
            }))
    }
}

/// Talks to the project's remote host directly over RPC via a captured
/// `AnyProtoClient`, rather than through the `Project` entity: entities can
/// only be accessed from the GPUI foreground thread (`AsyncApp` holds an
/// `Rc`), which conflicts with the `Send` futures `#[async_trait]` requires
/// here. `AnyProtoClient` has no such restriction.
struct RemoteHistoryFs {
    proto_client: AnyProtoClient,
    path_style: PathStyle,
}

#[async_trait]
impl HistoryFs for RemoteHistoryFs {
    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let response = self
            .proto_client
            .request(proto::ListRemoteDirectory {
                dev_server_id: proto::REMOTE_SERVER_PROJECT_ID,
                path: path_for_style(path, self.path_style)?,
                config: None,
            })
            .await?;
        Ok(response
            .entries
            .into_iter()
            .map(|entry| self.path_style.join_path(path, entry))
            .collect::<Result<Vec<_>>>()?)
    }

    async fn load(&self, path: &Path) -> Result<String> {
        Ok(String::from_utf8(self.load_bytes(path).await?)?)
    }

    async fn metadata(&self, path: &Path) -> Result<Option<HistoryFileIdentity>> {
        let response = self
            .proto_client
            .request(proto::GetPathMetadata {
                project_id: proto::REMOTE_SERVER_PROJECT_ID,
                path: path_for_style(path, self.path_style)?,
            })
            .await?;
        let Some(modified_at) = response.mtime else {
            return Ok(None);
        };
        let Some(length) = response.len else {
            return Ok(None);
        };
        Ok(Some(HistoryFileIdentity {
            modified_at: modified_at.into(),
            length,
        }))
    }
}

impl RemoteHistoryFs {
    async fn load_bytes(&self, path: &Path) -> Result<Vec<u8>> {
        let response = self
            .proto_client
            .request(proto::ReadRemoteFile {
                dev_server_id: proto::REMOTE_SERVER_PROJECT_ID,
                path: path_for_style(path, self.path_style)?,
            })
            .await?;
        Ok(response.content)
    }
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
        PathStyle::Windows => path.to_string(),
    };
    path_style.normalize(&path)
}

pub(crate) fn paths_equal_for_style(left: &Path, right: &Path, path_style: PathStyle) -> bool {
    let Some(left) = left.to_str() else {
        return left == right;
    };
    let Some(right) = right.to_str() else {
        return left == right;
    };
    normalize_path_for_style(left, path_style) == normalize_path_for_style(right, path_style)
}

/// The host-resolved filesystem and base config directory (e.g.
/// `~/.claude`) to scan. For a remote project both come from the remote
/// host; for a local project both come from the local machine.
pub struct AgentHistoryHost {
    pub fs: Arc<dyn HistoryFs>,
    pub base_dir: PathBuf,
    pub(crate) cache: Arc<HistoryParseCache>,
    pub(crate) path_style: PathStyle,
}

impl AgentHistoryHost {
    pub fn join(&self, path: impl AsRef<Path>, child: impl AsRef<Path>) -> Result<PathBuf> {
        let path = path_for_style(path.as_ref(), self.path_style)?;
        self.path_style.join_path(path, child)
    }
}

impl HistoryParseCache {
    /// Builds a cache that also persists per-file parse results to `path` on
    /// the given local filesystem.
    pub fn with_disk(fs: Arc<dyn fs::Fs>, path: PathBuf) -> Self {
        Self {
            files: Mutex::default(),
            disk: Some(DiskCache {
                fs,
                path,
                loaded: Mutex::new(None),
                pending: Mutex::new(HashMap::default()),
            }),
        }
    }

    fn memory_get<T: Any + Send + Sync>(
        &self,
        path: &Path,
        identity: HistoryFileIdentity,
    ) -> Result<Option<Arc<T>>> {
        let cached = self
            .files
            .lock()
            .map_err(|_| anyhow!("agent history parse cache lock poisoned"))?
            .get(path)
            .filter(|entry| entry.identity == identity)
            .and_then(|entry| entry.values.get(&TypeId::of::<T>()))
            .cloned();
        match cached {
            Some(cached) => {
                Ok(Some(Arc::downcast(cached).map_err(|_| {
                    anyhow!("agent history cache value type mismatch")
                })?))
            }
            None => Ok(None),
        }
    }

    fn memory_put<T: Any + Send + Sync>(
        &self,
        path: &Path,
        identity: HistoryFileIdentity,
        parsed: Arc<T>,
    ) -> Result<()> {
        let mut files = self
            .files
            .lock()
            .map_err(|_| anyhow!("agent history parse cache lock poisoned"))?;
        if files.len() >= MAX_CACHED_HISTORY_FILES && !files.contains_key(path) {
            if let Some(path_to_evict) = files.keys().next().cloned() {
                files.remove(&path_to_evict);
            }
        }
        let entry = files
            .entry(path.to_path_buf())
            .or_insert_with(|| CachedParsedFile {
                identity,
                values: HashMap::default(),
            });
        if entry.identity != identity {
            entry.identity = identity;
            entry.values.clear();
        }
        entry.values.insert(TypeId::of::<T>(), parsed);
        Ok(())
    }

    /// Returns the persisted value for `path` if one exists with a matching
    /// identity, recording the hit so the entry survives the next [`flush`].
    async fn disk_get(
        &self,
        path: &Path,
        identity: HistoryFileIdentity,
    ) -> Option<serde_json::Value> {
        let disk = self.disk.as_ref()?;
        disk.ensure_loaded().await;
        let entry = disk
            .loaded
            .lock()
            .ok()?
            .as_ref()?
            .get(path)
            .filter(|entry| entry.identity == identity)
            .cloned()?;
        if let Ok(mut pending) = disk.pending.lock() {
            pending.insert(path.to_path_buf(), entry.clone());
        }
        Some(entry.value)
    }

    fn disk_put(&self, path: &Path, identity: HistoryFileIdentity, value: serde_json::Value) {
        let Some(disk) = self.disk.as_ref() else {
            return;
        };
        if let Ok(mut pending) = disk.pending.lock() {
            pending.insert(path.to_path_buf(), PersistedEntry { identity, value });
        }
    }

    /// Writes the entries touched since the last flush to disk and rotates
    /// them into the loaded snapshot, pruning entries for files that were not
    /// touched (e.g. history files that have since been deleted). No-op for an
    /// in-memory-only cache.
    pub async fn flush(&self) -> Result<()> {
        let Some(disk) = self.disk.as_ref() else {
            return Ok(());
        };
        let pending = disk
            .pending
            .lock()
            .map_err(|_| anyhow!("agent history disk cache lock poisoned"))?
            .clone();
        // Avoid rewriting the file when this scan reproduced the persisted set
        // unchanged -- the watcher fires on any activity under the config dir
        // (e.g. an unrelated project's session), and a matching identity for
        // every path means the parsed values are identical too.
        let unchanged = disk
            .loaded
            .lock()
            .ok()
            .and_then(|loaded| {
                loaded.as_ref().map(|loaded| {
                    loaded.len() == pending.len()
                        && pending.iter().all(|(path, entry)| {
                            loaded
                                .get(path)
                                .is_some_and(|existing| existing.identity == entry.identity)
                        })
                })
            })
            .unwrap_or(false);
        if unchanged {
            if let Ok(mut pending) = disk.pending.lock() {
                pending.clear();
            }
            return Ok(());
        }
        let entries = pending
            .iter()
            .filter_map(|(path, entry)| {
                let (modified_at_secs, modified_at_nanos) = entry
                    .identity
                    .modified_at
                    .to_seconds_and_nanos_for_persistence()?;
                Some(PersistedCacheEntry {
                    path: path.clone(),
                    modified_at_secs,
                    modified_at_nanos,
                    length: entry.identity.length,
                    value: entry.value.clone(),
                })
            })
            .collect::<Vec<_>>();
        let text = serde_json::to_string(&PersistedCacheFile {
            version: PERSISTED_CACHE_VERSION,
            entries,
        })?;
        if let Some(parent) = disk.path.parent() {
            disk.fs.create_dir(parent).await?;
        }
        disk.fs.atomic_write(disk.path.clone(), text).await?;
        if let Ok(mut loaded) = disk.loaded.lock() {
            *loaded = Some(pending);
        }
        if let Ok(mut pending) = disk.pending.lock() {
            pending.clear();
        }
        Ok(())
    }
}

impl DiskCache {
    async fn ensure_loaded(&self) {
        if self
            .loaded
            .lock()
            .map(|loaded| loaded.is_some())
            .unwrap_or(true)
        {
            return;
        }
        let map = match self.fs.load(&self.path).await {
            Ok(text) => parse_persisted_cache(&text),
            // A missing or unreadable cache file just means a cold start.
            Err(_) => HashMap::default(),
        };
        if let Ok(mut loaded) = self.loaded.lock() {
            if loaded.is_none() {
                *loaded = Some(map);
            }
        }
    }
}

fn parse_persisted_cache(text: &str) -> HashMap<PathBuf, PersistedEntry> {
    let Ok(file) = serde_json::from_str::<PersistedCacheFile>(text) else {
        return HashMap::default();
    };
    if file.version != PERSISTED_CACHE_VERSION {
        return HashMap::default();
    }
    file.entries
        .into_iter()
        .map(|entry| {
            (
                entry.path,
                PersistedEntry {
                    identity: HistoryFileIdentity {
                        modified_at: fs::MTime::from_seconds_and_nanos(
                            entry.modified_at_secs,
                            entry.modified_at_nanos,
                        ),
                        length: entry.length,
                    },
                    value: entry.value,
                },
            )
        })
        .collect()
}

impl AgentHistoryHost {
    pub async fn parse_file<T>(&self, path: &Path, parse: impl FnOnce(&str) -> T) -> Result<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        let identity = self.fs.metadata(path).await?;
        if let Some(identity) = identity {
            if let Some(cached) = self.cache.memory_get::<T>(path, identity)? {
                return Ok(cached);
            }
        }

        let content = self.fs.load(path).await?;
        let parsed = Arc::new(parse(&content));

        if let Some(identity) = identity {
            self.cache.memory_put(path, identity, parsed.clone())?;
        }

        Ok(parsed)
    }

    /// Like [`parse_file`], but additionally persists the parse result to the
    /// cache's local backing file so a later cold start can reuse it without
    /// re-reading (or, for remote projects, re-downloading) the source file.
    /// Use this for per-session history files, which dominate scan cost.
    pub async fn parse_file_persistent<T>(
        &self,
        path: &Path,
        parse: impl FnOnce(&str) -> T,
    ) -> Result<Arc<T>>
    where
        T: Any + Send + Sync + Serialize + DeserializeOwned,
    {
        let identity = self.fs.metadata(path).await?;
        if let Some(identity) = identity {
            if let Some(cached) = self.cache.memory_get::<T>(path, identity)? {
                // Re-record so this entry survives the next flush even though
                // the parse was served from memory; otherwise a second scan in
                // the same run would drop it from the persisted file.
                if let Ok(value) = serde_json::to_value(&*cached) {
                    self.cache.disk_put(path, identity, value);
                }
                return Ok(cached);
            }
            if let Some(value) = self.cache.disk_get(path, identity).await {
                if let Ok(parsed) = serde_json::from_value::<T>(value) {
                    let parsed = Arc::new(parsed);
                    self.cache.memory_put(path, identity, parsed.clone())?;
                    return Ok(parsed);
                }
            }
        }

        let content = self.fs.load(path).await?;
        let parsed = Arc::new(parse(&content));

        if let Some(identity) = identity {
            self.cache.memory_put(path, identity, parsed.clone())?;
            if let Ok(value) = serde_json::to_value(&*parsed) {
                self.cache.disk_put(path, identity, value);
            }
        }

        Ok(parsed)
    }

    /// Persists the parse results accumulated during a scan to the cache's
    /// local backing file. Call once after a scan completes.
    pub async fn flush_cache(&self) -> Result<()> {
        self.cache.flush().await
    }
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
    cache: Arc<HistoryParseCache>,
    cx: &mut AsyncApp,
) -> Result<AgentHistoryHost> {
    let (fs, path_style) = project.read_with(cx, |project, cx| {
        let path_style = project.path_style(cx);
        let fs: Arc<dyn HistoryFs> = match project.remote_client() {
            Some(remote_client) => Arc::new(RemoteHistoryFs {
                proto_client: remote_client.read(cx).proto_client(),
                path_style,
            }),
            None => Arc::new(LocalHistoryFs(project.fs().clone())),
        };
        (fs, path_style)
    });

    let base_dir = resolve_history_base_dir(project, env_var_name, default_dir_name, cx).await?;

    Ok(AgentHistoryHost {
        fs,
        base_dir,
        cache,
        path_style,
    })
}

/// Resolves just the base config directory (e.g. `~/.claude`) for an agent,
/// without building a full [`AgentHistoryHost`]. Used to set up filesystem
/// watching on local projects, where only the directory to watch is needed.
pub async fn resolve_history_base_dir(
    project: &Entity<Project>,
    env_var_name: &str,
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

    base_dir_from_env(&env_map, env_var_name, default_dir_name, path_style)
}

/// Picks `$<env_var_name>` when set, otherwise `$HOME/<default_dir_name>`.
/// Pulled out of `resolve_history_host` so it's testable without needing a
/// real `Project`/environment resolution round trip.
fn base_dir_from_env(
    env_map: &HashMap<String, String>,
    env_var_name: &str,
    default_dir_name: &str,
    path_style: PathStyle,
) -> Result<PathBuf> {
    if let Some(override_dir) = env_map.get(env_var_name) {
        Ok(PathBuf::from(normalize_path_for_style(
            override_dir,
            path_style,
        )))
    } else {
        let home = env_map
            .get("HOME")
            .ok_or_else(|| anyhow!("no HOME in the project's resolved environment"))?;
        path_style.join_path(home, default_dir_name)
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
    use async_trait::async_trait;
    use fs::Fs as _;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use project::FakeFs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingHistoryFs {
        content: String,
        identity: Option<HistoryFileIdentity>,
        load_count: AtomicUsize,
    }

    struct ChangingIdentityHistoryFs {
        identity_seconds: AtomicUsize,
        load_count: AtomicUsize,
    }

    #[async_trait]
    impl HistoryFs for CountingHistoryFs {
        async fn read_dir(&self, _path: &Path) -> Result<Vec<PathBuf>> {
            Ok(Vec::new())
        }

        async fn load(&self, _path: &Path) -> Result<String> {
            self.load_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.content.clone())
        }

        async fn metadata(&self, _path: &Path) -> Result<Option<HistoryFileIdentity>> {
            Ok(self.identity)
        }
    }

    #[async_trait]
    impl HistoryFs for ChangingIdentityHistoryFs {
        async fn read_dir(&self, _path: &Path) -> Result<Vec<PathBuf>> {
            Ok(Vec::new())
        }

        async fn load(&self, _path: &Path) -> Result<String> {
            self.load_count.fetch_add(1, Ordering::SeqCst);
            Ok("one".to_string())
        }

        async fn metadata(&self, _path: &Path) -> Result<Option<HistoryFileIdentity>> {
            Ok(Some(HistoryFileIdentity {
                modified_at: fs::MTime::from_seconds_and_nanos(
                    self.identity_seconds.load(Ordering::SeqCst) as u64,
                    0,
                ),
                length: 3,
            }))
        }
    }

    #[test]
    fn base_dir_uses_override_when_set() {
        let mut env = HashMap::default();
        env.insert("CODEX_HOME".to_string(), "/custom/codex-home".to_string());
        env.insert("HOME".to_string(), "/home/alice".to_string());

        let base_dir = base_dir_from_env(&env, "CODEX_HOME", ".codex", PathStyle::Posix).unwrap();

        assert_eq!(base_dir, PathBuf::from("/custom/codex-home"));
    }

    #[test]
    fn base_dir_falls_back_to_home_when_override_unset() {
        let mut env = HashMap::default();
        env.insert("HOME".to_string(), "/home/alice".to_string());

        let base_dir = base_dir_from_env(&env, "CODEX_HOME", ".codex", PathStyle::Posix).unwrap();

        assert_eq!(base_dir, PathBuf::from("/home/alice/.codex"));
    }

    #[test]
    fn base_dir_errors_when_home_and_override_both_unset() {
        let env = HashMap::default();

        let result = base_dir_from_env(&env, "CODEX_HOME", ".codex", PathStyle::Posix);

        assert!(result.is_err());
    }

    #[test]
    fn base_dir_uses_project_path_style_when_falling_back_to_home() {
        let mut env = HashMap::default();
        env.insert("HOME".to_string(), "/home/alice".to_string());

        let base_dir = base_dir_from_env(&env, "CODEX_HOME", ".codex", PathStyle::Posix).unwrap();

        assert_eq!(base_dir.to_string_lossy(), "/home/alice/.codex");
    }

    #[test]
    fn base_dir_does_not_use_the_client_platform_separator() {
        let mut env = HashMap::default();
        env.insert("HOME".to_string(), "C:\\Users\\alice".to_string());

        let base_dir = base_dir_from_env(&env, "CODEX_HOME", ".codex", PathStyle::Windows).unwrap();

        assert_eq!(base_dir.to_string_lossy(), "C:\\Users\\alice\\.codex");
    }

    #[gpui::test]
    async fn parse_file_reuses_a_value_when_identity_is_unchanged(_cx: &mut TestAppContext) {
        let fs = Arc::new(CountingHistoryFs {
            content: "one\ntwo".to_string(),
            identity: Some(HistoryFileIdentity {
                modified_at: fs::MTime::from_seconds_and_nanos(1, 0),
                length: 7,
            }),
            load_count: AtomicUsize::new(0),
        });
        let host = AgentHistoryHost {
            fs: fs.clone(),
            base_dir: PathBuf::from("/history"),
            cache: Arc::new(HistoryParseCache::default()),
            path_style: PathStyle::Posix,
        };

        let first = host
            .parse_file(Path::new("/history/file"), |content| {
                content.lines().count()
            })
            .await
            .unwrap();
        let second = host
            .parse_file(Path::new("/history/file"), |content| {
                content.lines().count()
            })
            .await
            .unwrap();

        assert_eq!(*first, 2);
        assert_eq!(*second, 2);
        assert_eq!(fs.load_count.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    async fn parse_file_does_not_cache_without_identity(_cx: &mut TestAppContext) {
        let fs = Arc::new(CountingHistoryFs {
            content: "one".to_string(),
            identity: None,
            load_count: AtomicUsize::new(0),
        });
        let host = AgentHistoryHost {
            fs: fs.clone(),
            base_dir: PathBuf::from("/history"),
            cache: Arc::new(HistoryParseCache::default()),
            path_style: PathStyle::Posix,
        };

        host.parse_file(Path::new("/history/file"), str::len)
            .await
            .unwrap();
        host.parse_file(Path::new("/history/file"), str::len)
            .await
            .unwrap();

        assert_eq!(fs.load_count.load(Ordering::SeqCst), 2);
    }

    #[gpui::test]
    async fn parse_file_reloads_when_identity_changes(_cx: &mut TestAppContext) {
        let fs = Arc::new(ChangingIdentityHistoryFs {
            identity_seconds: AtomicUsize::new(1),
            load_count: AtomicUsize::new(0),
        });
        let host = AgentHistoryHost {
            fs: fs.clone(),
            base_dir: PathBuf::from("/history"),
            cache: Arc::new(HistoryParseCache::default()),
            path_style: PathStyle::Posix,
        };

        host.parse_file(Path::new("/history/file"), str::len)
            .await
            .unwrap();
        fs.identity_seconds.store(2, Ordering::SeqCst);
        host.parse_file(Path::new("/history/file"), str::len)
            .await
            .unwrap();

        assert_eq!(fs.load_count.load(Ordering::SeqCst), 2);
    }

    fn fixed_identity(seconds: u64, length: u64) -> Option<HistoryFileIdentity> {
        Some(HistoryFileIdentity {
            modified_at: fs::MTime::from_seconds_and_nanos(seconds, 0),
            length,
        })
    }

    async fn local_cache_fs(cx: &TestAppContext) -> Arc<FakeFs> {
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/cache")).await.unwrap();
        fs
    }

    fn disk_host(
        source: Arc<dyn HistoryFs>,
        local: Arc<dyn fs::Fs>,
        cache_path: &Path,
    ) -> AgentHistoryHost {
        AgentHistoryHost {
            fs: source,
            base_dir: PathBuf::from("/history"),
            cache: Arc::new(HistoryParseCache::with_disk(
                local,
                cache_path.to_path_buf(),
            )),
            path_style: PathStyle::Posix,
        }
    }

    #[gpui::test]
    async fn parse_file_persistent_reuses_disk_cache_across_cold_starts(cx: &mut TestAppContext) {
        let source = Arc::new(CountingHistoryFs {
            content: "alpha".to_string(),
            identity: fixed_identity(1, 5),
            load_count: AtomicUsize::new(0),
        });
        let local = local_cache_fs(cx).await;
        let cache_path = PathBuf::from("/cache/claude.json");
        let source_path = Path::new("/history/session.jsonl");

        let host = disk_host(source.clone(), local.clone(), &cache_path);
        let first = host
            .parse_file_persistent(source_path, |content| content.len())
            .await
            .unwrap();
        host.flush_cache().await.unwrap();
        assert_eq!(*first, 5);
        assert_eq!(source.load_count.load(Ordering::SeqCst), 1);

        // A fresh in-memory cache backed by the same file (a "restart") serves
        // the value from disk without re-loading the source file.
        let host = disk_host(source.clone(), local.clone(), &cache_path);
        let second = host
            .parse_file_persistent(source_path, |content| content.len())
            .await
            .unwrap();
        assert_eq!(*second, 5);
        assert_eq!(source.load_count.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    async fn parse_file_persistent_reparses_when_identity_changes_on_disk(cx: &mut TestAppContext) {
        let local = local_cache_fs(cx).await;
        let cache_path = PathBuf::from("/cache/claude.json");
        let source_path = Path::new("/history/session.jsonl");

        let source = Arc::new(CountingHistoryFs {
            content: "alpha".to_string(),
            identity: fixed_identity(1, 5),
            load_count: AtomicUsize::new(0),
        });
        let host = disk_host(source.clone(), local.clone(), &cache_path);
        host.parse_file_persistent(source_path, |content| content.len())
            .await
            .unwrap();
        host.flush_cache().await.unwrap();

        // Same path, different identity (the file changed): the persisted entry
        // is stale, so the source is loaded again.
        let source = Arc::new(CountingHistoryFs {
            content: "alphabet".to_string(),
            identity: fixed_identity(2, 8),
            load_count: AtomicUsize::new(0),
        });
        let host = disk_host(source.clone(), local.clone(), &cache_path);
        let parsed = host
            .parse_file_persistent(source_path, |content| content.len())
            .await
            .unwrap();
        assert_eq!(*parsed, 8);
        assert_eq!(source.load_count.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    async fn flush_prunes_entries_not_touched_by_the_latest_scan(cx: &mut TestAppContext) {
        let source = Arc::new(CountingHistoryFs {
            content: "alpha".to_string(),
            identity: fixed_identity(1, 5),
            load_count: AtomicUsize::new(0),
        });
        let local = local_cache_fs(cx).await;
        let cache_path = PathBuf::from("/cache/claude.json");
        let path_a = Path::new("/history/a.jsonl");
        let path_b = Path::new("/history/b.jsonl");

        let host = disk_host(source.clone(), local.clone(), &cache_path);
        host.parse_file_persistent(path_a, |content| content.len())
            .await
            .unwrap();
        host.parse_file_persistent(path_b, |content| content.len())
            .await
            .unwrap();
        host.flush_cache().await.unwrap();
        assert_eq!(
            persisted_paths(&local, &cache_path).await,
            vec![path_a.to_path_buf(), path_b.to_path_buf()]
        );

        // A later scan that only touches `a` drops `b` from the persisted file.
        host.parse_file_persistent(path_a, |content| content.len())
            .await
            .unwrap();
        host.flush_cache().await.unwrap();
        assert_eq!(
            persisted_paths(&local, &cache_path).await,
            vec![path_a.to_path_buf()]
        );
    }

    #[gpui::test]
    async fn flush_skips_rewriting_when_the_scan_reproduces_the_same_set(cx: &mut TestAppContext) {
        let source = Arc::new(CountingHistoryFs {
            content: "alpha".to_string(),
            identity: fixed_identity(1, 5),
            load_count: AtomicUsize::new(0),
        });
        let local = local_cache_fs(cx).await;
        let cache_path = PathBuf::from("/cache/claude.json");
        let source_path = Path::new("/history/session.jsonl");

        let host = disk_host(source.clone(), local.clone(), &cache_path);
        host.parse_file_persistent(source_path, |content| content.len())
            .await
            .unwrap();
        host.flush_cache().await.unwrap();

        // Stand-in content that a real flush would clobber, letting us detect
        // whether the unchanged re-scan rewrote the file.
        local.write(&cache_path, b"sentinel").await.unwrap();
        host.parse_file_persistent(source_path, |content| content.len())
            .await
            .unwrap();
        host.flush_cache().await.unwrap();

        assert_eq!(local.load(&cache_path).await.unwrap(), "sentinel");
    }

    async fn persisted_paths(local: &Arc<FakeFs>, cache_path: &Path) -> Vec<PathBuf> {
        let text = local.load(cache_path).await.unwrap();
        let file = serde_json::from_str::<PersistedCacheFile>(&text).unwrap();
        let mut paths = file
            .entries
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        paths.sort();
        paths
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
                    resolve_history_host(
                        &project,
                        "CODEX_HOME",
                        ".codex",
                        Arc::new(HistoryParseCache::default()),
                        cx,
                    )
                    .await
                })
            })
            .await;

        assert!(result.is_err());
    }
}
