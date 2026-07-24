//! Host-owned agent thread history index.
//!
//! This crate performs agent history discovery, metadata validation, and
//! parsing on the host that owns the history, persists a normalized index, and
//! serves compact thread snapshots. It is used directly by the local Flint
//! application for local projects and by `flint-remote-server` for remote
//! projects, so it deliberately depends only on filesystem, serialization,
//! time, and collection primitives -- never on GPUI, projects, workspaces, or
//! RPC clients.
//!
//! See `docs/superpowers/specs/2026-07-24-host-agent-thread-history-index-design.md`.

mod claude;
mod codex;
mod pi;

#[cfg(any(test, feature = "test-support"))]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use collections::HashMap;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use util::paths::PathStyle;

pub use claude::ClaudeHistoryProvider;
pub use codex::CodexHistoryProvider;
pub use pi::PiHistoryProvider;

/// Bumped when the persisted envelope layout changes. A mismatch is a cache
/// miss with no migration.
const SCHEMA_VERSION: u32 = 1;
/// Bumped when any provider's parsing or normalization changes in a way that
/// would make previously persisted summaries wrong. A mismatch is a cache miss.
const PARSER_VERSION: u32 = 1;

/// A registered agent history kind. New kinds are added here as their provider
/// scanners land.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HistoryKind {
    Codex,
    Claude,
    Pi,
}

impl HistoryKind {
    pub fn id(self) -> &'static str {
        match self {
            HistoryKind::Codex => "codex",
            HistoryKind::Claude => "claude",
            HistoryKind::Pi => "pi",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "codex" => Some(HistoryKind::Codex),
            "claude" => Some(HistoryKind::Claude),
            "pi" => Some(HistoryKind::Pi),
            _ => None,
        }
    }

    fn provider(self) -> Arc<dyn HistoryProvider> {
        match self {
            HistoryKind::Codex => Arc::new(CodexHistoryProvider),
            HistoryKind::Claude => Arc::new(ClaudeHistoryProvider),
            HistoryKind::Pi => Arc::new(PiHistoryProvider),
        }
    }

    /// Whether the projection collapses multiple index rows that share a
    /// session id to the newest one. Codex shows one entry per rollout file
    /// (matching the legacy scanner), so it does not dedup; Claude and Pi keep
    /// the newest record per session.
    fn dedup_by_session(self) -> bool {
        match self {
            HistoryKind::Codex => false,
            HistoryKind::Claude | HistoryKind::Pi => true,
        }
    }
}

/// Identity used to detect whether a source file changed between refreshes,
/// stored in the persisted index alongside the parsed summary.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileIdentity {
    pub modified_at_secs: u64,
    pub modified_at_nanos: u32,
    pub length: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Option<Self> {
        let (modified_at_secs, modified_at_nanos) =
            metadata.mtime.to_seconds_and_nanos_for_persistence()?;
        Some(Self {
            modified_at_secs,
            modified_at_nanos,
            length: metadata.len,
        })
    }
}

/// The filesystem operations history scanning needs, abstracted so the same
/// provider code serves a host-local scan (`flint-remote-server` or a local
/// Flint) and, for the legacy client fallback, a remote proxy.
#[async_trait]
pub trait HistoryFs: Send + Sync {
    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;
    async fn load(&self, path: &Path) -> Result<String>;
    async fn metadata(&self, path: &Path) -> Result<Option<FileIdentity>>;
}

/// A [`HistoryFs`] backed by a real (host-local) filesystem.
pub struct LocalHistoryFs(pub Arc<dyn fs::Fs>);

#[async_trait]
impl HistoryFs for LocalHistoryFs {
    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let mut entries = self.0.read_dir(path).await?;
        let mut paths = Vec::new();
        while let Some(entry) = futures::StreamExt::next(&mut entries).await {
            paths.push(entry?);
        }
        Ok(paths)
    }

    async fn load(&self, path: &Path) -> Result<String> {
        self.0.load(path).await
    }

    async fn metadata(&self, path: &Path) -> Result<Option<FileIdentity>> {
        Ok(self
            .0
            .metadata(path)
            .await?
            .as_ref()
            .and_then(FileIdentity::from_metadata))
    }
}

/// The host-resolved filesystem, agent history root (e.g. `~/.codex`), and
/// path style to scan. Cheap to clone: `fs` is an `Arc`.
#[derive(Clone)]
pub struct HistoryHost {
    pub fs: Arc<dyn HistoryFs>,
    pub base_dir: PathBuf,
    pub path_style: PathStyle,
}

impl HistoryHost {
    pub fn join(&self, base: impl AsRef<Path>, child: impl AsRef<Path>) -> Result<PathBuf> {
        let base = path_for_style(base.as_ref(), self.path_style)?;
        self.path_style.join_path(base, child)
    }

    fn normalized_root(&self) -> Result<String> {
        Ok(normalize_path_for_style(
            self.base_dir
                .to_str()
                .with_context(|| format!("history root is not UTF-8: {:?}", self.base_dir))?,
            self.path_style,
        ))
    }
}

/// A normalized index row: one session projected onto one working directory.
/// Codex and Pi sessions have a single working directory (one row each);
/// Claude sessions that changed directories produce one row per directory, each
/// carrying the title and activity time the legacy scanner would compute when
/// that directory is the requested project root. This uniform per-directory
/// shape makes query-time filtering an exact directory match and keeps the
/// public projection identical across providers.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexedSession {
    pub session_id: String,
    /// The fully resolved title after applying provider title precedence,
    /// including the provider default; always present.
    pub resolved_title: String,
    /// The provider's fallback title derived from the session file itself
    /// (e.g. the first user message), retained for cheap title recomputation.
    pub fallback_title: Option<String>,
    /// The working directory this row is projected onto, in the host's path
    /// style.
    pub working_dir: String,
    pub last_activity_secs: u64,
    pub last_activity_nanos: u32,
}

impl IndexedSession {
    fn last_activity(&self) -> SystemTime {
        UNIX_EPOCH + std::time::Duration::new(self.last_activity_secs, self.last_activity_nanos)
    }
}

/// The result of a provider refresh: the new opaque source state to persist for
/// the next incremental refresh, and the normalized sessions.
pub struct ProviderRefresh {
    pub source_state: serde_json::Value,
    pub sessions: Vec<IndexedSession>,
}

/// A provider discovers and parses one agent kind's history, reusing unchanged
/// parses via the opaque `previous` source state it produced on the last
/// refresh.
#[async_trait]
pub trait HistoryProvider: Send + Sync {
    fn kind(&self) -> HistoryKind;

    async fn refresh(
        &self,
        host: &HistoryHost,
        previous: Option<&serde_json::Value>,
    ) -> Result<ProviderRefresh>;
}

/// Whether a snapshot came from the persisted index (immediately, before
/// revalidation) or from a completed refresh.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Freshness {
    Cached,
    Fresh,
}

/// The compact public projection of one session for one requested project root.
#[derive(Clone, PartialEq, Debug)]
pub struct SnapshotEntry {
    pub session_id: String,
    pub title: String,
    pub project_root: PathBuf,
    pub last_activity_at: SystemTime,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub freshness: Freshness,
    pub generation: u64,
    pub entries: Vec<SnapshotEntry>,
}

#[derive(Serialize, Deserialize)]
struct PersistedIndex {
    schema_version: u32,
    parser_version: u32,
    agent_kind: String,
    normalized_history_root: String,
    generation: u64,
    generated_at_secs: u64,
    provider_source_state: serde_json::Value,
    indexed_sessions: Vec<IndexedSession>,
}

impl PersistedIndex {
    /// Whether this persisted file is usable for `kind` at `normalized_root`.
    /// A schema/parser mismatch or a stored-root mismatch (hash collision or a
    /// moved cache file) is a cache miss.
    fn is_valid_for(&self, kind: HistoryKind, normalized_root: &str) -> bool {
        self.schema_version == SCHEMA_VERSION
            && self.parser_version == PARSER_VERSION
            && self.agent_kind == kind.id()
            && self.normalized_history_root == normalized_root
    }
}

type RefreshResult = std::result::Result<Arc<PersistedIndex>, Arc<anyhow::Error>>;
type SharedRefresh = Shared<BoxFuture<'static, RefreshResult>>;

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    kind: HistoryKind,
    root_key: String,
}

struct IndexServiceInner {
    fs: Arc<dyn fs::Fs>,
    cache_root: PathBuf,
    inflight: Mutex<HashMap<CacheKey, SharedRefresh>>,
}

/// The shared, host-local history index service. Cheap to clone (an `Arc`).
#[derive(Clone)]
pub struct IndexService(Arc<IndexServiceInner>);

impl IndexService {
    /// `cache_root` is the directory holding all agent-thread indexes on this
    /// host, e.g. `~/.flint/cache/agent_threads`. `fs` is the host-local
    /// filesystem used only for cache persistence.
    pub fn new(fs: Arc<dyn fs::Fs>, cache_root: PathBuf) -> Self {
        Self(Arc::new(IndexServiceInner {
            fs,
            cache_root,
            inflight: Mutex::new(HashMap::default()),
        }))
    }

    /// Returns a filtered snapshot from the persisted index without touching
    /// source files, or `None` when no valid persisted index exists (a cold
    /// start emits no cached snapshot).
    pub async fn cached_snapshot(
        &self,
        kind: HistoryKind,
        host: &HistoryHost,
        project_roots: &[PathBuf],
    ) -> Option<Snapshot> {
        let normalized_root = host.normalized_root().ok()?;
        let cache_path = self.0.cache_path(kind, &normalized_root);
        let index = self
            .0
            .load_valid_index(&cache_path, kind, &normalized_root)
            .await?;
        Some(filter_snapshot(
            &index,
            kind,
            project_roots,
            host.path_style,
            Freshness::Cached,
        ))
    }

    /// Discovers and parses source files on the host, persists a refreshed
    /// index, and returns a filtered `Fresh` snapshot. Concurrent callers for
    /// the same `(kind, history root)` within this process join one refresh.
    pub async fn refresh(
        &self,
        kind: HistoryKind,
        host: &HistoryHost,
        project_roots: &[PathBuf],
    ) -> Result<Snapshot> {
        let index = self.refresh_index(kind, host).await?;
        Ok(filter_snapshot(
            &index,
            kind,
            project_roots,
            host.path_style,
            Freshness::Fresh,
        ))
    }

    fn refresh_index(
        &self,
        kind: HistoryKind,
        host: &HistoryHost,
    ) -> impl std::future::Future<Output = Result<Arc<PersistedIndex>>> + 'static {
        let normalized_root = host.normalized_root();
        let inner = self.0.clone();
        let host = host.clone();
        async move {
            let normalized_root = normalized_root?;
            let key = CacheKey {
                kind,
                root_key: cache_key_hash(host.path_style, &normalized_root),
            };
            let shared = {
                let mut inflight = inner.inflight.lock();
                if let Some(existing) = inflight.get(&key).cloned() {
                    existing
                } else {
                    let refresh_inner = inner.clone();
                    let refresh_key = key.clone();
                    let refresh_host = host.clone();
                    let refresh_root = normalized_root.clone();
                    let future = async move {
                        let result = refresh_inner
                            .do_refresh(kind, &refresh_host, &refresh_root)
                            .await;
                        refresh_inner.inflight.lock().remove(&refresh_key);
                        result.map(Arc::new).map_err(Arc::new)
                    }
                    .boxed()
                    .shared();
                    inflight.insert(key.clone(), future.clone());
                    future
                }
            };
            shared.await.map_err(|error| anyhow!("{error:#}"))
        }
    }
}

impl IndexServiceInner {
    fn cache_path(&self, kind: HistoryKind, normalized_root: &str) -> PathBuf {
        self.cache_root
            .join(kind.id())
            .join(cache_key_hash(PathStyle::local(), normalized_root))
            .join("index.json")
    }

    async fn load_valid_index(
        &self,
        cache_path: &Path,
        kind: HistoryKind,
        normalized_root: &str,
    ) -> Option<Arc<PersistedIndex>> {
        let text = self.fs.load(cache_path).await.ok()?;
        let index = serde_json::from_str::<PersistedIndex>(&text).ok()?;
        index
            .is_valid_for(kind, normalized_root)
            .then(|| Arc::new(index))
    }

    async fn do_refresh(
        &self,
        kind: HistoryKind,
        host: &HistoryHost,
        normalized_root: &str,
    ) -> Result<PersistedIndex> {
        let cache_path = self.cache_path(kind, normalized_root);
        // Reload the latest valid index (another process may have written since
        // we last read) so an incremental refresh builds on the newest state.
        let previous = self
            .load_valid_index(&cache_path, kind, normalized_root)
            .await;
        let previous_state = previous.as_ref().map(|index| &index.provider_source_state);

        let refreshed = kind.provider().refresh(host, previous_state).await?;

        // Unchanged source state means the parsed results are identical; keep
        // the existing generation and avoid rewriting the file.
        if let Some(previous) = &previous {
            if previous.provider_source_state == refreshed.source_state
                && previous.indexed_sessions == refreshed.sessions
            {
                return Ok(PersistedIndex {
                    schema_version: SCHEMA_VERSION,
                    parser_version: PARSER_VERSION,
                    agent_kind: kind.id().to_string(),
                    normalized_history_root: normalized_root.to_string(),
                    generation: previous.generation,
                    generated_at_secs: previous.generated_at_secs,
                    provider_source_state: refreshed.source_state,
                    indexed_sessions: refreshed.sessions,
                });
            }
        }

        let index = PersistedIndex {
            schema_version: SCHEMA_VERSION,
            parser_version: PARSER_VERSION,
            agent_kind: kind.id().to_string(),
            normalized_history_root: normalized_root.to_string(),
            generation: previous.as_ref().map(|p| p.generation).unwrap_or(0) + 1,
            generated_at_secs: now_secs(),
            provider_source_state: refreshed.source_state,
            indexed_sessions: refreshed.sessions,
        };

        // An atomic-write failure still returns the freshly computed snapshot to
        // this requester; persistence is retried on a later refresh.
        if let Err(error) = self.persist(&cache_path, &index).await {
            log::warn!(
                "agent_history: failed to persist {} index: {error:#}",
                kind.id()
            );
        }
        Ok(index)
    }

    async fn persist(&self, cache_path: &Path, index: &PersistedIndex) -> Result<()> {
        if let Some(parent) = cache_path.parent() {
            self.fs.create_dir(parent).await?;
        }
        let text = serde_json::to_string(index)?;
        self.fs.atomic_write(cache_path.to_path_buf(), text).await
    }
}

fn filter_snapshot(
    index: &PersistedIndex,
    kind: HistoryKind,
    project_roots: &[PathBuf],
    path_style: PathStyle,
    freshness: Freshness,
) -> Snapshot {
    let mut entries = Vec::new();
    for session in &index.indexed_sessions {
        // Each row is projected onto exactly one working directory: with a
        // filter it must equal a requested root (and that root becomes the
        // entry's project_root); with no filter the row's own directory is used.
        let project_root = if project_roots.is_empty() {
            Some(PathBuf::from(&session.working_dir))
        } else {
            project_roots
                .iter()
                .find(|root| paths_equal_for_style(&session.working_dir, root, path_style))
                .cloned()
        };
        let Some(project_root) = project_root else {
            continue;
        };
        entries.push(SnapshotEntry {
            session_id: session.session_id.clone(),
            title: session.resolved_title.clone(),
            project_root,
            last_activity_at: session.last_activity(),
        });
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_activity_at));
    if kind.dedup_by_session() {
        // Keep the newest entry per session across the matched roots, matching
        // the legacy scanner's per-session dedup. Entries are already sorted
        // newest-first, so the first occurrence of each id wins.
        let mut seen = collections::HashSet::default();
        entries.retain(|entry| seen.insert(entry.session_id.clone()));
    }
    Snapshot {
        freshness,
        generation: index.generation,
        entries,
    }
}

fn cache_key_hash(path_style: PathStyle, normalized_root: &str) -> String {
    let mut hasher = Sha256::new();
    let style_tag: &[u8] = match path_style {
        PathStyle::Posix => b"posix:",
        PathStyle::Windows => b"windows:",
    };
    hasher.update(style_tag);
    hasher.update(normalized_root.as_bytes());
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0xf) as u32, 16).unwrap_or('0'));
    }
    out
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(crate) fn path_for_style(path: &Path, path_style: PathStyle) -> Result<String> {
    let path = path
        .to_str()
        .with_context(|| format!("path contains invalid UTF-8: {path:?}"))?;
    Ok(normalize_path_for_style(path, path_style))
}

pub(crate) fn normalize_path_for_style(path: &str, path_style: PathStyle) -> String {
    let path = match path_style {
        PathStyle::Posix => path.replace('\\', "/"),
        PathStyle::Windows => path.to_string(),
    };
    path_style.normalize(&path)
}

pub(crate) fn paths_equal_for_style(left: &str, right: &Path, path_style: PathStyle) -> bool {
    let Some(right) = right.to_str() else {
        return false;
    };
    normalize_path_for_style(left, path_style) == normalize_path_for_style(right, path_style)
}

/// A deterministic, in-memory [`HistoryFs`] for tests: maps absolute paths to
/// file contents and identities.
#[cfg(any(test, feature = "test-support"))]
pub struct InMemoryHistoryFs {
    files: Mutex<BTreeMap<String, (String, FileIdentity)>>,
    load_count: Mutex<usize>,
}

#[cfg(any(test, feature = "test-support"))]
impl InMemoryHistoryFs {
    pub fn new() -> Self {
        Self {
            files: Mutex::new(BTreeMap::new()),
            load_count: Mutex::new(0),
        }
    }

    pub fn insert(&self, path: &str, content: &str, identity: FileIdentity) {
        self.files
            .lock()
            .insert(path.to_string(), (content.to_string(), identity));
    }

    pub fn remove(&self, path: &str) {
        self.files.lock().remove(path);
    }

    pub fn load_count(&self) -> usize {
        *self.load_count.lock()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Default for InMemoryHistoryFs {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-support"))]
#[async_trait]
impl HistoryFs for InMemoryHistoryFs {
    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let prefix = format!("{}/", path.to_str().unwrap_or_default());
        let files = self.files.lock();
        let mut children: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        for key in files.keys() {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            let child = rest.split('/').next().unwrap_or_default();
            if !child.is_empty() {
                children.insert(PathBuf::from(format!("{prefix}{child}")));
            }
        }
        Ok(children.into_iter().collect())
    }

    async fn load(&self, path: &Path) -> Result<String> {
        *self.load_count.lock() += 1;
        let key = path.to_str().unwrap_or_default();
        self.files
            .lock()
            .get(key)
            .map(|(content, _)| content.clone())
            .ok_or_else(|| anyhow!("no such file: {key}"))
    }

    async fn metadata(&self, path: &Path) -> Result<Option<FileIdentity>> {
        let key = path.to_str().unwrap_or_default();
        Ok(self.files.lock().get(key).map(|(_, identity)| *identity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;

    fn identity(secs: u64, length: u64) -> FileIdentity {
        FileIdentity {
            modified_at_secs: secs,
            modified_at_nanos: 0,
            length,
        }
    }

    fn host(fs: Arc<dyn HistoryFs>) -> HistoryHost {
        HistoryHost {
            fs,
            base_dir: PathBuf::from("/home/user/.codex"),
            path_style: PathStyle::Posix,
        }
    }

    fn service(cx: &mut TestAppContext) -> IndexService {
        let fs = fs::FakeFs::new(cx.executor());
        IndexService::new(fs, PathBuf::from("/cache/agent_threads"))
    }

    fn session_meta(id: &str, cwd: &str, timestamp: &str) -> String {
        serde_json::json!({
            "type": "session_meta",
            "payload": { "id": id, "cwd": cwd, "timestamp": timestamp },
        })
        .to_string()
    }

    fn user_message(text: &str) -> String {
        serde_json::json!({
            "payload": { "type": "user_message", "message": text },
        })
        .to_string()
    }

    fn rollout(meta: &str, message: &str) -> String {
        format!("{meta}\n{message}\n")
    }

    #[gpui::test]
    async fn cold_refresh_builds_index_and_filters_by_project_root(cx: &mut TestAppContext) {
        let source = Arc::new(InMemoryHistoryFs::new());
        source.insert(
            "/home/user/.codex/sessions/2026/07/24/rollout-2026-07-24T10-00-00-aaa.jsonl",
            &rollout(
                &session_meta("aaa", "/work/project", "2026-07-24T10:00:00.000Z"),
                &user_message("first task"),
            ),
            identity(1, 10),
        );
        source.insert(
            "/home/user/.codex/sessions/2026/07/24/rollout-2026-07-24T09-00-00-bbb.jsonl",
            &rollout(
                &session_meta("bbb", "/other/project", "2026-07-24T09:00:00.000Z"),
                &user_message("other task"),
            ),
            identity(1, 10),
        );

        let service = service(cx);
        let host = host(source.clone());

        // No cached snapshot on a cold start.
        assert!(
            service
                .cached_snapshot(HistoryKind::Codex, &host, &[PathBuf::from("/work/project")])
                .await
                .is_none()
        );

        let snapshot = service
            .refresh(HistoryKind::Codex, &host, &[PathBuf::from("/work/project")])
            .await
            .unwrap();
        assert_eq!(snapshot.freshness, Freshness::Fresh);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].session_id, "aaa");
        assert_eq!(snapshot.entries[0].title, "first task");
        assert_eq!(
            snapshot.entries[0].project_root,
            PathBuf::from("/work/project")
        );

        // After a refresh persisted the index, a cached snapshot is available.
        let cached = service
            .cached_snapshot(HistoryKind::Codex, &host, &[PathBuf::from("/work/project")])
            .await
            .unwrap();
        assert_eq!(cached.freshness, Freshness::Cached);
        assert_eq!(cached.entries.len(), 1);
        assert_eq!(cached.entries[0].session_id, "aaa");
    }

    #[gpui::test]
    async fn incremental_refresh_reuses_unchanged_files(cx: &mut TestAppContext) {
        let source = Arc::new(InMemoryHistoryFs::new());
        source.insert(
            "/home/user/.codex/sessions/2026/07/24/rollout-2026-07-24T10-00-00-aaa.jsonl",
            &rollout(
                &session_meta("aaa", "/work/project", "2026-07-24T10:00:00.000Z"),
                &user_message("first task"),
            ),
            identity(1, 10),
        );

        let service = service(cx);
        let host = host(source.clone());
        let roots = [PathBuf::from("/work/project")];

        service
            .refresh(HistoryKind::Codex, &host, &roots)
            .await
            .unwrap();
        let loads_after_first = source.load_count();
        assert!(loads_after_first > 0);

        // A second refresh with unchanged files must not reload the rollout.
        service
            .refresh(HistoryKind::Codex, &host, &roots)
            .await
            .unwrap();
        assert_eq!(
            source.load_count(),
            loads_after_first,
            "unchanged rollout files should not be reloaded"
        );
    }

    #[gpui::test]
    async fn deleted_files_disappear_after_refresh(cx: &mut TestAppContext) {
        let source = Arc::new(InMemoryHistoryFs::new());
        let path = "/home/user/.codex/sessions/2026/07/24/rollout-2026-07-24T10-00-00-aaa.jsonl";
        source.insert(
            path,
            &rollout(
                &session_meta("aaa", "/work/project", "2026-07-24T10:00:00.000Z"),
                &user_message("first task"),
            ),
            identity(1, 10),
        );

        let service = service(cx);
        let host = host(source.clone());
        let roots = [PathBuf::from("/work/project")];

        let snapshot = service
            .refresh(HistoryKind::Codex, &host, &roots)
            .await
            .unwrap();
        assert_eq!(snapshot.entries.len(), 1);

        source.remove(path);
        let snapshot = service
            .refresh(HistoryKind::Codex, &host, &roots)
            .await
            .unwrap();
        assert_eq!(snapshot.entries.len(), 0);
    }

    #[gpui::test]
    async fn different_roots_use_different_cache_files(cx: &mut TestAppContext) {
        let service = service(cx);
        let normalized_a = "/home/user/.codex".to_string();
        let normalized_b = "/home/user/.codex-work".to_string();
        let path_a = service.0.cache_path(HistoryKind::Codex, &normalized_a);
        let path_b = service.0.cache_path(HistoryKind::Codex, &normalized_b);
        assert_ne!(path_a, path_b);
    }

    fn host_at(fs: Arc<dyn HistoryFs>, base_dir: &str) -> HistoryHost {
        HistoryHost {
            fs,
            base_dir: PathBuf::from(base_dir),
            path_style: PathStyle::Posix,
        }
    }

    fn claude_history_line(
        session_id: &str,
        project: &str,
        display: &str,
        timestamp: u64,
    ) -> String {
        serde_json::json!({
            "sessionId": session_id,
            "project": project,
            "display": display,
            "timestamp": timestamp,
        })
        .to_string()
    }

    fn claude_project_line(session_id: &str, cwd: &str, content: &str, timestamp: &str) -> String {
        serde_json::json!({
            "sessionId": session_id,
            "cwd": cwd,
            "timestamp": timestamp,
            "message": { "role": "user", "content": content },
        })
        .to_string()
    }

    #[gpui::test]
    async fn claude_merges_history_and_project_files_newest_wins(cx: &mut TestAppContext) {
        let source = Arc::new(InMemoryHistoryFs::new());
        // Global history: an older title for s1 under /work/proj.
        source.insert(
            "/home/user/.claude/history.jsonl",
            &format!(
                "{}\n",
                claude_history_line("s1", "/work/proj", "history title", 10_000)
            ),
            identity(1, 50),
        );
        // Project file (newer) should win the title.
        source.insert(
            "/home/user/.claude/projects/-work-proj/s1.jsonl",
            &format!(
                "{}\n",
                claude_project_line(
                    "s1",
                    "/work/proj",
                    "project title",
                    "2026-07-24T10:00:00.000Z"
                )
            ),
            identity(1, 50),
        );

        let service = service(cx);
        let host = host_at(source.clone(), "/home/user/.claude");
        let snapshot = service
            .refresh(HistoryKind::Claude, &host, &[PathBuf::from("/work/proj")])
            .await
            .unwrap();
        assert_eq!(snapshot.entries.len(), 1, "session deduped to one entry");
        assert_eq!(snapshot.entries[0].session_id, "s1");
        assert_eq!(snapshot.entries[0].title, "project title");
    }

    #[gpui::test]
    async fn claude_secondary_cwd_does_not_surface_under_nonorigin_root(cx: &mut TestAppContext) {
        let source = Arc::new(InMemoryHistoryFs::new());
        // A session that started in /work/proj and later cd'd into /work/other,
        // recorded under the origin's encoded directory.
        source.insert(
            "/home/user/.claude/projects/-work-proj/s2.jsonl",
            &format!(
                "{}\n{}\n",
                claude_project_line("s2", "/work/proj", "task", "2026-07-24T10:00:00.000Z"),
                claude_project_line("s2", "/work/other", "task", "2026-07-24T11:00:00.000Z"),
            ),
            identity(1, 80),
        );

        let service = service(cx);
        let host = host_at(source.clone(), "/home/user/.claude");

        // The origin root surfaces the session.
        let origin = service
            .refresh(HistoryKind::Claude, &host, &[PathBuf::from("/work/proj")])
            .await
            .unwrap();
        assert_eq!(origin.entries.len(), 1);
        assert_eq!(origin.entries[0].project_root, PathBuf::from("/work/proj"));

        // The secondary directory must not surface it (matches the legacy
        // directory-name lookup), even though it appears as a cwd in the file.
        let secondary = service
            .refresh(HistoryKind::Claude, &host, &[PathBuf::from("/work/other")])
            .await
            .unwrap();
        assert_eq!(secondary.entries.len(), 0);
    }

    fn pi_session_file(id: &str, cwd: &str, content: &str, timestamp: &str) -> String {
        let header = serde_json::json!({
            "type": "session",
            "id": id,
            "cwd": cwd,
            "timestamp": timestamp,
        });
        let message = serde_json::json!({
            "type": "message",
            "timestamp": timestamp,
            "message": { "role": "user", "content": content },
        });
        format!("{header}\n{message}\n")
    }

    #[gpui::test]
    async fn pi_scans_sessions_and_dedups_by_session(cx: &mut TestAppContext) {
        let source = Arc::new(InMemoryHistoryFs::new());
        source.insert(
            "/home/user/.pi/agent/sessions/--work-proj--/older.jsonl",
            &pi_session_file("p1", "/work/proj", "old prompt", "2026-07-24T09:00:00.000Z"),
            identity(1, 40),
        );
        source.insert(
            "/home/user/.pi/agent/sessions/--work-proj--/newer.jsonl",
            &pi_session_file("p1", "/work/proj", "new prompt", "2026-07-24T10:00:00.000Z"),
            identity(1, 40),
        );

        let service = service(cx);
        let host = host_at(source.clone(), "/home/user/.pi/agent");
        let snapshot = service
            .refresh(HistoryKind::Pi, &host, &[PathBuf::from("/work/proj")])
            .await
            .unwrap();
        assert_eq!(snapshot.entries.len(), 1, "same session deduped to newest");
        assert_eq!(snapshot.entries[0].session_id, "p1");
        assert_eq!(snapshot.entries[0].title, "new prompt");
    }
}
