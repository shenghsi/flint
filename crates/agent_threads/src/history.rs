use std::any::{Any, TypeId};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use collections::HashMap;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
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

pub(crate) type HistorySnapshotStream = BoxStream<'static, Result<Vec<HistoricalThread>>>;

pub(crate) async fn load_history_source<Legacy, LegacyFuture, Publish>(
    indexed: Option<HistorySnapshotStream>,
    legacy: Legacy,
    mut publish: Publish,
) -> Result<()>
where
    Legacy: FnOnce() -> LegacyFuture,
    LegacyFuture: Future<Output = Result<Vec<HistoricalThread>>>,
    Publish: FnMut(Vec<HistoricalThread>),
{
    if let Some(mut indexed) = indexed {
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
                    log::warn!(
                        "agent_threads: indexed history failed before a snapshot; falling back: {error:#}"
                    );
                    break;
                }
            }
        }
        if published_indexed_snapshot {
            return Ok(());
        }
    }

    publish(legacy().await?);
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
/// Shared by Agent Threads and the background session-discovery
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryFileIdentity {
    pub(crate) modified_at: fs::MTime,
    pub(crate) length: u64,
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

    fn local_path(&self, _path: &Path) -> Option<PathBuf> {
        None
    }
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

    fn local_path(&self, path: &Path) -> Option<PathBuf> {
        (!self.0.is_fake()).then(|| path.to_path_buf())
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
        PathStyle::Windows => path.replace('/', "\\"),
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

pub(crate) fn history_host_for_resolved_base_dir(
    project: &Entity<Project>,
    base_dir: PathBuf,
    cache: Arc<HistoryParseCache>,
    cx: &AsyncApp,
) -> AgentHistoryHost {
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

    AgentHistoryHost {
        fs,
        base_dir,
        cache,
        path_style,
    }
}

/// Resolves just the base config directory (e.g. `~/.claude`) for an agent,
/// without building a full [`AgentHistoryHost`]. Used to set up filesystem
/// watching on local projects, where only the directory to watch is needed.
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
    use async_trait::async_trait;
    use fs::Fs as _;
    use futures::FutureExt as _;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use project::FakeFs;
    use proto::EnvelopedMessage as _;
    use rpc::{ProtoClient, ProtoMessageHandlerSet};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct CountingHistoryFs {
        content: String,
        identity: Option<HistoryFileIdentity>,
        load_count: AtomicUsize,
    }

    struct ChangingIdentityHistoryFs {
        identity_seconds: AtomicUsize,
        load_count: AtomicUsize,
    }

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

    #[gpui::test]
    async fn indexed_snapshot_is_published_without_running_legacy(_cx: &mut TestAppContext) {
        let indexed_thread = HistoricalThread {
            session_id: SharedString::from("indexed"),
            title: SharedString::from("Indexed thread"),
            project_root: PathBuf::from("/work/project"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };
        let indexed = futures::stream::iter([Ok(vec![indexed_thread.clone()])]).boxed();
        let mut published = Vec::new();

        load_history_source(
            Some(indexed),
            || async { anyhow::bail!("legacy scan should not run") },
            |threads| published.push(threads),
        )
        .await
        .unwrap();

        assert_eq!(published, vec![vec![indexed_thread]]);
    }

    #[gpui::test]
    async fn indexed_error_before_a_snapshot_falls_back_to_legacy(_cx: &mut TestAppContext) {
        let legacy_thread = HistoricalThread {
            session_id: SharedString::from("legacy"),
            title: SharedString::from("Legacy thread"),
            project_root: PathBuf::from("/work/project"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };
        let indexed = futures::stream::iter([Err(anyhow!("index unavailable"))]).boxed();
        let mut published = Vec::new();

        load_history_source(
            Some(indexed),
            || {
                let legacy_thread = legacy_thread.clone();
                async move { Ok(vec![legacy_thread]) }
            },
            |threads| published.push(threads),
        )
        .await
        .unwrap();

        assert_eq!(published, vec![vec![legacy_thread]]);
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

        load_history_source(
            Some(indexed),
            || async { anyhow::bail!("legacy scan should not run after a snapshot") },
            |threads| published.push(threads),
        )
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

        load_history_source(
            Some(indexed),
            || async { anyhow::bail!("legacy scan should not run") },
            |threads| published.push(threads),
        )
        .await
        .unwrap();

        assert_eq!(published, vec![vec![cached_thread], vec![fresh_thread]]);
    }

    #[gpui::test]
    async fn absent_index_source_uses_legacy_history(_cx: &mut TestAppContext) {
        let legacy_thread = HistoricalThread {
            session_id: SharedString::from("legacy"),
            title: SharedString::from("Legacy thread"),
            project_root: PathBuf::from("/work/project"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };
        let mut published = Vec::new();

        load_history_source(
            None,
            || {
                let legacy_thread = legacy_thread.clone();
                async move { Ok(vec![legacy_thread]) }
            },
            |threads| published.push(threads),
        )
        .await
        .unwrap();

        assert_eq!(published, vec![vec![legacy_thread]]);
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
