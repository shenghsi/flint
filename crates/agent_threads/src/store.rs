use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Result, anyhow};
use collections::{HashMap, HashSet};
use fs::Fs;
use futures::{
    StreamExt as _,
    channel::{mpsc, oneshot},
};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EntityId, EventEmitter, Global, PromptLevel,
    SharedString, Subscription, Task, TaskExt, WeakEntity, Window, WindowHandle,
};
use project::Project;
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use task::{RevealStrategy, RevealTarget, Shell, SpawnInTerminal};
use terminal_view::{TerminalView, terminal_panel::TerminalPanel};
use util::ResultExt as _;
use workspace::notifications::NotificationId;
use workspace::{MultiWorkspace, SaveIntent, Workspace, WorkspaceId};

use crate::{
    AgentKindDefinition, AgentLaunchCommand, AgentThreadSettings, HistoricalThread,
    RemoteAgentRoutingSettings, agent_kind_registry,
    artifact_cache::AgentArtifactCache,
    egress::{AgentEgressLease, AgentEgressManager},
    history,
    managed_agent::{
        CachedAgentArtifactSource, ManagedAgentInstallation, ManagedAgentProvisioner,
        RemoteClientAgentHost,
    },
    managed_agent_progress::{
        ManagedAgentProgressEvent, ManagedAgentProgressNotification, ManagedAgentProgressReporter,
        ManagedAgentProgressState, ManagedAgentProvisioningCoordinator,
        ManagedAgentProvisioningKey, ManagedAgentProvisioningOwner,
    },
    remote_process::{RemoteAgentProcess, wait_for_graceful_exit_or_force},
    resolve_default_launch_args,
};

#[derive(Clone)]
pub struct AgentThreadMetadata {
    pub terminal_item_id: EntityId,
    pub kind_id: &'static str,
    pub title: SharedString,
    pub project_root: PathBuf,
    /// The worktree this thread is grouped under in the Agent Threads panel.
    /// Usually equal to `project_root`'s owning worktree, but can diverge:
    /// `terminal.working_directory = current_file_directory` can put
    /// `project_root` in a subdirectory rather than a worktree root, and a
    /// retie (see `AgentThreadStore::commit_retie`) changes this field
    /// without moving the process's real cwd.
    pub tied_worktree_root: PathBuf,
    /// The main worktree of the repository `tied_worktree_root` belonged to
    /// when the tie was assigned, if any. Used only as a fallback target if
    /// `tied_worktree_root` is later removed from that repository's
    /// worktree set -- see `TieResolution::effective_tie`. Captured at
    /// assignment time because once the worktree is gone there is no
    /// current state from which to rediscover which repository owned it.
    pub tied_repo_main_root: Option<PathBuf>,
    pub launched_at: SystemTime,
    /// The thread's CLI session id, when Flint knows it: either the id the
    /// thread was resumed from, or the id assigned at launch via the kind's
    /// `session_id_flag`. `None` means the CLI generated its own id that
    /// Flint can't see (e.g. fresh Codex threads).
    pub resumed_session_id: Option<SharedString>,
}

#[derive(Clone)]
pub enum AgentThreadRow {
    Historical {
        thread: HistoricalThread,
        live_terminal_item_id: Option<EntityId>,
    },
    FreshLive(AgentThreadMetadata),
}

impl AgentThreadRow {
    pub fn last_activity_at(&self) -> SystemTime {
        match self {
            AgentThreadRow::Historical { thread, .. } => thread.last_activity_at,
            AgentThreadRow::FreshLive(metadata) => metadata.launched_at,
        }
    }
}

/// Merges a kind's live and historical threads for one project into a
/// single, deduplicated, recency-sorted list. Resumed terminals attach live
/// state to their persisted row. A brand-new (not-yet-resumed) live thread
/// suppresses historical
///   entries for the same kind/project activity at or after its launch,
///   since the CLI hasn't necessarily written its session id anywhere yet
pub fn merge_threads(
    live: Vec<AgentThreadMetadata>,
    historical: impl IntoIterator<Item = HistoricalThread>,
) -> Vec<AgentThreadRow> {
    let mut resumed_terminals: HashMap<SharedString, EntityId> = live
        .iter()
        .filter_map(|metadata| {
            metadata
                .resumed_session_id
                .clone()
                .map(|session_id| (session_id, metadata.terminal_item_id))
        })
        .collect();
    let earliest_fresh_launch = live
        .iter()
        .filter(|metadata| metadata.resumed_session_id.is_none())
        .map(|metadata| metadata.launched_at)
        .min();

    let mut rows = Vec::new();
    for thread in historical {
        let live_terminal_item_id = resumed_terminals.remove(&thread.session_id);
        if let Some(launch_time) = earliest_fresh_launch {
            if live_terminal_item_id.is_none() && thread.last_activity_at >= launch_time {
                continue;
            }
        }
        rows.push(AgentThreadRow::Historical {
            thread,
            live_terminal_item_id,
        });
    }
    for metadata in live {
        let matched_historical = metadata
            .resumed_session_id
            .as_ref()
            .is_some_and(|session_id| !resumed_terminals.contains_key(session_id));
        if !matched_historical {
            rows.push(AgentThreadRow::FreshLive(metadata));
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.last_activity_at()));
    rows
}

/// The outcome of trying to discover which indexed session a fresh, not-yet-
/// resumable live thread turned into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveredSession {
    /// Exactly one session appeared after launch and isn't bound to another
    /// live terminal; safe to attach automatically.
    Resolved(SharedString),
    /// More than one candidate; the caller must ask rather than guess.
    Ambiguous(Vec<SharedString>),
    /// No new session has appeared yet.
    NotFound,
}

/// Resolves which (if any) newly indexed session belongs to a fresh live
/// thread that has no Flint-assigned session id (some CLIs have no
/// `session_id_flag`, see `AgentKindDefinition::session_id_flag`). A session is
/// a candidate when its recorded activity is at or after the thread's
/// `launched_at` -- the same signal `merge_threads` already uses to suppress
/// historical rows for a not-yet-resumed live thread -- and its id is not
/// already bound to another live terminal. Both `indexed_sessions` and
/// `already_bound` must already be scoped to the thread's kind and project,
/// matching `merge_threads`'s contract.
pub fn resolve_discovered_session(
    launched_at: SystemTime,
    indexed_sessions: &[HistoricalThread],
    already_bound: &HashSet<SharedString>,
) -> DiscoveredSession {
    let candidates: Vec<SharedString> = indexed_sessions
        .iter()
        .filter(|thread| {
            thread.last_activity_at >= launched_at && !already_bound.contains(&thread.session_id)
        })
        .map(|thread| thread.session_id.clone())
        .collect();
    match candidates.len() {
        0 => DiscoveredSession::NotFound,
        1 => DiscoveredSession::Resolved(candidates[0].clone()),
        _ => DiscoveredSession::Ambiguous(candidates),
    }
}

/// How often the background loop retries turning not-yet-restorable live
/// threads (kinds with no `session_id_flag`, e.g. Codex/OpenCode) into
/// restorable ones, by checking whether their CLI has since recorded a
/// session file `resolve_discovered_session` can match against.
const SESSION_DISCOVERY_INTERVAL: Duration = Duration::from_secs(30);

struct SessionDiscoveryCandidate {
    terminal_item_id: EntityId,
    kind: AgentKindDefinition,
    project_root: PathBuf,
    launched_at: SystemTime,
    project: Entity<Project>,
    fs: Arc<dyn Fs>,
}

fn spawn_session_discovery_loop(store: Entity<AgentThreadStore>, cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(SESSION_DISCOVERY_INTERVAL)
                .await;
            let candidates =
                store.read_with(cx, |store, cx| store.session_discovery_candidates(cx));
            for candidate in candidates {
                discover_one_session(&store, candidate, cx).await;
            }
        }
    })
    .detach();
}

async fn discover_one_session(
    store: &Entity<AgentThreadStore>,
    candidate: SessionDiscoveryCandidate,
    cx: &mut AsyncApp,
) {
    let SessionDiscoveryCandidate {
        terminal_item_id,
        kind,
        project_root,
        launched_at,
        project,
        fs,
    } = candidate;
    let discovered: Result<DiscoveredSession> = async {
        let indexed_kind = agent_history::HistoryKind::from_id(kind.id)
            .ok_or_else(|| anyhow!("unsupported agent history kind"))?;
        let base_dir = history::resolve_history_base_dir(
            &project,
            kind.home_env_var,
            kind.home_env_child,
            kind.home_dir_name,
            cx,
        )
        .await?;
        let path_style = project.read_with(cx, |project, cx| project.path_style(cx));
        let host = agent_history::HistoryHost {
            fs: Arc::new(agent_history::LocalHistoryFs(fs.clone())),
            base_dir,
            path_style,
        };
        let history_index = cx.update(|cx| history::global_history_index(&fs, cx));
        let snapshot = history_index
            .refresh(indexed_kind, &host, std::slice::from_ref(&project_root))
            .await?;
        let indexed = history::indexed_snapshot_threads(snapshot);
        let already_bound: HashSet<SharedString> = store
            .read_with(cx, |store, _| {
                // Session-id dedup only, not the panel's display path -- the
                // deleted-worktree fallback doesn't apply here, so raw ties
                // are compared exactly as before `TieResolution` existed.
                store.live_threads_for_project(
                    kind.id,
                    std::slice::from_ref(&project_root),
                    &TieResolution::not_ready(),
                )
            })
            .into_iter()
            .filter(|metadata| metadata.terminal_item_id != terminal_item_id)
            .filter_map(|metadata| metadata.resumed_session_id)
            .collect();
        Ok(resolve_discovered_session(
            launched_at,
            &indexed,
            &already_bound,
        ))
    }
    .await;

    match discovered {
        Ok(DiscoveredSession::Resolved(session_id)) => {
            store.update(cx, |store, cx| {
                store.attach_discovered_session_id(terminal_item_id, session_id, cx)
            });
        }
        // Ambiguous and NotFound are the ordinary state of a thread the CLI
        // hasn't recorded (or hasn't uniquely recorded) yet; the next tick
        // retries rather than treating either as an error.
        Ok(DiscoveredSession::Ambiguous(_) | DiscoveredSession::NotFound) => {}
        Err(error) => {
            log::debug!(
                "agent_threads: session discovery failed for {}: {error:#}",
                kind.id
            );
        }
    }
}

pub enum AgentThreadStoreEvent {
    ThreadOpened {
        kind_id: &'static str,
    },
    ThreadClosed {
        kind_id: &'static str,
    },
    /// A live thread's tie or ownership changed without opening or closing
    /// it (currently: a retie). Panels handle this like `ThreadOpened` --
    /// `cx.notify()` and re-run their own queries -- so a retie taking
    /// effect in one panel and disappearing from another happens for free
    /// via the existing per-panel query architecture.
    ThreadUpdated {
        kind_id: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentThreadSessionRestoreRecord {
    pub workspace_id: WorkspaceId,
    pub kind_id: String,
    pub session_id: String,
    pub title: String,
    pub project_root: PathBuf,
    /// The thread's tied worktree at snapshot time. `#[serde(default)]` so
    /// pre-existing serialized records (written before this field existed)
    /// still deserialize as `None`. `None` means "no resolved tie was
    /// recorded" -- legacy records only, since every *new* snapshot always
    /// populates this -- and routes by `workspace_id` instead of by tie;
    /// see `records_to_restore_for_workspace`. Never treat `None` as
    /// "derive from `project_root`": `project_root` is a launch cwd that
    /// can be a subdirectory rather than a worktree root (e.g.
    /// `terminal.working_directory = current_file_directory`), so a
    /// path-equality fallback against it is a different, wrong behavior,
    /// not "unchanged".
    #[serde(default)]
    pub tied_worktree_root: Option<PathBuf>,
    pub last_activity_at: u64,
}

const SESSION_RESTORE_NAMESPACE: &str = "agent-thread-session-restore";

/// Persisted retie overrides, keyed by `(kind_id, session_id)` rather than
/// bare session_id: session ids are only unique per provider, not globally.
/// See the design doc's "Session ids cannot be the primary key" section.
const SESSION_TIE_NAMESPACE: &str = "agent-thread-session-tie";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedTieOverride {
    tied_worktree_root: PathBuf,
    repo_main_root: Option<PathBuf>,
}

fn session_tie_key(kind_id: &str, session_id: &str) -> String {
    format!("{kind_id}:{session_id}")
}

/// Reads the persisted tie override for a resumed session, if this session
/// was ever retied. `None` is the common case (never retied) and is not an
/// error -- logged and treated as absent either way.
pub(crate) fn read_tie_override(cx: &App, kind_id: &str, session_id: &str) -> Option<TiedWorktree> {
    let json = db::kvp::KeyValueStore::global(cx)
        .scoped(SESSION_TIE_NAMESPACE)
        .read(&session_tie_key(kind_id, session_id))
        .log_err()
        .flatten()?;
    let parsed: PersistedTieOverride = serde_json::from_str(&json).log_err()?;
    Some(TiedWorktree {
        root: parsed.tied_worktree_root,
        repo_main_root: parsed.repo_main_root,
    })
}

/// Persists `tie` as the override for a resumed session. Awaited by design
/// (not `db::write_and_log`'s fire-and-forget pattern used elsewhere in this
/// file): the retie orchestration's response needs to distinguish a
/// truthful "persisted" from "in_memory_only", which a detached write can't
/// back.
async fn write_tie_override(
    cx: &mut AsyncApp,
    kind_id: &str,
    session_id: &str,
    tie: &TiedWorktree,
) -> Result<()> {
    let value = serde_json::to_string(&PersistedTieOverride {
        tied_worktree_root: tie.root.clone(),
        repo_main_root: tie.repo_main_root.clone(),
    })?;
    let store = cx.update(|cx| db::kvp::KeyValueStore::global(cx));
    store
        .scoped(SESSION_TIE_NAMESPACE)
        .write(session_tie_key(kind_id, session_id), value)
        .await
}

struct SnapshotRequest {
    session_id: String,
    records_json: String,
    completion: oneshot::Sender<Result<()>>,
}

async fn run_snapshot_writer<Persist, PersistFuture>(
    mut receiver: mpsc::UnboundedReceiver<SnapshotRequest>,
    mut persist: Persist,
) where
    Persist: FnMut(String, String) -> PersistFuture,
    PersistFuture: std::future::Future<Output = Result<()>>,
{
    while let Some(request) = receiver.next().await {
        let result = persist(request.session_id, request.records_json).await;
        if let Err(result) = request.completion.send(result)
            && let Err(error) = result
        {
            log::error!("Failed to persist agent thread restore snapshot: {error:#}");
        }
    }
}

pub struct AgentThreadStore {
    threads: HashMap<EntityId, ThreadEntry>,
    subscriptions: HashMap<EntityId, Vec<Subscription>>,
    /// Workspaces `restore_threads_for_workspace` has already run for this
    /// app session. Restore can be triggered from several places (startup,
    /// open requests, lazy workspace activation); the first attempt wins so
    /// concurrent triggers can't resume the same session twice, and closing
    /// a restored thread doesn't bring it back on the next trigger.
    restore_attempted: HashSet<WorkspaceId>,
    egress_manager: std::sync::Arc<AgentEgressManager>,
    route_changes: HashSet<remote::RemoteConnectionIdentity>,
    agent_artifact_cache: Option<Arc<AgentArtifactCache>>,
    managed_provisioning:
        ManagedAgentProvisioningCoordinator<Entity<ManagedAgentProgressNotification>>,
    snapshot_sender: mpsc::UnboundedSender<SnapshotRequest>,
    /// Entries retied before their session id was known (so the persisted
    /// tie override, keyed by `(kind_id, session_id)`, couldn't be written
    /// yet). `attach_discovered_session_id` persists the entry's current
    /// tie and clears the marker once the id becomes known. See the design
    /// doc's "Session ids cannot be the primary key" section.
    pending_tie_persistence: HashSet<EntityId>,
    /// The agent-control server's accept-loop task (see `crate::control`),
    /// held here so its lifetime is the app's rather than any caller's.
    /// `None` on non-Unix hosts, where the control server doesn't exist.
    _control_server_task: Option<Task<()>>,
}

struct ThreadEntry {
    metadata: AgentThreadMetadata,
    workspace: WeakEntity<Workspace>,
    terminal_view: WeakEntity<TerminalView>,
    terminal: Entity<terminal::Terminal>,
    window: Option<WindowHandle<MultiWorkspace>>,
    remote_process: Option<RemoteAgentProcess>,
    egress: Option<AgentEgressLease>,
}

struct ThreadShutdown {
    terminal: Entity<terminal::Terminal>,
    remote_process: Option<RemoteAgentProcess>,
    egress: Option<AgentEgressLease>,
    workspace: WeakEntity<Workspace>,
}

fn notification_project_name(project_root: &Path) -> String {
    let project_root = project_root.to_string_lossy();
    project_root
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or(project_root.as_ref())
        .to_string()
}

const AGENT_SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(2);

async fn retain_resource_until_shutdown<Resource, Shutdown>(
    resource: Resource,
    shutdown: Shutdown,
) -> Result<()>
where
    Shutdown: std::future::Future<Output = Result<()>>,
{
    let result = shutdown.await;
    drop(resource);
    result
}

fn take_thread_for_shutdown<Entry>(
    entries: &mut HashMap<EntityId, Entry>,
    terminal_item_id: EntityId,
) -> Option<Entry> {
    entries.remove(&terminal_item_id)
}

impl ThreadShutdown {
    fn run(self, cx: &mut App) -> Task<Result<()>> {
        let completion = self.terminal.update(cx, |terminal, cx| {
            let completion = terminal.wait_for_completed_task(cx);
            terminal.input(vec![0x03]);
            completion
        });
        let timeout = cx.background_executor().timer(AGENT_SHUTDOWN_GRACE_PERIOD);
        let terminal = self.terminal;
        let remote_process = self.remote_process;
        let egress = self.egress;
        let workspace = self.workspace;
        cx.spawn(async move |cx| {
            let result = match remote_process {
                Some(remote_process) => wait_for_graceful_exit_or_force(
                    async move {
                        let _exit_status = completion.await;
                    },
                    timeout,
                    remote_process.force_terminate(),
                )
                .await
                .map(|_| ()),
                None => {
                    let force = async {
                        terminal.update(cx, |terminal, _| terminal.kill_active_task());
                        Ok(())
                    };
                    wait_for_graceful_exit_or_force(
                        async move {
                            let _exit_status = completion.await;
                        },
                        timeout,
                        force,
                    )
                    .await
                    .map(|_| ())
                }
            };
            let result = retain_resource_until_shutdown(egress, async move { result }).await;
            if let Err(error) = &result {
                if let Some(workspace) = workspace.upgrade() {
                    workspace.update(cx, |workspace, cx| workspace.show_error(error, cx));
                } else {
                    log::error!("Failed to stop remote Agent Thread: {error:#}");
                }
            }
            result
        })
    }
}

struct GlobalAgentThreadStore(Entity<AgentThreadStore>);
impl Global for GlobalAgentThreadStore {}

impl EventEmitter<AgentThreadStoreEvent> for AgentThreadStore {}

impl AgentThreadStore {
    pub fn init_global(cx: &mut App) {
        if cx.has_global::<GlobalAgentThreadStore>() {
            return;
        }
        let (snapshot_sender, snapshot_receiver) = mpsc::unbounded();
        let key_value_store = db::kvp::KeyValueStore::global(cx);
        cx.background_spawn(run_snapshot_writer(
            snapshot_receiver,
            move |session_id, records_json| {
                let key_value_store = key_value_store.clone();
                async move {
                    key_value_store
                        .scoped(SESSION_RESTORE_NAMESPACE)
                        .write(session_id, records_json)
                        .await
                }
            },
        ))
        .detach();
        let store = cx.new(|_| Self {
            threads: HashMap::default(),
            subscriptions: HashMap::default(),
            restore_attempted: HashSet::default(),
            egress_manager: std::sync::Arc::new(AgentEgressManager::new()),
            route_changes: HashSet::default(),
            agent_artifact_cache: None,
            managed_provisioning: ManagedAgentProvisioningCoordinator::default(),
            snapshot_sender,
            pending_tie_persistence: HashSet::default(),
            _control_server_task: None,
        });
        spawn_session_discovery_loop(store.clone(), cx);
        cx.set_global(GlobalAgentThreadStore(store));
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAgentThreadStore>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalAgentThreadStore>()
            .map(|store| store.0.clone())
    }

    pub fn begin_route_change(
        &mut self,
        connection_options: &remote::RemoteConnectionOptions,
    ) -> Result<()> {
        let identity = remote::remote_connection_identity(connection_options);
        if !self.route_changes.insert(identity) {
            anyhow::bail!("the agent route is already changing for this host");
        }
        Ok(())
    }

    pub fn finish_route_change(&mut self, connection_options: &remote::RemoteConnectionOptions) {
        self.route_changes
            .remove(&remote::remote_connection_identity(connection_options));
    }

    fn route_change_in_progress(
        &self,
        connection_options: &remote::RemoteConnectionOptions,
    ) -> bool {
        self.route_changes
            .contains(&remote::remote_connection_identity(connection_options))
    }

    pub(crate) fn live_threads_for_project(
        &self,
        kind_id: &str,
        project_roots: &[PathBuf],
        tie_resolution: &TieResolution,
    ) -> Vec<AgentThreadMetadata> {
        self.threads
            .values()
            .map(|entry| &entry.metadata)
            .filter(|metadata| {
                if metadata.kind_id != kind_id {
                    return false;
                }
                let Some(effective_tie) = tie_resolution.effective_tie(
                    &metadata.tied_worktree_root,
                    metadata.tied_repo_main_root.as_deref(),
                ) else {
                    return false;
                };
                project_roots.iter().any(|root| root == &effective_tie)
            })
            .cloned()
            .collect()
    }

    /// Live threads eligible for background session discovery: no
    /// Flint-assigned id (the kind has no `session_id_flag`), a history
    /// provider that can index one, and a local (non-remote) workspace,
    /// matching the existing on-demand handoff discovery's constraints.
    fn session_discovery_candidates(&self, cx: &App) -> Vec<SessionDiscoveryCandidate> {
        self.threads
            .values()
            .filter(|entry| entry.metadata.resumed_session_id.is_none())
            .filter_map(|entry| {
                let kind = agent_kind_registry()
                    .into_iter()
                    .find(|kind| kind.id == entry.metadata.kind_id)?;
                if kind.session_id_flag.is_some() || kind.history_provider.is_none() {
                    return None;
                }
                let workspace = entry.workspace.upgrade()?;
                let workspace = workspace.read(cx);
                if workspace.project().read(cx).remote_client().is_some() {
                    return None;
                }
                Some(SessionDiscoveryCandidate {
                    terminal_item_id: entry.metadata.terminal_item_id,
                    kind,
                    project_root: entry.metadata.project_root.clone(),
                    launched_at: entry.metadata.launched_at,
                    project: workspace.project().clone(),
                    fs: workspace.app_state().fs.clone(),
                })
            })
            .collect()
    }

    /// Attaches a session id discovered after the fact (see
    /// `resolve_discovered_session`, run periodically by
    /// `spawn_session_discovery_loop`) to a live thread that launched without
    /// one. Once attached, the thread becomes eligible for session-restore
    /// snapshots, the same as if the CLI had supported `session_id_flag` from
    /// the start.
    fn attach_discovered_session_id(
        &mut self,
        terminal_item_id: EntityId,
        session_id: SharedString,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.threads.get_mut(&terminal_item_id) else {
            return;
        };
        if entry.metadata.resumed_session_id.is_some() {
            return;
        }
        entry.metadata.resumed_session_id = Some(session_id);
        cx.notify();

        // Migrate a retie that happened before this thread's session id was
        // known: the persisted override table is keyed by (kind_id,
        // session_id), so it couldn't be written until now. Fire-and-forget
        // is fine here (unlike the retie orchestration's own write, nothing
        // is waiting on this to report success/failure back to a caller).
        if self.pending_tie_persistence.remove(&terminal_item_id)
            && let Some((kind_id, session_id, tie)) = self.tie_override_to_persist(terminal_item_id)
        {
            cx.spawn(async move |_, cx| write_tie_override(cx, kind_id, &session_id, &tie).await)
                .detach_and_log_err(cx);
        }
    }

    pub fn active_thread_count_for_connection(
        &self,
        connection_options: &remote::RemoteConnectionOptions,
        cx: &App,
    ) -> usize {
        self.threads
            .values()
            .filter(|entry| entry_matches_connection(entry, connection_options, cx))
            .count()
    }

    pub fn close_threads_for_connection(
        connection_options: remote::RemoteConnectionOptions,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let store = Self::global(cx);
        let entries = store
            .read(cx)
            .threads
            .values()
            .filter(|entry| entry_matches_connection(entry, &connection_options, cx))
            .map(|entry| {
                anyhow::Ok((
                    entry
                        .workspace
                        .upgrade()
                        .ok_or_else(|| anyhow!("agent thread workspace closed"))?,
                    entry
                        .window
                        .ok_or_else(|| anyhow!("agent thread window closed"))?,
                    entry.metadata.terminal_item_id,
                ))
            })
            .collect::<Result<Vec<_>>>();
        let entries = match entries {
            Ok(entries) => entries,
            Err(error) => return Task::ready(Err(error)),
        };

        cx.spawn(async move |cx| {
            for (workspace, window_handle, terminal_item_id) in entries {
                let shutdown_task =
                    store.update(cx, |store, cx| store.begin_shutdown(terminal_item_id, cx));
                let close_result = window_handle.update(cx, |_multi_workspace, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        let pane = workspace
                            .pane_for_item_id(terminal_item_id)
                            .ok_or_else(|| anyhow!("agent thread pane closed"))?;
                        anyhow::Ok(pane.update(cx, |pane, cx| {
                            pane.close_items(window, cx, SaveIntent::Close, &move |item_id| {
                                item_id == terminal_item_id
                            })
                        }))
                    })
                });
                let close_result = match close_result {
                    Ok(Ok(close_task)) => close_task.await,
                    Ok(Err(error)) | Err(error) => Err(error),
                };
                let shutdown_result = match shutdown_task {
                    Some(shutdown_task) => shutdown_task.await,
                    None => Ok(()),
                };
                close_result?;
                shutdown_result?;
            }
            anyhow::Ok(())
        })
    }

    fn live_threads_for_workspace(
        &self,
        workspace_id: WorkspaceId,
        cx: &App,
    ) -> Vec<AgentThreadMetadata> {
        self.threads
            .values()
            .filter(|entry| {
                entry
                    .workspace
                    .upgrade()
                    .is_some_and(|workspace| workspace.read(cx).database_id() == Some(workspace_id))
            })
            .map(|entry| entry.metadata.clone())
            .collect()
    }

    fn session_restore_records(&self, cx: &App) -> Vec<AgentThreadSessionRestoreRecord> {
        let mut records = Vec::new();
        for entry in self.threads.values() {
            let Some(workspace) = entry.workspace.upgrade() else {
                continue;
            };
            let Some(workspace_id) = workspace.read(cx).database_id() else {
                continue;
            };
            records.extend(snapshot_records_for_workspace(
                workspace_id,
                [entry.metadata.clone()],
            ));
        }
        records
    }

    pub fn focus_thread(
        &mut self,
        terminal_item_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let entry = self
            .threads
            .get(&terminal_item_id)
            .ok_or_else(|| anyhow!("agent thread no longer exists"))?;
        let workspace = entry
            .workspace
            .upgrade()
            .ok_or_else(|| anyhow!("agent thread workspace closed"))?;
        let terminal_view = entry
            .terminal_view
            .upgrade()
            .ok_or_else(|| anyhow!("agent thread terminal closed"))?;

        workspace.update(cx, |workspace, cx| {
            let pane = workspace
                .pane_for_item_id(terminal_view.entity_id())
                .ok_or_else(|| anyhow!("agent thread pane closed"))?;
            pane.update(cx, |pane, cx| {
                let index = pane
                    .index_for_item(&terminal_view)
                    .ok_or_else(|| anyhow!("agent thread item closed"))?;
                pane.activate_item(index, true, true, window, cx);
                anyhow::Ok(())
            })
        })?;

        Ok(())
    }

    /// The synchronous, infallible-once-called part of a retie: updates
    /// `entry.workspace` and the metadata's tie fields, and emits
    /// `ThreadUpdated`. No IO, no workspace resolution -- those happen
    /// first, in the async `retie_thread` orchestration, which only calls
    /// this once the checked move into `new_workspace` has already
    /// succeeded (so `entry.workspace` here is not a promise, it is
    /// already true).
    ///
    /// Marks the entry for later persistence (via `pending_tie_persistence`)
    /// when its session id isn't known yet -- session ids are the
    /// persisted-override table's key, and a `retie-thread` request can
    /// easily arrive before discovery has assigned one. See the design
    /// doc's "Session ids cannot be the primary key" section.
    // Reachable only from `retie_thread` today, which is itself only called
    // from tests -- Stage 2's control.rs is its production caller.
    #[allow(dead_code)]
    pub(crate) fn commit_retie(
        &mut self,
        terminal_item_id: EntityId,
        new_workspace: Entity<Workspace>,
        new_tie: TiedWorktree,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let entry = self
            .threads
            .get_mut(&terminal_item_id)
            .ok_or_else(|| anyhow!("agent thread no longer exists"))?;
        entry.workspace = new_workspace.downgrade();
        entry.metadata.tied_worktree_root = new_tie.root;
        entry.metadata.tied_repo_main_root = new_tie.repo_main_root;
        let kind_id = entry.metadata.kind_id;
        if entry.metadata.resumed_session_id.is_none() {
            self.pending_tie_persistence.insert(terminal_item_id);
        }
        cx.emit(AgentThreadStoreEvent::ThreadUpdated { kind_id });
        cx.notify();
        Ok(())
    }

    /// The tie override to persist for `terminal_item_id` right now, if its
    /// session id is already known -- `None` when it isn't (the caller
    /// should leave `pending_tie_persistence`'s marker in place instead;
    /// `attach_discovered_session_id` will persist it once the id arrives).
    pub(crate) fn tie_override_to_persist(
        &self,
        terminal_item_id: EntityId,
    ) -> Option<(&'static str, SharedString, TiedWorktree)> {
        let entry = self.threads.get(&terminal_item_id)?;
        let session_id = entry.metadata.resumed_session_id.clone()?;
        Some((
            entry.metadata.kind_id,
            session_id,
            TiedWorktree {
                root: entry.metadata.tied_worktree_root.clone(),
                repo_main_root: entry.metadata.tied_repo_main_root.clone(),
            },
        ))
    }

    /// The workspace and terminal item a live thread currently lives in --
    /// the source side of a retie's move. Mirrors `focus_thread`'s own
    /// resolution of these two handles.
    // Reachable only from `retie_thread` today; see its own dead_code note.
    #[allow(dead_code)]
    pub(crate) fn thread_workspace_and_terminal(
        &self,
        terminal_item_id: EntityId,
    ) -> Result<(Entity<Workspace>, Entity<TerminalView>)> {
        let entry = self
            .threads
            .get(&terminal_item_id)
            .ok_or_else(|| anyhow!("agent thread no longer exists"))?;
        let workspace = entry
            .workspace
            .upgrade()
            .ok_or_else(|| anyhow!("agent thread workspace closed"))?;
        let terminal_view = entry
            .terminal_view
            .upgrade()
            .ok_or_else(|| anyhow!("agent thread terminal closed"))?;
        Ok((workspace, terminal_view))
    }

    fn register(
        &mut self,
        kind_id: &'static str,
        title: SharedString,
        project_root: PathBuf,
        tied_worktree: TiedWorktree,
        resumed_session_id: Option<SharedString>,
        launched_at: SystemTime,
        workspace: Entity<Workspace>,
        terminal_view: Entity<TerminalView>,
        window: Option<WindowHandle<MultiWorkspace>>,
        remote_process: Option<RemoteAgentProcess>,
        egress: Option<AgentEgressLease>,
        cx: &mut Context<Self>,
    ) {
        let terminal_item_id = terminal_view.entity_id();
        let terminal = terminal_view.read(cx).terminal().clone();
        let project_name = notification_project_name(&project_root);
        let metadata = AgentThreadMetadata {
            terminal_item_id,
            kind_id,
            title: title.clone(),
            project_root,
            tied_worktree_root: tied_worktree.root,
            tied_repo_main_root: tied_worktree.repo_main_root,
            launched_at,
            resumed_session_id,
        };
        let attention_window = window;
        self.threads.insert(
            terminal_item_id,
            ThreadEntry {
                metadata,
                workspace: workspace.downgrade(),
                terminal_view: terminal_view.downgrade(),
                terminal: terminal.clone(),
                window,
                remote_process,
                egress,
            },
        );

        let release_subscription =
            cx.observe_release(&terminal_view, move |store, _terminal_view, cx| {
                if let Some(shutdown) = store.begin_shutdown(terminal_item_id, cx) {
                    shutdown.detach_and_log_err(cx);
                }
            });
        // Claude Code and Codex CLI both ring the terminal bell when a turn
        // finishes or a permission prompt needs an answer, so it's the
        // signal we have for "this agent thread needs attention" -- the
        // underlying `Terminal` (not `TerminalView`, which only re-emits
        // `Wakeup`/`UpdateTab` for a bell) is what actually re-emits it.
        let bell_subscription = cx.subscribe(&terminal, move |_store, _terminal, event, cx| {
            if !matches!(event, terminal::Event::Bell) {
                return;
            }
            if !AgentThreadSettings::get_global(cx).notify_when_finished {
                return;
            }
            let kind_label = agent_kind_registry()
                .into_iter()
                .find(|kind| kind.id == kind_id)
                .map(|kind| kind.label.to_string())
                .unwrap_or_else(|| kind_id.to_string());
            if let Some(window_handle) = attention_window.as_ref() {
                window_handle
                    .update(cx, |_, window, _| window.request_attention())
                    .log_err();
            }
            cx.show_desktop_notification(
                &title,
                Some(&format!(
                    "{kind_label} is waiting for you · Project: {project_name}"
                )),
            );
        });
        self.subscriptions.insert(
            terminal_item_id,
            vec![release_subscription, bell_subscription],
        );
        cx.emit(AgentThreadStoreEvent::ThreadOpened { kind_id });
    }

    fn begin_shutdown(
        &mut self,
        terminal_item_id: EntityId,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let entry = take_thread_for_shutdown(&mut self.threads, terminal_item_id)?;
        self.subscriptions.remove(&terminal_item_id);
        cx.emit(AgentThreadStoreEvent::ThreadClosed {
            kind_id: entry.metadata.kind_id,
        });
        Some(
            ThreadShutdown {
                terminal: entry.terminal,
                remote_process: entry.remote_process,
                egress: entry.egress,
                workspace: entry.workspace,
            }
            .run(cx),
        )
    }

    /// The window a live thread's terminal lives in -- needed by the control
    /// server to call `retie_thread`/`launch_seeded_thread`, both of which
    /// require a live `Window` rather than just the owning `Workspace`.
    pub(crate) fn thread_window(
        &self,
        terminal_item_id: EntityId,
    ) -> Result<WindowHandle<MultiWorkspace>> {
        self.threads
            .get(&terminal_item_id)
            .ok_or_else(|| anyhow!("agent thread no longer exists"))?
            .window
            .ok_or_else(|| anyhow!("agent thread window closed"))
    }

    /// Stores the agent-control server's accept-loop task so its lifetime is
    /// the app's, not any caller's -- see `crate::control::init`.
    pub(crate) fn hold_control_server_task(&mut self, task: Task<()>) {
        self._control_server_task = Some(task);
    }

    /// Every live thread's underlying terminal process id, for the control
    /// server's peer-credential resolution: it walks a connecting process's
    /// ancestry looking for a PID in this map, so it knows which thread (if
    /// any) is calling. A remote thread's terminal process runs on a
    /// different machine, so it can never appear in this map or match a
    /// local process's ancestry -- remote threads are excluded from the
    /// control surface by construction, with no separate check needed.
    pub(crate) fn live_terminal_pids(&self, cx: &App) -> HashMap<u32, EntityId> {
        self.threads
            .iter()
            .filter_map(|(terminal_item_id, entry)| {
                let pid = entry.terminal.read(cx).pid()?;
                Some((pid.as_u32(), *terminal_item_id))
            })
            .collect()
    }
}

fn entry_matches_connection(
    entry: &ThreadEntry,
    connection_options: &remote::RemoteConnectionOptions,
    cx: &App,
) -> bool {
    entry
        .workspace
        .upgrade()
        .and_then(|workspace| workspace.read(cx).project().read(cx).remote_client())
        .and_then(|client| client.read(cx).remote_connection())
        .is_some_and(|connection| {
            remote::same_remote_connection_identity(
                Some(&connection.connection_options()),
                Some(connection_options),
            )
        })
}

pub fn launch_new_thread(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    extra_args: &[String],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    match new_thread_launch_route(current_remote_agent_route(workspace, cx)) {
        NewThreadLaunchRoute::Configured => {
            launch_configured_thread(workspace, kind, extra_args, window, cx)
        }
        NewThreadLaunchRoute::ManagedTunneled => launch_managed_thread_for_route(
            workspace,
            kind,
            extra_args,
            Some(RequiredAgentRoute(settings::RemoteAgentRoute::Tunneled)),
            window,
            cx,
        ),
    }
}

/// Launches a fresh thread for `kind` seeded with `initial_prompt` as its first
/// turn, for cross-agent handoff. Only used on the configured (non-managed)
/// route -- handoff does not support the managed/tunneled route yet, so
/// callers must not reach this for a project using it.
///
/// Returns whether the prompt was actually seeded, so a caller that promised
/// a seeded launch (e.g. `create-thread`'s command handler) can report a
/// structured failure instead of silently spawning an unseeded thread when
/// `kind.initial_prompt_strategy` is `Unsupported` or the prompt is empty.
pub(crate) fn launch_seeded_thread(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    initial_prompt: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let extra_args = resolve_new_thread_launch_args(cx, kind);
    let settings = AgentThreadSettings::get_global(cx);
    let base = settings.command_for_kind(kind.id);
    let mut launch = build_new_thread_launch(kind, base, &extra_args, None);
    let seeded = crate::seed_launch_command_with_prompt(&mut launch.command, kind, initial_prompt);
    spawn_thread(
        workspace,
        kind,
        kind.label.clone(),
        launch.command,
        launch.session_id,
        window,
        cx,
    );
    seeded
}

fn launch_configured_thread(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    extra_args: &[String],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let settings = AgentThreadSettings::get_global(cx);
    let base = settings.command_for_kind(kind.id);
    let launch = build_new_thread_launch(kind, base, extra_args, None);
    spawn_thread(
        workspace,
        kind,
        kind.label.clone(),
        launch.command,
        launch.session_id,
        window,
        cx,
    );
}

pub fn launch_credential_command(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    summary: SharedString,
    arguments: &'static [&'static str],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let base = AgentThreadSettings::get_global(cx)
        .command_for_kind(kind.id)
        .clone();
    let route = current_remote_agent_route(workspace, cx);
    let required_route = route.map(RequiredAgentRoute);
    if !uses_managed_credential_command(route) {
        let command = build_credential_command(&base, arguments);
        let task = spawn_thread_task_for_route(
            workspace,
            kind,
            summary,
            command,
            None,
            required_route,
            window,
            cx,
        );
        cx.spawn_in(window, async move |workspace, cx| {
            if let Err(error) = task.await {
                workspace.update(cx, |workspace, cx| workspace.show_error(&error, cx))?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
        return;
    }

    let preparation = prepare_managed_agent(workspace, kind, window, cx);
    let kind = kind.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        match preparation.await {
            Ok(ManagedAgentPreparation::Ready(prepared)) => {
                prepared.notification.update(cx, |notification, cx| {
                    notification.set_state(ManagedAgentProgressState::Launching, cx);
                });
                let task = workspace.update_in(cx, |workspace, window, cx| {
                    workspace.dismiss_notification(
                        &managed_agent_notification_id(kind.id, &prepared.installation.version),
                        cx,
                    );
                    let command = build_managed_credential_command(
                        &kind,
                        &base,
                        arguments,
                        &prepared.installation.executable_path,
                    );
                    spawn_thread_task_for_route(
                        workspace,
                        &kind,
                        summary,
                        command,
                        None,
                        Some(RequiredAgentRoute(settings::RemoteAgentRoute::Tunneled)),
                        window,
                        cx,
                    )
                })?;
                if let Err(error) = task.await {
                    workspace.update(cx, |workspace, cx| workspace.show_error(&error, cx))?;
                }
            }
            Ok(ManagedAgentPreparation::Cancelled)
            | Ok(ManagedAgentPreparation::AlreadyInProgress) => {}
            Err(error) => {
                workspace.update(cx, |workspace, cx| workspace.show_error(&error, cx))?;
            }
        }
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

/// Namespace for the per-thread "remembered launch option" key-value store
/// (`db::kvp`). The value is the chosen `ResumeOption::id`, or an empty
/// string for an explicit "plain resume, no extra args" choice. Absence of
/// a key means no per-thread choice has been made yet.
///
/// Keyed by `id` rather than `label`: `label` is UI copy that can be edited
/// freely, and matching against it would silently orphan every persisted
/// choice (falling back to the default) the next time the label's wording
/// changes.
const LAUNCH_OPTION_NAMESPACE: &str = "agent-thread-launch-option";

/// Reads the launch option id the user last picked for this specific
/// thread (via its "..." menu), if any.
pub fn remembered_launch_option(cx: &App, session_id: &str) -> Option<String> {
    db::kvp::KeyValueStore::global(cx)
        .scoped(LAUNCH_OPTION_NAMESPACE)
        .read(session_id)
        .log_err()
        .flatten()
}

/// Persists `id` as this thread's remembered launch option choice.
/// `None` represents an explicit "plain resume" choice (stored as an empty
/// string, distinct from no choice having been made at all).
pub fn remember_launch_option(cx: &App, session_id: SharedString, id: Option<String>) {
    let store = db::kvp::KeyValueStore::global(cx);
    db::write_and_log(cx, move || async move {
        store
            .scoped(LAUNCH_OPTION_NAMESPACE)
            .write(session_id.to_string(), id.unwrap_or_default())
            .await
    });
}

/// Resolves the extra arguments to use when resuming `thread`: its
/// remembered per-thread choice takes priority over `kind`'s
/// agent-wide `default_launch_option` setting.
pub fn resolve_thread_launch_args(
    cx: &App,
    kind: &AgentKindDefinition,
    session_id: &str,
) -> Vec<String> {
    match remembered_launch_option(cx, session_id) {
        Some(id) if id.is_empty() => Vec::new(),
        Some(id) => kind
            .resume_options
            .iter()
            .find(|option| option.id == id)
            .map(|option| option.args.clone())
            .unwrap_or_default(),
        None => {
            let settings = AgentThreadSettings::get_global(cx);
            resolve_default_launch_args(settings.command_for_kind(kind.id), kind).to_vec()
        }
    }
}

/// Namespace for the "remembered new-thread launch option" key-value store,
/// keyed by agent kind id rather than session id since a brand-new thread
/// has no session yet. Same empty-string-means-"no extra args" convention as
/// [`LAUNCH_OPTION_NAMESPACE`], and the same id-not-label keying rationale.
const NEW_THREAD_LAUNCH_OPTION_NAMESPACE: &str = "agent-thread-new-launch-option";

/// Reads the launch option id the user last picked from the new-thread
/// dropdown for this agent kind, if any.
pub fn remembered_new_thread_launch_option(cx: &App, kind_id: &str) -> Option<String> {
    db::kvp::KeyValueStore::global(cx)
        .scoped(NEW_THREAD_LAUNCH_OPTION_NAMESPACE)
        .read(kind_id)
        .log_err()
        .flatten()
}

/// Persists `id` as this agent kind's remembered new-thread launch option
/// choice. `None` represents an explicit "plain new thread" choice (stored
/// as an empty string, distinct from no choice having been made at all).
pub fn remember_new_thread_launch_option(cx: &App, kind_id: &'static str, id: Option<String>) {
    let store = db::kvp::KeyValueStore::global(cx);
    db::write_and_log(cx, move || async move {
        store
            .scoped(NEW_THREAD_LAUNCH_OPTION_NAMESPACE)
            .write(kind_id.to_string(), id.unwrap_or_default())
            .await
    });
}

/// Resolves the extra arguments to use when starting a *new* thread for
/// `kind`: the remembered choice from the new-thread dropdown takes priority
/// over `kind`'s agent-wide `default_launch_option` setting.
pub fn resolve_new_thread_launch_args(cx: &App, kind: &AgentKindDefinition) -> Vec<String> {
    match remembered_new_thread_launch_option(cx, kind.id) {
        Some(id) if id.is_empty() => Vec::new(),
        Some(id) => kind
            .resume_options
            .iter()
            .find(|option| option.id == id)
            .map(|option| option.args.clone())
            .unwrap_or_default(),
        None => {
            let settings = AgentThreadSettings::get_global(cx);
            resolve_default_launch_args(settings.command_for_kind(kind.id), kind).to_vec()
        }
    }
}

pub fn resume_thread(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    thread: &HistoricalThread,
    extra_args: &[String],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(task) = resume_thread_task(workspace, kind, thread, extra_args, window, cx) else {
        return;
    };
    cx.spawn_in(window, async move |workspace, cx| {
        if let Err(error) = task.await {
            workspace.update(cx, |workspace, cx| workspace.show_error(&error, cx))?;
        }
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn resume_thread_task(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    thread: &HistoricalThread,
    extra_args: &[String],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<Task<Result<ResumeThreadOutcome>>> {
    let settings = AgentThreadSettings::get_global(cx);
    let base = settings.command_for_kind(kind.id).clone();
    let route = current_remote_agent_route(workspace, cx);
    let required_route = route.map(RequiredAgentRoute);
    if !uses_managed_resume(kind, route) {
        let command = build_resume_command(kind, &base, thread, extra_args, None)?;
        let task = spawn_thread_task_for_route(
            workspace,
            kind,
            thread.title.clone(),
            command,
            Some(thread.session_id.clone()),
            required_route,
            window,
            cx,
        );
        return Some(cx.spawn_in(window, async move |_workspace, _cx| {
            task.await?;
            Ok(ResumeThreadOutcome::Launched)
        }));
    }

    let preparation = prepare_managed_agent(workspace, kind, window, cx);
    let kind = kind.clone();
    let thread = thread.clone();
    let extra_args = extra_args.to_vec();
    Some(cx.spawn_in(window, async move |workspace, cx| {
        let ManagedAgentPreparation::Ready(prepared) = preparation.await? else {
            return Ok(ResumeThreadOutcome::NotLaunched);
        };
        prepared.notification.update(cx, |notification, cx| {
            notification.set_state(ManagedAgentProgressState::Resuming, cx);
        });
        let notification_id =
            managed_agent_notification_id(kind.id, &prepared.installation.version);
        let result = async {
            let task = workspace.update_in(cx, |workspace, window, cx| {
                let command = build_managed_resume_command(
                    &kind,
                    &base,
                    &thread,
                    &extra_args,
                    &prepared.installation.executable_path,
                )
                .ok_or_else(|| anyhow!("{} does not support session resume", kind.label))?;
                anyhow::Ok(spawn_thread_task_for_route(
                    workspace,
                    &kind,
                    thread.title.clone(),
                    command,
                    Some(thread.session_id.clone()),
                    Some(RequiredAgentRoute(settings::RemoteAgentRoute::Tunneled)),
                    window,
                    cx,
                ))
            })??;
            task.await
        }
        .await;
        workspace.update(cx, |workspace, cx| {
            workspace.dismiss_notification(&notification_id, cx);
        })?;
        result?;
        Ok(ResumeThreadOutcome::Launched)
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumeThreadOutcome {
    Launched,
    NotLaunched,
}

fn current_remote_agent_route(
    workspace: &Workspace,
    cx: &App,
) -> Option<settings::RemoteAgentRoute> {
    let remote_client = workspace.project().read(cx).remote_client()?;
    let connection_options = remote_client.read(cx).connection_options();
    RemoteAgentRoutingSettings::get_global(cx).route_for(&connection_options)
}

pub(crate) fn workspace_uses_tunneled(workspace: &Workspace, cx: &App) -> bool {
    uses_managed_agent_route(current_remote_agent_route(workspace, cx))
}

struct NewThreadLaunch {
    command: AgentLaunchCommand,
    session_id: Option<SharedString>,
}

fn build_new_thread_launch(
    kind: &AgentKindDefinition,
    base: &AgentLaunchCommand,
    extra_args: &[String],
    managed_executable: Option<&std::path::Path>,
) -> NewThreadLaunch {
    let mut command = base.clone();
    command.args.extend(extra_args.iter().cloned());
    // Assign the session id ourselves when the CLI supports it, so a fresh
    // thread remains resumable and restorable before the CLI writes history.
    let session_id = kind.session_id_flag.map(|flag| {
        let session_id = SharedString::from(uuid::Uuid::new_v4().to_string());
        command.args.push(flag.to_string());
        command.args.push(session_id.to_string());
        session_id
    });
    if let Some(managed_executable) = managed_executable {
        command.command = Some(managed_executable.to_string_lossy().into_owned());
        apply_self_update_policy(&mut command, kind);
    }
    NewThreadLaunch {
        command,
        session_id,
    }
}

fn build_resume_command(
    kind: &AgentKindDefinition,
    base: &AgentLaunchCommand,
    thread: &HistoricalThread,
    extra_args: &[String],
    managed_executable: Option<&std::path::Path>,
) -> Option<AgentLaunchCommand> {
    let provider = kind.history_provider.as_ref()?;
    let mut command = provider.resume_command(base, thread, extra_args);
    if let Some(managed_executable) = managed_executable {
        command.command = Some(managed_executable.to_string_lossy().into_owned());
    }
    Some(command)
}

fn build_managed_resume_command(
    kind: &AgentKindDefinition,
    base: &AgentLaunchCommand,
    thread: &HistoricalThread,
    extra_args: &[String],
    managed_executable: &std::path::Path,
) -> Option<AgentLaunchCommand> {
    let mut command =
        build_resume_command(kind, base, thread, extra_args, Some(managed_executable))?;
    apply_self_update_policy(&mut command, kind);
    Some(command)
}

fn build_credential_command(base: &AgentLaunchCommand, arguments: &[&str]) -> AgentLaunchCommand {
    let mut command = base.clone();
    command.args = arguments
        .iter()
        .map(|argument| (*argument).to_string())
        .collect();
    command.initialization_command = None;
    command
}

fn build_managed_credential_command(
    kind: &AgentKindDefinition,
    base: &AgentLaunchCommand,
    arguments: &[&str],
    managed_executable: &std::path::Path,
) -> AgentLaunchCommand {
    let mut command = build_credential_command(base, arguments);
    command.command = Some(managed_executable.to_string_lossy().into_owned());
    apply_self_update_policy(&mut command, kind);
    command
}

fn uses_managed_resume(
    _kind: &AgentKindDefinition,
    route: Option<settings::RemoteAgentRoute>,
) -> bool {
    uses_managed_agent_route(route)
}

fn uses_managed_credential_command(route: Option<settings::RemoteAgentRoute>) -> bool {
    uses_managed_agent_route(route)
}

fn uses_managed_agent_route(route: Option<settings::RemoteAgentRoute>) -> bool {
    route == Some(settings::RemoteAgentRoute::Tunneled)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewThreadLaunchRoute {
    Configured,
    ManagedTunneled,
}

fn new_thread_launch_route(route: Option<settings::RemoteAgentRoute>) -> NewThreadLaunchRoute {
    if uses_managed_agent_route(route) {
        NewThreadLaunchRoute::ManagedTunneled
    } else {
        NewThreadLaunchRoute::Configured
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RequiredAgentRoute(settings::RemoteAgentRoute);

fn ensure_required_route(
    required_route: Option<RequiredAgentRoute>,
    actual_route: Option<settings::RemoteAgentRoute>,
) -> Result<()> {
    if required_route.is_none_or(|required| Some(required.0) == actual_route) {
        return Ok(());
    }
    anyhow::bail!("the agent route changed while preparing the session; launch it again")
}

fn spawn_thread(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    summary: SharedString,
    command: AgentLaunchCommand,
    resumed_session_id: Option<SharedString>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    spawn_thread_task(
        workspace,
        kind,
        summary,
        command,
        resumed_session_id,
        window,
        cx,
    )
    .detach_and_log_err(cx);
}

struct PreparedManagedAgent {
    installation: ManagedAgentInstallation,
    notification: Entity<ManagedAgentProgressNotification>,
}

enum ManagedAgentPreparation {
    Ready(PreparedManagedAgent),
    Cancelled,
    AlreadyInProgress,
}

fn prepare_managed_agent(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Task<Result<ManagedAgentPreparation>> {
    let project = workspace.project().clone();
    let Some(remote_client) = project.read(cx).remote_client() else {
        return Task::ready(Err(anyhow!(
            "managed agents are available only for remote projects"
        )));
    };
    let Some(platform) = remote_client.read(cx).platform() else {
        return Task::ready(Err(anyhow!("remote agent target could not be detected")));
    };
    let Some(release) = kind.release_for(platform).copied() else {
        return Task::ready(Err(anyhow!(
            "no pinned {} release supports this remote target",
            kind.label
        )));
    };
    let Some(remote_connection) = remote_client.read(cx).remote_connection() else {
        return Task::ready(Err(anyhow!("remote connection is unavailable")));
    };
    let key = ManagedAgentProvisioningKey {
        remote_identity: remote::remote_connection_identity(
            &remote_connection.connection_options(),
        ),
        agent_id: kind.id,
        version: release.version,
        platform,
    };
    let notification =
        cx.new(|cx| ManagedAgentProgressNotification::new(kind.label.clone(), release.version, cx));
    let store = AgentThreadStore::global(cx);
    let owner = match store.update(cx, |store, _| {
        store
            .managed_provisioning
            .begin(key.clone(), notification.clone())
    }) {
        Ok(owner) => owner,
        Err(active_notification) => {
            show_managed_agent_progress(
                workspace,
                kind.id,
                release.version,
                active_notification,
                cx,
            );
            return Task::ready(Ok(ManagedAgentPreparation::AlreadyInProgress));
        }
    };
    show_managed_agent_progress(
        workspace,
        kind.id,
        release.version,
        notification.clone(),
        cx,
    );
    let remote_host = match RemoteClientAgentHost::new(remote_client.read(cx)) {
        Ok(remote_host) => remote_host,
        Err(error) => {
            finish_managed_agent_provisioning(&store, owner, cx);
            workspace
                .dismiss_notification(&managed_agent_notification_id(kind.id, release.version), cx);
            return Task::ready(Err(error));
        }
    };
    let http_client = project.read(cx).http_client();
    let artifact_cache = store.update(cx, |store, _| {
        store
            .agent_artifact_cache
            .get_or_insert_with(|| Arc::new(AgentArtifactCache::for_app(http_client)))
            .clone()
    });
    let (progress_reporter, mut progress_receiver) = ManagedAgentProgressReporter::channel();
    let artifacts = CachedAgentArtifactSource::from_cache_with_progress(
        artifact_cache.clone(),
        kind.official_source_prefixes(),
        {
            let progress_reporter = progress_reporter.clone();
            move |progress| {
                progress_reporter.report(ManagedAgentProgressEvent::Download(progress));
            }
        },
    );
    let provisioner = ManagedAgentProvisioner::new(artifacts, remote_host);
    let progress_task = cx.spawn({
        let notification = notification.clone();
        async move |_workspace, cx| {
            while let Some(event) = progress_receiver.next().await {
                notification.update(cx, |notification, cx| {
                    notification.apply_event(event, cx);
                });
            }
        }
    });
    let kind = kind.clone();
    let notification_id = managed_agent_notification_id(kind.id, release.version);

    cx.spawn_in(window, async move |workspace, cx| {
        let result = async {
            if let Some(installation) = provisioner
                .find_installed_with_progress(kind.id, &release, {
                    let progress_reporter = progress_reporter.clone();
                    move |phase| {
                        progress_reporter.report(ManagedAgentProgressEvent::Install(phase));
                    }
                })
                .await?
            {
                return anyhow::Ok(ManagedAgentPreparation::Ready(PreparedManagedAgent {
                    installation,
                    notification: notification.clone(),
                }));
            }

            notification.update(cx, |notification, cx| {
                notification.set_state(ManagedAgentProgressState::CheckingCache, cx);
            });
            let release_is_cached = artifact_cache.release_is_cached(&release).await?;
            if !release_is_cached {
                notification.update(cx, |notification, cx| {
                    notification.set_state(
                        ManagedAgentProgressState::AwaitingConfirmation,
                        cx,
                    );
                });
                let prompt = cx.prompt(
                    PromptLevel::Info,
                    &format!(
                        "Download the official {} CLI v{}?",
                        kind.label, release.version
                    ),
                    Some("Flint will verify it locally, upload it to this remote host, and launch it by absolute path."),
                    &["Download and launch", "Cancel"],
                );
                if prompt.await? != 0 {
                    return anyhow::Ok(ManagedAgentPreparation::Cancelled);
                }
                notification.update(cx, |notification, cx| {
                    notification.set_state(
                        ManagedAgentProgressState::Downloading {
                            downloaded_bytes: 0,
                            total_bytes: None,
                        },
                        cx,
                    );
                });
            } else {
                notification.update(cx, |notification, cx| {
                    notification.set_state(ManagedAgentProgressState::CheckingCache, cx);
                });
            }

            let installation = provisioner
                .install_with_progress(kind.id, &release, {
                    let progress_reporter = progress_reporter.clone();
                    move |phase| {
                        progress_reporter
                            .report(ManagedAgentProgressEvent::Install(phase));
                    }
                })
                .await?;
            anyhow::Ok(ManagedAgentPreparation::Ready(PreparedManagedAgent {
                installation,
                notification: notification.clone(),
            }))
        }
        .await;

        drop(provisioner);
        drop(progress_reporter);
        progress_task.await;
        finish_managed_agent_provisioning_async(&store, owner, cx)?;

        match result {
            Ok(ManagedAgentPreparation::Cancelled) => {
                workspace.update(cx, |workspace, cx| {
                    workspace.dismiss_notification(&notification_id, cx);
                })?;
                Ok(ManagedAgentPreparation::Cancelled)
            }
            Ok(ready @ ManagedAgentPreparation::Ready(_)) => Ok(ready),
            Ok(ManagedAgentPreparation::AlreadyInProgress) => {
                Ok(ManagedAgentPreparation::AlreadyInProgress)
            }
            Err(error) => {
                workspace.update(cx, |workspace, cx| {
                    workspace.dismiss_notification(&notification_id, cx);
                })?;
                Err(error)
            }
        }
    })
}

fn launch_managed_thread_for_route(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    extra_args: &[String],
    required_route: Option<RequiredAgentRoute>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let preparation = prepare_managed_agent(workspace, kind, window, cx);
    let kind = kind.clone();
    let extra_args = extra_args.to_vec();
    cx.spawn_in(window, async move |workspace, cx| {
        match preparation.await {
            Ok(ManagedAgentPreparation::Ready(prepared)) => {
                prepared.notification.update(cx, |notification, cx| {
                    notification.set_state(ManagedAgentProgressState::Launching, cx);
                });
                let task = workspace.update_in(cx, |workspace, window, cx| {
                    workspace.dismiss_notification(
                        &managed_agent_notification_id(kind.id, &prepared.installation.version),
                        cx,
                    );
                    let base = AgentThreadSettings::get_global(cx).command_for_kind(kind.id);
                    let launch = build_new_thread_launch(
                        &kind,
                        base,
                        &extra_args,
                        Some(&prepared.installation.executable_path),
                    );
                    spawn_thread_task_for_route(
                        workspace,
                        &kind,
                        SharedString::from(format!("New {} thread", kind.label)),
                        launch.command,
                        launch.session_id,
                        required_route,
                        window,
                        cx,
                    )
                })?;
                if let Err(error) = task.await {
                    workspace.update(cx, |workspace, cx| workspace.show_error(&error, cx))?;
                }
            }
            Ok(ManagedAgentPreparation::Cancelled)
            | Ok(ManagedAgentPreparation::AlreadyInProgress) => {}
            Err(error) => {
                workspace.update(cx, |workspace, cx| workspace.show_error(&error, cx))?;
            }
        }
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn managed_agent_notification_id(kind_id: &str, version: &str) -> NotificationId {
    NotificationId::composite::<ManagedAgentProgressNotification>(SharedString::from(format!(
        "{kind_id}-{version}"
    )))
}

fn show_managed_agent_progress(
    workspace: &mut Workspace,
    kind_id: &str,
    version: &str,
    notification: Entity<ManagedAgentProgressNotification>,
    cx: &mut Context<Workspace>,
) {
    workspace.show_notification(managed_agent_notification_id(kind_id, version), cx, |_cx| {
        notification
    });
}

fn finish_managed_agent_provisioning(
    store: &Entity<AgentThreadStore>,
    owner: ManagedAgentProvisioningOwner,
    cx: &mut App,
) {
    let finished = store.update(cx, |store, _| store.managed_provisioning.finish(owner));
    if finished.is_none() {
        log::warn!("managed agent provisioning owner no longer owns its reservation");
    }
}

fn finish_managed_agent_provisioning_async(
    store: &Entity<AgentThreadStore>,
    owner: ManagedAgentProvisioningOwner,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<()> {
    let finished = store.update(cx, |store, _| store.managed_provisioning.finish(owner));
    if finished.is_none() {
        log::warn!("managed agent provisioning owner no longer owns its reservation");
    }
    Ok(())
}

fn spawn_thread_task(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    summary: SharedString,
    command: AgentLaunchCommand,
    resumed_session_id: Option<SharedString>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Task<Result<()>> {
    spawn_thread_task_for_route(
        workspace,
        kind,
        summary,
        command,
        resumed_session_id,
        None,
        window,
        cx,
    )
}

fn spawn_thread_task_for_route(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    summary: SharedString,
    mut command: AgentLaunchCommand,
    resumed_session_id: Option<SharedString>,
    required_route: Option<RequiredAgentRoute>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Task<Result<()>> {
    let remote_client = workspace.project().read(cx).remote_client();
    let connection_options = remote_client
        .as_ref()
        .map(|client| client.read(cx).connection_options());
    let remote_connection = remote_client
        .as_ref()
        .and_then(|client| client.read(cx).remote_connection());
    if let Some(connection_options) = connection_options.as_ref()
        && AgentThreadStore::global(cx)
            .read(cx)
            .route_change_in_progress(connection_options)
    {
        return Task::ready(Err(anyhow!(
            "the agent route is changing for this remote host"
        )));
    }
    let route = connection_options.as_ref().and_then(|connection_options| {
        RemoteAgentRoutingSettings::get_global(cx).route_for(connection_options)
    });
    if let Err(error) = ensure_required_route(required_route, route) {
        return Task::ready(Err(error));
    }
    if route != Some(settings::RemoteAgentRoute::Tunneled) {
        return spawn_thread_task_inner(
            workspace,
            kind,
            summary,
            command,
            resumed_session_id,
            remote_connection,
            None,
            window,
            cx,
        );
    }
    let Some(remote_connection) = remote_connection else {
        return Task::ready(Err(anyhow!(
            "Tunneled agent routing requires an SSH connection"
        )));
    };
    let process_connection = remote_connection.clone();
    let Some(remote_client_id) = remote_client.map(|client| client.entity_id()) else {
        return Task::ready(Err(anyhow!(
            "Tunneled agent routing requires a remote client"
        )));
    };
    let egress_manager = AgentThreadStore::global(cx).read(cx).egress_manager.clone();
    apply_self_update_policy(&mut command, kind);
    let kind = kind.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let executor = cx.background_executor().clone();
        let egress = egress_manager
            .acquire(
                remote_client_id,
                remote_connection,
                kind.egress_hosts(),
                &executor,
            )
            .await?;
        let proxy_url = egress.proxy_url();
        apply_proxy_environment(&mut command, &proxy_url);
        let task = workspace.update_in(cx, |workspace, window, cx| {
            let actual_route = workspace
                .project()
                .read(cx)
                .remote_client()
                .and_then(|client| {
                    let connection_options = client.read(cx).connection_options();
                    RemoteAgentRoutingSettings::get_global(cx).route_for(&connection_options)
                });
            ensure_required_route(required_route, actual_route)?;
            anyhow::Ok(spawn_thread_task_inner(
                workspace,
                &kind,
                summary,
                command,
                resumed_session_id,
                Some(process_connection),
                Some(egress),
                window,
                cx,
            ))
        })??;
        task.await
    })
}

fn apply_proxy_environment(command: &mut AgentLaunchCommand, proxy_url: &str) {
    for name in ["HTTPS_PROXY", "https_proxy"] {
        command.env.insert(name.to_string(), proxy_url.to_string());
    }
    for name in ["NO_PROXY", "no_proxy"] {
        command
            .env
            .insert(name.to_string(), "localhost,127.0.0.1,::1".to_string());
    }
}

fn apply_self_update_policy(command: &mut AgentLaunchCommand, kind: &AgentKindDefinition) {
    let policy = kind.self_update_policy();
    if !policy.arguments.is_empty()
        && !command
            .args
            .windows(policy.arguments.len())
            .any(|arguments| {
                arguments
                    .iter()
                    .map(String::as_str)
                    .eq(policy.arguments.iter().copied())
            })
    {
        command
            .args
            .extend(policy.arguments.iter().map(|argument| argument.to_string()));
    }
    for (name, value) in policy.environment {
        command
            .env
            .insert((*name).to_string(), (*value).to_string());
    }
}

/// A worktree a thread is tied to, plus the repository it belongs to (if
/// any). `repo_main_root` is captured at tie-assignment time rather than
/// re-derived later, because once `root` is removed from its repository's
/// worktree set there is no longer any current state from which to
/// rediscover which repository it used to belong to -- see the design doc's
/// "Deleted worktrees fall back to the main worktree" section.
#[derive(Clone)]
pub(crate) struct TiedWorktree {
    pub root: PathBuf,
    pub repo_main_root: Option<PathBuf>,
}

/// Resolves the worktree a newly-launched thread should be tied to: prefer
/// the worktree owning the project's active repository, else the first
/// visible worktree ("default to main worktree" for a project with several
/// roots and no active repository), else `None` for a worktree-less
/// project. The active-repository preference mirrors
/// `TitleBar::effective_active_worktree`
/// (`crates/title_bar/src/title_bar.rs:403-419`) -- duplicated rather than
/// shared, since pulling in a `title_bar` dependency for this one lookup
/// would be a heavier coupling than the ~15 lines it saves.
fn resolve_tied_worktree(workspace: &Workspace, cx: &App) -> Option<TiedWorktree> {
    let project = workspace.project().read(cx);

    if let Some(repo) = project.active_repository(cx) {
        let repo = repo.read(cx);
        let repo_path = &repo.work_directory_abs_path;

        for worktree in project.visible_worktrees(cx) {
            let worktree_path = worktree.read(cx).abs_path();
            if worktree_path == *repo_path || worktree_path.starts_with(repo_path.as_ref()) {
                return Some(TiedWorktree {
                    root: worktree_path.to_path_buf(),
                    repo_main_root: repo.main_worktree_abs_path().map(|path| path.to_path_buf()),
                });
            }
        }
    }

    let root = project
        .visible_worktrees(cx)
        .next()
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())?;
    Some(TiedWorktree {
        repo_main_root: repo_main_root_for_worktree(project, &root, cx),
        root,
    })
}

/// Live-worktree state gathered once per use (a panel render, or a restore
/// pass), used to resolve where a thread's tie should actually be
/// considered to point right now -- see the design doc's "Deleted
/// worktrees fall back to the main worktree" section. A flat `HashSet`
/// rather than the design doc's original `HashMap<RepositoryId, _>`
/// sketch: `effective_tie` doesn't need to discover *which* repository a
/// dangling tie belonged to from current state, because that's exactly
/// the state that's gone missing once a tie goes dangling -- callers
/// instead pass each thread's own `tied_repo_main_root`, captured at
/// tie-assignment time (see `TiedWorktree`/`AgentThreadMetadata`).
pub(crate) struct TieResolution {
    /// Every worktree path currently live across every repository the
    /// caller passed in (condition 1).
    live_worktree_roots: HashSet<PathBuf>,
    /// Worktree paths with a currently-open workspace (condition 2). Left
    /// empty for restore, which uses condition 1 only -- see the design
    /// doc's "Condition 2 is racy at restore, deliberately" note.
    open_workspace_roots: HashSet<PathBuf>,
    /// False until git state has loaded; suppresses the fallback (keeps
    /// the raw tie) rather than treating "not scanned yet" as "deleted".
    git_ready: bool,
}

impl TieResolution {
    /// A resolution that never applies the deleted-worktree fallback --
    /// `effective_tie` always returns the raw tie unchanged, matching the
    /// pre-fallback behavior. Useful for callers with no `Project` handy
    /// (or, in tests, no git repository at all) where the fallback simply
    /// doesn't apply.
    pub(crate) fn not_ready() -> Self {
        Self {
            live_worktree_roots: HashSet::default(),
            open_workspace_roots: HashSet::default(),
            git_ready: false,
        }
    }

    /// Gathers condition 1 from `project`'s visible worktrees plus every
    /// repository's linked worktrees, condition 2 from
    /// `open_workspace_roots`, and `git_ready` from the caller (see
    /// `AgentThreadsPanel::git_ready`, cached from a one-time
    /// `Project::git_scans_complete` task -- rebuilding this per render is
    /// cheap; re-awaiting readiness per render would not be).
    ///
    /// Condition 1 must include plain visible worktrees, not only
    /// git-repository worktrees: `resolve_tied_worktree`'s fallback path
    /// (no active repository) ties a thread to `visible_worktrees(cx).next()`
    /// even for a project with no git repository at all, and that same
    /// project then has no repository to contribute a `repo_main_root`
    /// either -- so without this, a plain (non-git) project's own threads
    /// would look dangling with nowhere to fall back to the moment
    /// `git_ready` turns true, and vanish from their own panel.
    pub(crate) fn new(
        project: &Project,
        open_workspace_roots: HashSet<PathBuf>,
        git_ready: bool,
        cx: &App,
    ) -> Self {
        let mut live_worktree_roots: HashSet<PathBuf> = project
            .visible_worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .collect();
        for repo in project.repositories(cx).values() {
            let repo = repo.read(cx);
            live_worktree_roots.insert(repo.work_directory_abs_path.to_path_buf());
            live_worktree_roots.extend(
                repo.linked_worktrees()
                    .iter()
                    .map(|worktree| worktree.path.clone()),
            );
        }
        Self {
            live_worktree_roots,
            open_workspace_roots,
            git_ready,
        }
    }

    /// Resolves where `tied_worktree_root` should currently be considered
    /// to point. `None` only when the tie is dangling and
    /// `tied_repo_main_root` is also `None` (e.g. a `retie-thread` target
    /// outside every repository) -- there is nowhere left to fall back to.
    pub(crate) fn effective_tie(
        &self,
        tied_worktree_root: &Path,
        tied_repo_main_root: Option<&Path>,
    ) -> Option<PathBuf> {
        if !self.git_ready
            || self.live_worktree_roots.contains(tied_worktree_root)
            || self.open_workspace_roots.contains(tied_worktree_root)
        {
            return Some(tied_worktree_root.to_path_buf());
        }
        tied_repo_main_root.map(|root| root.to_path_buf())
    }

    #[cfg(test)]
    fn for_test(
        live_worktree_roots: impl IntoIterator<Item = PathBuf>,
        open_workspace_roots: impl IntoIterator<Item = PathBuf>,
        git_ready: bool,
    ) -> Self {
        Self {
            live_worktree_roots: live_worktree_roots.into_iter().collect(),
            open_workspace_roots: open_workspace_roots.into_iter().collect(),
            git_ready,
        }
    }
}

/// Finds the repository (if any) that owns `worktree_root` -- either as its
/// main worktree or one of its linked worktrees -- and returns that
/// repository's main worktree path.
fn repo_main_root_for_worktree(
    project: &Project,
    worktree_root: &Path,
    cx: &App,
) -> Option<PathBuf> {
    project.repositories(cx).values().find_map(|repo| {
        let repo = repo.read(cx);
        let owns_root = repo.work_directory_abs_path.as_ref() == worktree_root
            || repo
                .linked_worktrees()
                .iter()
                .any(|worktree| worktree.path.as_path() == worktree_root);
        owns_root
            .then(|| repo.main_worktree_abs_path())
            .flatten()
            .map(|path| path.to_path_buf())
    })
}

/// Whether `retie_thread`'s persisted-tie write actually happened. A thread
/// with no session id yet legitimately reports `InMemoryOnly` -- not a
/// failure, see `AgentThreadStore::commit_retie` -- the tie is fully in
/// effect for the live thread either way; only the persisted-override
/// table's write is what's deferred.
// Only constructed by `retie_thread` today; see its own dead_code note --
// Stage 2's control.rs is the pending production caller.
#[allow(dead_code)]
pub(crate) enum RetiePersistence {
    Persisted,
    InMemoryOnly,
}

/// Re-ties a live thread to the worktree at `target_path`, moving its
/// terminal into that worktree's workspace (creating a background workspace
/// for it if none is open yet) so tie and ownership never diverge. See the
/// design doc's "Retie moves the terminal" and "Commit ordering" sections
/// for the invariants this implements.
// Exercised end-to-end by tests (panel.rs's retie_thread_* tests) but has
// no production caller yet -- that's Stage 2's control.rs, not yet built.
#[allow(dead_code)]
pub(crate) async fn retie_thread(
    terminal_item_id: EntityId,
    target_path: PathBuf,
    window_handle: WindowHandle<MultiWorkspace>,
    cx: &mut AsyncApp,
) -> Result<(TiedWorktree, RetiePersistence)> {
    // Step 1: resolve-or-create the destination background workspace.
    // Never activates -- see find_or_create_background_local_workspace's
    // own doc comment for why this, and not the picker's activating
    // counterpart, is used here.
    let destination_workspace = window_handle
        .update(cx, |multi_workspace, window, cx| {
            multi_workspace.find_or_create_background_local_workspace(
                util::path_list::PathList::new(&[target_path]),
                None,
                &[],
                None,
                window,
                cx,
            )
        })?
        .await?;

    // The committed tie is derived from the destination workspace's own
    // resolved worktree root, never the raw caller-supplied path: `..`
    // segments, symlinks, and platform case/spelling differences would all
    // fail the exact path-equality checks TieResolution performs.
    let new_tie = destination_workspace
        .update(cx, |workspace, cx| resolve_tied_worktree(workspace, cx))
        .ok_or_else(|| anyhow!("could not resolve a worktree root for the retie target"))?;

    // Step 2: checked move into the destination workspace's active pane.
    let store = cx.update(|cx| AgentThreadStore::global(cx));
    let (source_workspace, terminal_view) = store
        .read_with(cx, |store, _| {
            store.thread_workspace_and_terminal(terminal_item_id)
        })
        .log_err()
        .ok_or_else(|| anyhow!("agent thread is no longer live"))?;
    let source_pane = source_workspace
        .read_with(cx, |workspace, _| {
            workspace.pane_for_item_id(terminal_item_id)
        })
        .ok_or_else(|| anyhow!("agent thread pane closed"))?;
    let destination_pane =
        destination_workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

    // Only follow the terminal into its new worktree if the user could
    // already see it -- i.e. its source workspace was the window's
    // foreground one. A background agent retying itself must not yank the
    // user out of whatever worktree they're actually looking at; but a
    // terminal the user was watching would otherwise just vanish from the
    // window once its pane item moves below, with no visible explanation.
    let source_was_active = window_handle
        .read_with(cx, |multi_workspace, _| {
            multi_workspace.workspace() == &source_workspace
        })
        .unwrap_or(false);

    if source_pane != destination_pane {
        window_handle.update(cx, |_, window, cx| {
            workspace::move_item_checked(
                &source_pane,
                &destination_pane,
                terminal_item_id,
                destination_pane.read(cx).items_len(),
                false,
                window,
                cx,
            )
        })??;
    }
    // `move_item_checked` (when it ran) triggered `TerminalView::added_to_workspace`
    // as a side effect of the pane-add, which already called `reparent` --
    // `self.workspace`/`self.project`/`_terminal_subscriptions` on the
    // terminal view are already correct by this point. Nothing left to do
    // for ownership beyond the store-side commit below.
    let _ = terminal_view;

    if source_was_active {
        window_handle.update(cx, |multi_workspace, window, cx| {
            multi_workspace.activate(
                destination_workspace.clone(),
                Some(source_workspace.downgrade()),
                window,
                cx,
            );
        })?;
    }

    // Step 3: commit in-memory (infallible now that the move succeeded).
    let committed_tie = new_tie.clone();
    let store = cx.update(|cx| AgentThreadStore::global(cx));
    store.update(cx, |store, cx| {
        store.commit_retie(terminal_item_id, destination_workspace, new_tie, cx)
    })?;

    // Step 4: persist, awaited -- so this function's return value is a
    // truthful account of what happened, not an assumption.
    let Some((kind_id, session_id, tie)) = store.read_with(cx, |store, _| {
        store.tie_override_to_persist(terminal_item_id)
    }) else {
        return Ok((committed_tie, RetiePersistence::InMemoryOnly));
    };
    write_tie_override(cx, kind_id, &session_id, &tie).await?;
    Ok((committed_tie, RetiePersistence::Persisted))
}

fn prepare_remote_thread_process(
    command: &mut AgentLaunchCommand,
    remote_connection: Option<Arc<dyn remote::RemoteConnection>>,
    is_windows: bool,
    lifecycle_id: uuid::Uuid,
) -> Result<Option<RemoteAgentProcess>> {
    if is_windows {
        return Ok(None);
    }
    let Some(remote_connection) = remote_connection else {
        return Ok(None);
    };
    RemoteAgentProcess::prepare(command, remote_connection, lifecycle_id).map(Some)
}

fn spawn_thread_task_inner(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    summary: SharedString,
    mut command: AgentLaunchCommand,
    resumed_session_id: Option<SharedString>,
    remote_connection: Option<Arc<dyn remote::RemoteConnection>>,
    egress: Option<AgentEgressLease>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Task<Result<()>> {
    let Some(cwd) = command
        .cwd
        .clone()
        .or_else(|| terminal_view::default_working_directory(workspace, cx))
    else {
        return Task::ready(Err(anyhow!(
            "agent thread working directory is unavailable"
        )));
    };
    let tied_worktree = resolve_tied_worktree(workspace, cx).unwrap_or_else(|| TiedWorktree {
        root: cwd.clone(),
        repo_main_root: None,
    });
    let kind_id = kind.id;
    let kind_icon = kind.icon;
    let title = summary.clone();
    let label = summary.to_string();
    let command_label = command_label(&command, &label);
    let initialization_command = command.initialization_command.take();
    let is_windows = workspace.project().read(cx).path_style(cx).is_windows();
    let remote_client = workspace.project().read(cx).remote_client();
    let remote_process = match prepare_remote_thread_process(
        &mut command,
        remote_connection,
        is_windows,
        uuid::Uuid::new_v4(),
    ) {
        Ok(remote_process) => remote_process,
        Err(error) => return Task::ready(Err(error)),
    };
    let mut task = SpawnInTerminal {
        full_label: label.clone(),
        label,
        command: command.command,
        args: command.args,
        command_label,
        cwd: Some(cwd.clone()),
        env: command.env,
        use_new_terminal: true,
        allow_concurrent_runs: true,
        reveal: RevealStrategy::Always,
        reveal_target: RevealTarget::Center,
        shell: Shell::System,
        show_summary: false,
        show_command: false,
        show_rerun: false,
        ..SpawnInTerminal::default()
    };
    if let Some(initialization_command) = initialization_command {
        let shell = remote_client
            .as_ref()
            .and_then(|remote_client| remote_client.read(cx).shell())
            .unwrap_or_else(|| {
                if remote_client.is_some() {
                    util::shell::get_default_system_shell()
                } else {
                    util::shell::get_system_shell()
                }
            });
        task = project::terminals::wrap_task_with_initialization_command(
            task,
            &initialization_command,
            &shell,
            is_windows,
        );
    } else if is_windows && remote_client.is_none() {
        task = project::terminals::wrap_task_in_system_shell(
            task,
            &util::shell::get_system_shell(),
            true,
        );
    }

    let workspace_entity = cx.entity();
    let window_handle = window.window_handle().downcast::<MultiWorkspace>();
    let launched_at = SystemTime::now();
    let terminal_view_task =
        TerminalPanel::add_center_terminal_view(workspace, window, cx, |project, cx| {
            project.create_terminal_task(task, cx)
        });
    cx.spawn_in(window, async move |_workspace, cx| {
        let (_, terminal_view) = terminal_view_task.await?;
        let terminal_view = terminal_view
            .upgrade()
            .ok_or_else(|| anyhow!("agent thread terminal closed before registration"))?;
        terminal_view.update(cx, |terminal_view, cx| {
            terminal_view.set_tab_icon_override(Some(kind_icon), cx);
            terminal_view.set_agent_thread(true, cx);
        });
        let store = cx.update(|_, cx| AgentThreadStore::global(cx))?;
        store.update(cx, |store, cx| {
            store.register(
                kind_id,
                title,
                cwd,
                tied_worktree,
                resumed_session_id,
                launched_at,
                workspace_entity,
                terminal_view,
                window_handle,
                remote_process,
                egress,
                cx,
            );
        });
        anyhow::Ok(())
    })
}

pub fn snapshot_live_agent_threads(session_id: String, cx: &mut App) -> Task<Result<()>> {
    let store = AgentThreadStore::global(cx);
    let records = store.read(cx).session_restore_records(cx);
    enqueue_session_restore_snapshot(session_id, records, &store, cx)
}

pub fn checkpoint_live_agent_threads(session_id: String, cx: &mut App) -> Option<Task<Result<()>>> {
    let store = AgentThreadStore::global(cx);
    let records = store.read(cx).session_restore_records(cx);
    (!records.is_empty()).then(|| enqueue_session_restore_snapshot(session_id, records, &store, cx))
}

fn enqueue_session_restore_snapshot(
    session_id: String,
    records: Vec<AgentThreadSessionRestoreRecord>,
    store: &Entity<AgentThreadStore>,
    cx: &App,
) -> Task<Result<()>> {
    let records_json = match serde_json::to_string(&records) {
        Ok(records_json) => records_json,
        Err(error) => return Task::ready(Err(error.into())),
    };
    let (completion, receiver) = oneshot::channel();
    let request = SnapshotRequest {
        session_id,
        records_json,
        completion,
    };
    if store
        .read(cx)
        .snapshot_sender
        .unbounded_send(request)
        .is_err()
    {
        return Task::ready(Err(anyhow!("agent thread snapshot writer stopped")));
    }
    cx.background_spawn(async move {
        receiver
            .await
            .map_err(|_| anyhow!("agent thread snapshot writer stopped"))?
    })
}

/// Each app launch persists its live snapshot under a fresh session id
/// (see `Session::new`), and only the immediately preceding session's
/// snapshot is ever read again (by startup restore). Older sessions'
/// snapshots are dead the moment a second-newer session starts, so without
/// this they accumulate in the database forever, one row per launch.
pub fn prune_stale_session_restore_snapshots(
    current_session_id: String,
    previous_session_id: Option<String>,
    cx: &mut App,
) -> Task<Result<()>> {
    let key_value_store = db::kvp::KeyValueStore::global(cx);
    cx.background_spawn(async move {
        let scoped = key_value_store.scoped(SESSION_RESTORE_NAMESPACE);
        let keys = scoped.keys()?;
        let keep: HashSet<&str> = [
            Some(current_session_id.as_str()),
            previous_session_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();
        for key in stale_session_restore_keys(keys, &keep) {
            scoped.delete(key).await?;
        }
        Ok(())
    })
}

fn stale_session_restore_keys(keys: Vec<String>, keep: &HashSet<&str>) -> Vec<String> {
    keys.into_iter()
        .filter(|key| !keep.contains(key.as_str()))
        .collect()
}

pub fn restore_threads_for_workspace(
    workspace: &mut Workspace,
    last_session_id: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Task<usize> {
    let Some(workspace_id) = workspace.database_id() else {
        return Task::ready(0);
    };

    let store = AgentThreadStore::global(cx);
    let first_attempt = store.update(cx, |store, _| store.restore_attempted.insert(workspace_id));
    if !first_attempt {
        return Task::ready(0);
    }

    let records = match restore_records_for_session(last_session_id, cx) {
        Ok(records) => records,
        Err(error) => {
            log::error!("Failed to read agent thread restore snapshot: {error:#}");
            return Task::ready(1);
        }
    };

    let live_threads = store.read(cx).live_threads_for_workspace(workspace_id, cx);
    let own_project_roots = workspace.project().read_with(cx, |project, cx| {
        history::project_worktree_roots(project, cx)
    });
    let records =
        records_to_restore_for_workspace(workspace_id, &own_project_roots, &records, &live_threads);
    let settings = AgentThreadSettings::get_global(cx).clone();
    let mut restores = Vec::new();

    for record in records {
        let Some(kind) = agent_kind_registry()
            .into_iter()
            .find(|kind| kind.id == record.kind_id)
        else {
            log::warn!(
                "Skipping agent thread restore for unknown kind {:?}",
                record.kind_id
            );
            continue;
        };
        if settings.command_for_kind(kind.id).hidden {
            continue;
        }

        let thread = HistoricalThread {
            session_id: SharedString::from(record.session_id),
            title: SharedString::from(record.title),
            project_root: record.project_root,
            last_activity_at: system_time_from_millis(record.last_activity_at),
        };
        let extra_args = resolve_thread_launch_args(cx, &kind, &thread.session_id);
        restores.push((kind, thread, extra_args));
    }

    cx.spawn_in(window, async move |workspace, cx| {
        run_thread_restores_sequentially(restores, |(kind, thread, extra_args)| {
            let kind_id = kind.id;
            let session_id = thread.session_id.to_string();
            let task = workspace.update_in(cx, |workspace, window, cx| {
                resume_thread_task(workspace, &kind, &thread, &extra_args, window, cx)
            });
            async move {
                let result = async {
                    let Some(task) = task? else {
                        return anyhow::Ok(ResumeThreadOutcome::NotLaunched);
                    };
                    task.await
                }
                .await;
                if let Err(error) = &result {
                    log::error!("Failed to reopen {kind_id} agent session {session_id}: {error:#}");
                } else if matches!(&result, Ok(ResumeThreadOutcome::NotLaunched)) {
                    log::info!("Did not reopen {kind_id} agent session {session_id}");
                }
                result
            }
        })
        .await
    })
}

async fn run_thread_restores_sequentially<Item, Launch, LaunchFuture>(
    items: Vec<Item>,
    mut launch: Launch,
) -> usize
where
    Launch: FnMut(Item) -> LaunchFuture,
    LaunchFuture: std::future::Future<Output = Result<ResumeThreadOutcome>>,
{
    let mut failure_count = 0;
    for item in items {
        match launch(item).await {
            Ok(ResumeThreadOutcome::Launched) => {}
            Ok(ResumeThreadOutcome::NotLaunched) | Err(_) => failure_count += 1,
        }
    }
    failure_count
}

fn restore_records_for_session(
    session_id: &str,
    cx: &App,
) -> Result<Vec<AgentThreadSessionRestoreRecord>> {
    let Some(records_json) = db::kvp::KeyValueStore::global(cx)
        .scoped(SESSION_RESTORE_NAMESPACE)
        .read(session_id)?
    else {
        return Ok(Vec::new());
    };
    Ok(serde_json::from_str(&records_json)?)
}

fn snapshot_records_for_workspace(
    workspace_id: WorkspaceId,
    live_threads: impl IntoIterator<Item = AgentThreadMetadata>,
) -> Vec<AgentThreadSessionRestoreRecord> {
    live_threads
        .into_iter()
        .filter_map(|thread| {
            let session_id = thread.resumed_session_id?;
            Some(AgentThreadSessionRestoreRecord {
                workspace_id,
                kind_id: thread.kind_id.to_string(),
                session_id: session_id.to_string(),
                title: thread.title.to_string(),
                project_root: thread.project_root,
                // Always populated, not only for retied threads: every
                // *new* snapshot carries a resolved tie, so restore routing
                // never has to fall back to path-comparing project_root
                // (which can be a subdirectory, not a worktree root) for a
                // record written after this field existed.
                tied_worktree_root: Some(thread.tied_worktree_root),
                last_activity_at: system_time_to_millis(thread.launched_at),
            })
        })
        .collect()
}

/// Selects the records `workspace_id` (whose own worktree roots are
/// `own_project_roots`) should restore. Routes by effective tie for records
/// that carry one (every record written since `tied_worktree_root` was
/// added); legacy records (`None`, written before) fall back to their
/// original `workspace_id`-based routing, exactly as before this field
/// existed -- never by comparing `own_project_roots` against `project_root`,
/// which is a launch cwd, not necessarily a worktree root.
///
/// Once a record is selected here, the actual resume launches into *this*
/// restoring workspace via the normal `spawn_thread_task_inner` path, which
/// re-derives `tied_worktree_root` fresh via `resolve_tied_worktree` rather
/// than being handed the record's tie directly -- deliberately not plumbed
/// through as an explicit override. For the common single-root-workspace
/// case (this filter already guarantees the workspace's own root matches
/// the tie) the two always agree, so this is not a behavior gap in the
/// case that matters; a multi-root workspace whose `active_repository`
/// picks a different one of its own roots than the specific one that was
/// tied is the only case where they could diverge, left as a known,
/// narrow gap rather than threading an override through the whole launch
/// pipeline for it.
fn records_to_restore_for_workspace(
    workspace_id: WorkspaceId,
    own_project_roots: &[PathBuf],
    records: &[AgentThreadSessionRestoreRecord],
    live_threads: &[AgentThreadMetadata],
) -> Vec<AgentThreadSessionRestoreRecord> {
    records
        .iter()
        .filter(|record| match &record.tied_worktree_root {
            Some(tied_root) => own_project_roots.contains(tied_root),
            None => record.workspace_id == workspace_id,
        })
        .filter(|record| {
            !live_threads.iter().any(|thread| {
                thread.kind_id == record.kind_id
                    && thread
                        .resumed_session_id
                        .as_ref()
                        .is_some_and(|session_id| session_id.as_ref() == record.session_id)
            })
        })
        .cloned()
        .collect()
}

fn system_time_to_millis(time: SystemTime) -> u64 {
    let millis = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn system_time_from_millis(millis: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(millis)
}

fn command_label(command: &AgentLaunchCommand, fallback: &str) -> String {
    let Some(command_name) = command.command.as_ref() else {
        return fallback.to_string();
    };
    std::iter::once(command_name.as_str())
        .chain(command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn init(cx: &mut App) {
    AgentThreadStore::init_global(cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use pretty_assertions::assert_eq;
    use project::{FakeFs, Project};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use workspace::MultiWorkspace;

    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[gpui::test]
    async fn resolve_tied_worktree_falls_back_to_the_first_visible_worktree(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [Path::new("/root")], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let resolved = window_handle
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.read_with(cx, |workspace, cx| resolve_tied_worktree(workspace, cx))
            })
            .expect("window should be live");
        let resolved = resolved.expect("a single-worktree project should resolve a tie");

        // No git repository in this fixture, so there's no repo to fall
        // back to if this worktree later vanishes.
        assert_eq!(resolved.root, PathBuf::from("/root"));
        assert_eq!(resolved.repo_main_root, None);
    }

    #[gpui::test]
    async fn tie_resolution_keeps_a_plain_non_git_projects_own_worktree_live(
        cx: &mut TestAppContext,
    ) {
        // Regression test: TieResolution::new used to seed live_worktree_roots
        // only from project.repositories(cx). A project with no git
        // repository at all has no repository to contribute either a live
        // root or a repo_main_root fallback, so once git_ready turned true
        // every thread in a plain (non-git) project would look dangling with
        // nowhere to fall back to, and vanish from its own panel.
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [Path::new("/root")], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));

        let tied = window_handle
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.read_with(cx, |workspace, cx| resolve_tied_worktree(workspace, cx))
            })
            .expect("window should be live")
            .expect("a single-worktree project should resolve a tie");

        let effective = cx.update(|cx| {
            let resolution = TieResolution::new(project.read(cx), HashSet::default(), true, cx);
            resolution.effective_tie(&tied.root, tied.repo_main_root.as_deref())
        });

        assert_eq!(effective, Some(PathBuf::from("/root")));
    }

    #[gpui::test]
    async fn resolve_tied_worktree_is_none_for_a_worktree_less_project(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let resolved = window_handle
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.read_with(cx, |workspace, cx| resolve_tied_worktree(workspace, cx))
            })
            .expect("window should be live");

        assert!(resolved.is_none());
    }

    #[gpui::test]
    async fn resolve_tied_worktree_prefers_the_active_repository_and_captures_its_main_root(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/root",
            serde_json::json!({
                "project": {
                    ".git": {},
                    "src": { "main.rs": "fn main() {}" },
                },
            }),
        )
        .await;

        let project_root = PathBuf::from("/root/project");
        let project = Project::test(fs, [project_root.as_path()], cx).await;
        project
            .update(cx, |project, cx| project.git_scans_complete(cx))
            .await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));

        let resolved = window_handle
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.read_with(cx, |workspace, cx| resolve_tied_worktree(workspace, cx))
            })
            .expect("window should be live");
        let resolved = resolved.expect("a git-repository project should resolve a tie");

        assert_eq!(resolved.root, project_root);
        assert_eq!(resolved.repo_main_root, Some(project_root));
    }

    #[test]
    fn effective_tie_is_unchanged_while_the_worktree_is_live() {
        let resolution = TieResolution::for_test([PathBuf::from("/repo/x")], [], true);
        assert_eq!(
            resolution.effective_tie(Path::new("/repo/x"), None),
            Some(PathBuf::from("/repo/x"))
        );
    }

    #[test]
    fn effective_tie_stays_put_when_a_workspace_is_open_even_if_the_repo_forgot_it() {
        // Condition 2: the worktree isn't in the repo's own worktree set
        // (as if it had just been externally removed), but a workspace is
        // still open there -- the regression test for the divergence
        // condition 2 exists to prevent (see the design doc).
        let resolution = TieResolution::for_test([], [PathBuf::from("/repo/x")], true);
        assert_eq!(
            resolution.effective_tie(Path::new("/repo/x"), Some(&PathBuf::from("/repo"))),
            Some(PathBuf::from("/repo/x"))
        );
    }

    #[test]
    fn effective_tie_falls_back_to_the_repo_main_root_once_dangling() {
        let resolution = TieResolution::for_test([], [], true);
        assert_eq!(
            resolution.effective_tie(Path::new("/repo/x"), Some(&PathBuf::from("/repo"))),
            Some(PathBuf::from("/repo"))
        );
    }

    #[test]
    fn effective_tie_is_none_when_dangling_with_no_repo_to_fall_back_to() {
        let resolution = TieResolution::for_test([], [], true);
        assert_eq!(
            resolution.effective_tie(Path::new("/elsewhere/x"), None),
            None
        );
    }

    #[test]
    fn effective_tie_keeps_the_raw_tie_while_git_state_is_not_ready() {
        // git_ready: false must suppress the fallback entirely, even though
        // the tie would otherwise look dangling -- "not scanned yet" must
        // never be treated as "deleted".
        let resolution = TieResolution::for_test([], [], false);
        assert_eq!(
            resolution.effective_tie(Path::new("/repo/x"), Some(&PathBuf::from("/repo"))),
            Some(PathBuf::from("/repo/x"))
        );
    }

    fn live(id: u64, launched_at: u64, resumed_session_id: Option<&str>) -> AgentThreadMetadata {
        AgentThreadMetadata {
            terminal_item_id: EntityId::from(id),
            kind_id: "codex",
            title: SharedString::from("live"),
            project_root: PathBuf::from("/root"),
            tied_worktree_root: PathBuf::from("/root"),
            tied_repo_main_root: None,
            launched_at: at(launched_at),
            resumed_session_id: resumed_session_id.map(SharedString::from),
        }
    }

    fn live_with_kind(
        id: u64,
        kind_id: &'static str,
        resumed_session_id: Option<&str>,
    ) -> AgentThreadMetadata {
        AgentThreadMetadata {
            kind_id,
            ..live(id, 100, resumed_session_id)
        }
    }

    fn historical(session_id: &str, last_activity_at: u64) -> HistoricalThread {
        HistoricalThread {
            session_id: SharedString::from(session_id),
            title: SharedString::from("historical"),
            project_root: PathBuf::from("/root"),
            last_activity_at: at(last_activity_at),
        }
    }

    #[test]
    fn managed_resume_replaces_only_the_configured_executable() {
        let kind = agent_kind_registry()
            .into_iter()
            .find(|kind| kind.id == "codex")
            .expect("Codex should be registered");
        let mut environment = HashMap::default();
        environment.insert("CODEX_HOME".to_string(), "/remote/codex-home".to_string());
        let base = AgentLaunchCommand {
            command: Some("codex".to_string()),
            env: environment,
            initialization_command: Some("source ~/.profile".to_string()),
            ..AgentLaunchCommand::default()
        };
        let managed_executable =
            PathBuf::from("/remote/flint/agents/codex/0.144.6/linux-x86_64-glibc/codex");

        let command = build_managed_resume_command(
            &kind,
            &base,
            &historical("session-a", 10),
            &["--dangerously-bypass-approvals-and-sandbox".to_string()],
            &managed_executable,
        )
        .expect("Codex should support resume");

        assert_eq!(command.command.as_deref(), managed_executable.to_str());
        assert_eq!(
            command.args,
            vec![
                "resume",
                "session-a",
                "--dangerously-bypass-approvals-and-sandbox",
                "--config",
                "check_for_update_on_startup=false"
            ]
        );
        assert_eq!(
            command.env.get("CODEX_HOME").map(String::as_str),
            Some("/remote/codex-home")
        );
        assert_eq!(
            command.initialization_command.as_deref(),
            Some("source ~/.profile")
        );
    }

    #[test]
    fn new_thread_builder_preserves_options_environment_and_managed_executable() {
        for kind in agent_kind_registry() {
            let mut environment = HashMap::default();
            environment.insert("EXISTING".to_string(), "value".to_string());
            let base = AgentLaunchCommand {
                command: Some(format!("ambient-{}", kind.id)),
                args: vec!["base".to_string()],
                env: environment,
                initialization_command: Some("source ~/.profile".to_string()),
                ..AgentLaunchCommand::default()
            };
            let managed_executable = PathBuf::from(format!("/managed/{}/cli", kind.id));
            let option_arguments = kind
                .resume_options
                .first()
                .map(|option| option.args.clone())
                .unwrap_or_default();

            let launch =
                build_new_thread_launch(&kind, &base, &option_arguments, Some(&managed_executable));

            assert_eq!(
                launch.command.command.as_deref(),
                managed_executable.to_str()
            );
            assert_eq!(
                launch.command.env.get("EXISTING").map(String::as_str),
                Some("value")
            );
            assert_eq!(
                launch.command.initialization_command.as_deref(),
                Some("source ~/.profile")
            );
            let mut expected_prefix = vec!["base".to_string()];
            expected_prefix.extend(option_arguments);
            assert!(launch.command.args.starts_with(&expected_prefix));
            let mut expected = launch.command.clone();
            apply_self_update_policy(&mut expected, &kind);
            assert_eq!(launch.command, expected);
        }
    }

    #[test]
    fn every_agent_preserves_direct_and_tunneled_executable_selection() {
        for kind in agent_kind_registry() {
            let ambient_command = format!("ambient-{}", kind.id);
            let base = AgentLaunchCommand {
                command: Some(ambient_command.clone()),
                initialization_command: Some("source ~/.profile".to_string()),
                ..AgentLaunchCommand::default()
            };

            let direct = build_new_thread_launch(&kind, &base, &[], None);
            assert_eq!(
                direct.command.command.as_deref(),
                Some(ambient_command.as_str()),
                "{} Direct route should use the configured ambient executable",
                kind.id
            );
            assert_eq!(
                direct.command.initialization_command.as_deref(),
                Some("source ~/.profile")
            );

            let managed_executable = PathBuf::from(format!("/managed/{}/cli", kind.id));
            let tunneled = build_new_thread_launch(&kind, &base, &[], Some(&managed_executable));
            assert_eq!(
                tunneled.command.command.as_deref(),
                managed_executable.to_str(),
                "{} Tunneled route should use its pinned managed executable",
                kind.id
            );
            assert_eq!(
                tunneled.command.initialization_command.as_deref(),
                Some("source ~/.profile")
            );
        }
    }

    #[test]
    fn managed_agents_keep_their_generated_session_ids() {
        for kind_id in ["claude", "pi"] {
            let kind = agent_kind_registry()
                .into_iter()
                .find(|kind| kind.id == kind_id)
                .expect("agent should be registered");
            let launch = build_new_thread_launch(
                &kind,
                &AgentLaunchCommand::default(),
                &[],
                Some(std::path::Path::new("/managed/agent")),
            );

            let session_id = launch
                .session_id
                .expect("managed launch should have a session id");
            assert!(
                launch
                    .command
                    .args
                    .ends_with(&["--session-id".to_string(), session_id.to_string(),])
            );
        }
    }

    #[test]
    fn configured_credential_command_keeps_the_ambient_executable() {
        let base = AgentLaunchCommand {
            command: Some("custom-codex".to_string()),
            args: vec!["ignored".to_string()],
            initialization_command: Some("source ~/.profile".to_string()),
            ..AgentLaunchCommand::default()
        };

        let command = build_credential_command(&base, &["logout"]);

        assert_eq!(command.command.as_deref(), Some("custom-codex"));
        assert_eq!(command.args, vec!["logout".to_string()]);
        assert!(command.initialization_command.is_none());
    }

    #[test]
    fn managed_credential_commands_use_each_pinned_executable_and_update_policy() {
        for kind in agent_kind_registry() {
            let mut environment = HashMap::default();
            environment.insert("EXISTING".to_string(), "value".to_string());
            let base = AgentLaunchCommand {
                command: Some(format!("custom-{}", kind.id)),
                env: environment,
                ..AgentLaunchCommand::default()
            };
            let managed_executable = PathBuf::from(format!("/managed/{}/cli", kind.id));
            let Some(credential_policy) = kind.credential_policy() else {
                continue;
            };
            let arguments = credential_policy.logout_arguments;

            let command =
                build_managed_credential_command(&kind, &base, arguments, &managed_executable);

            assert_eq!(command.command.as_deref(), managed_executable.to_str());
            assert_eq!(
                command.env.get("EXISTING").map(String::as_str),
                Some("value")
            );
            let mut expected = build_credential_command(&base, arguments);
            expected.command = Some(managed_executable.to_string_lossy().into_owned());
            apply_self_update_policy(&mut expected, &kind);
            assert_eq!(command, expected);
        }
    }

    #[test]
    fn only_tunneled_credential_commands_use_managed_provisioning() {
        assert!(uses_managed_credential_command(Some(
            settings::RemoteAgentRoute::Tunneled
        )));
        assert!(!uses_managed_credential_command(Some(
            settings::RemoteAgentRoute::Direct
        )));
        assert!(!uses_managed_credential_command(None));
    }

    #[test]
    fn local_agent_launch_is_not_wrapped() {
        let mut command = AgentLaunchCommand {
            command: Some("codex".into()),
            args: vec!["resume".into(), "session-a".into()],
            ..AgentLaunchCommand::default()
        };
        let original = command.clone();

        let process = prepare_remote_thread_process(&mut command, None, false, uuid::Uuid::nil())
            .expect("local launch preparation");

        assert!(process.is_none());
        assert_eq!(command, original);
    }

    #[gpui::test]
    async fn shutdown_resource_is_retained_until_cleanup_finishes(cx: &mut gpui::TestAppContext) {
        let dropped = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = async_channel::bounded(1);
        let task = cx.background_executor.spawn(retain_resource_until_shutdown(
            DropProbe(dropped.clone()),
            async move {
                release_rx.recv().await?;
                anyhow::Ok(())
            },
        ));

        cx.run_until_parked();
        assert!(!dropped.load(Ordering::SeqCst));
        release_tx.send(()).await.expect("release shutdown");
        task.await.expect("shutdown result");
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[gpui::test]
    async fn snapshot_writer_persists_requests_in_order(cx: &mut gpui::TestAppContext) {
        let (sender, receiver) = mpsc::unbounded();
        let writes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let writer = cx.background_executor.spawn(run_snapshot_writer(receiver, {
            let writes = writes.clone();
            move |session_id, records_json| {
                let writes = writes.clone();
                async move {
                    writes.lock().push((session_id, records_json));
                    Ok(())
                }
            }
        }));

        let (first_completion, first_result) = oneshot::channel();
        sender
            .unbounded_send(SnapshotRequest {
                session_id: "session".to_string(),
                records_json: "first".to_string(),
                completion: first_completion,
            })
            .expect("snapshot writer should accept first request");
        let (second_completion, second_result) = oneshot::channel();
        sender
            .unbounded_send(SnapshotRequest {
                session_id: "session".to_string(),
                records_json: "second".to_string(),
                completion: second_completion,
            })
            .expect("snapshot writer should accept second request");
        drop(sender);

        writer.await;
        first_result
            .await
            .expect("first snapshot response")
            .expect("first snapshot write");
        second_result
            .await
            .expect("second snapshot response")
            .expect("second snapshot write");
        assert_eq!(
            *writes.lock(),
            vec![
                ("session".to_string(), "first".to_string()),
                ("session".to_string(), "second".to_string())
            ]
        );
    }

    #[test]
    fn stale_session_restore_keys_keeps_current_and_previous_only() {
        let keys = vec![
            "session-1".to_string(),
            "session-2".to_string(),
            "session-3".to_string(),
        ];
        let keep = HashSet::from_iter(["session-2", "session-3"]);

        assert_eq!(
            stale_session_restore_keys(keys, &keep),
            vec!["session-1".to_string()]
        );
    }

    #[test]
    fn stale_session_restore_keys_with_no_previous_session_keeps_only_current() {
        let keys = vec!["session-1".to_string(), "session-2".to_string()];
        let keep = HashSet::from_iter(["session-2"]);

        assert_eq!(
            stale_session_restore_keys(keys, &keep),
            vec!["session-1".to_string()]
        );
    }

    #[test]
    fn repeated_shutdown_cannot_take_the_same_entry() {
        let id = EntityId::from(7);
        let mut entries = HashMap::default();
        entries.insert(id, "thread");

        assert_eq!(take_thread_for_shutdown(&mut entries, id), Some("thread"));
        assert_eq!(take_thread_for_shutdown(&mut entries, id), None);
    }

    #[test]
    fn only_tunneled_resume_uses_managed_resolution_for_both_agents() {
        for kind in agent_kind_registry() {
            assert!(uses_managed_resume(
                &kind,
                Some(settings::RemoteAgentRoute::Tunneled)
            ));
            assert!(!uses_managed_resume(
                &kind,
                Some(settings::RemoteAgentRoute::Direct)
            ));
            assert!(!uses_managed_resume(&kind, None));
        }
    }

    #[test]
    fn new_thread_launch_route_is_managed_only_tunneled() {
        assert_eq!(
            new_thread_launch_route(Some(settings::RemoteAgentRoute::Tunneled)),
            NewThreadLaunchRoute::ManagedTunneled
        );
        assert_eq!(
            new_thread_launch_route(Some(settings::RemoteAgentRoute::Direct)),
            NewThreadLaunchRoute::Configured
        );
        assert_eq!(
            new_thread_launch_route(None),
            NewThreadLaunchRoute::Configured
        );
    }

    #[test]
    fn required_resume_route_rejects_a_route_change() {
        let required = RequiredAgentRoute(settings::RemoteAgentRoute::Tunneled);

        ensure_required_route(Some(required), Some(settings::RemoteAgentRoute::Tunneled))
            .expect("unchanged route should be accepted");
        let error = ensure_required_route(Some(required), Some(settings::RemoteAgentRoute::Direct))
            .expect_err("changed route should be rejected");

        assert_eq!(
            error.to_string(),
            "the agent route changed while preparing the session; launch it again"
        );
    }

    #[gpui::test]
    async fn restoration_sequence_waits_for_each_resume_before_starting_the_next(
        cx: &mut gpui::TestAppContext,
    ) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let executor = cx.background_executor.clone();

        let failure_count = run_thread_restores_sequentially(vec![1, 2], {
            let events = events.clone();
            move |item| {
                let events = events.clone();
                let executor = executor.clone();
                async move {
                    events.borrow_mut().push(format!("start {item}"));
                    executor.timer(Duration::from_millis(1)).await;
                    events.borrow_mut().push(format!("finish {item}"));
                    anyhow::Ok(ResumeThreadOutcome::Launched)
                }
            }
        })
        .await;

        assert_eq!(failure_count, 0);
        assert_eq!(
            events.borrow().as_slice(),
            ["start 1", "finish 1", "start 2", "finish 2"]
        );
    }

    #[gpui::test]
    async fn restoration_sequence_counts_skipped_and_failed_resumes() {
        let failure_count = run_thread_restores_sequentially(vec![0, 1, 2], |item| async move {
            match item {
                0 => Ok(ResumeThreadOutcome::Launched),
                1 => Ok(ResumeThreadOutcome::NotLaunched),
                _ => Err(anyhow!("launch failed")),
            }
        })
        .await;

        assert_eq!(failure_count, 2);
    }

    fn live_label(id: u64) -> String {
        format!("live:{}", EntityId::from(id).as_u64())
    }

    fn row_session_ids(rows: &[AgentThreadRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                AgentThreadRow::FreshLive(metadata) => {
                    format!("live:{}", metadata.terminal_item_id.as_u64())
                }
                AgentThreadRow::Historical {
                    thread,
                    live_terminal_item_id,
                } => match live_terminal_item_id {
                    Some(terminal_item_id) => format!(
                        "historical-live:{}:{}",
                        thread.session_id,
                        terminal_item_id.as_u64()
                    ),
                    None => format!("historical:{}", thread.session_id),
                },
            })
            .collect()
    }

    #[test]
    fn tunneled_environment_is_scoped_to_https_and_replaces_bypass_rules() {
        let mut command = AgentLaunchCommand::default();
        command
            .env
            .insert("NO_PROXY".to_string(), "metadata.internal".to_string());

        apply_proxy_environment(&mut command, "http://flint:redacted@127.0.0.1:43123");

        assert_eq!(
            command.env.get("HTTPS_PROXY").map(String::as_str),
            Some("http://flint:redacted@127.0.0.1:43123")
        );
        assert_eq!(
            command.env.get("https_proxy").map(String::as_str),
            Some("http://flint:redacted@127.0.0.1:43123")
        );
        assert_eq!(
            command.env.get("NO_PROXY").map(String::as_str),
            Some("localhost,127.0.0.1,::1")
        );
        assert_eq!(
            command.env.get("no_proxy").map(String::as_str),
            Some("localhost,127.0.0.1,::1")
        );
        for name in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            assert!(!command.env.contains_key(name));
        }
    }

    #[test]
    fn self_update_suppression_is_idempotent() {
        for kind in agent_kind_registry() {
            let mut command = AgentLaunchCommand::default();
            apply_self_update_policy(&mut command, &kind);
            let once = command.clone();
            apply_self_update_policy(&mut command, &kind);
            assert_eq!(command, once, "{} policy was applied twice", kind.id);
        }
    }

    #[test]
    fn unrelated_live_and_historical_entries_both_appear() {
        let rows = merge_threads(
            vec![live(1, 100, None)],
            vec![historical("session-old", 10)],
        );

        assert_eq!(
            row_session_ids(&rows),
            vec![live_label(1), "historical:session-old".to_string()]
        );
    }

    #[test]
    fn exact_session_id_match_marks_the_historical_row_live() {
        let rows = merge_threads(
            vec![live(1, 100, Some("session-a"))],
            vec![historical("session-a", 50)],
        );

        assert_eq!(
            row_session_ids(&rows),
            vec![format!(
                "historical-live:session-a:{}",
                EntityId::from(1).as_u64()
            )]
        );
    }

    #[test]
    fn resumed_thread_without_loaded_history_remains_visible_as_live() {
        let rows = merge_threads(vec![live(1, 100, Some("session-a"))], Vec::new());

        assert_eq!(row_session_ids(&rows), vec![live_label(1)]);
    }

    #[test]
    fn fresh_live_thread_suppresses_same_kind_activity_at_or_after_its_launch() {
        let rows = merge_threads(
            vec![live(1, 100, None)],
            vec![
                historical("session-after", 150),
                historical("session-at-launch", 100),
                historical("session-before", 50),
            ],
        );

        // "at-launch" (>= launch time) and "after" are suppressed; only the
        // strictly-earlier historical entry survives alongside the live one.
        assert_eq!(
            row_session_ids(&rows),
            vec![live_label(1), "historical:session-before".to_string()]
        );
    }

    #[test]
    fn combined_resume_and_heuristic_suppression_for_concurrent_live_threads() {
        let rows = merge_threads(
            vec![live(1, 100, Some("session-resumed")), live(2, 120, None)],
            vec![
                historical("session-resumed", 20),
                historical("session-after-fresh-launch", 130),
                historical("session-before-fresh-launch", 110),
            ],
        );

        // "resumed" keeps its historical identity and gains live state;
        // "after-fresh-launch" (>= 120) is dropped by the heuristic;
        // "before-fresh-launch" (< 120) survives.
        assert_eq!(
            row_session_ids(&rows),
            vec![
                live_label(2),
                "historical:session-before-fresh-launch".to_string(),
                format!(
                    "historical-live:session-resumed:{}",
                    EntityId::from(1).as_u64()
                )
            ]
        );
    }

    #[test]
    fn rows_are_sorted_by_recency_descending() {
        // The live thread is resumed (not fresh), so the heuristic
        // suppression rule doesn't apply here -- this test is purely about
        // sort order, not dedup.
        let rows = merge_threads(
            vec![live(1, 5, Some("unrelated-session"))],
            vec![
                historical("session-newest", 30),
                historical("session-oldest", 1),
            ],
        );

        assert_eq!(
            row_session_ids(&rows),
            vec![
                "historical:session-newest".to_string(),
                live_label(1),
                "historical:session-oldest".to_string()
            ]
        );
    }

    #[test]
    fn discovery_resolves_a_single_post_launch_session() {
        let result = resolve_discovered_session(
            at(100),
            &[
                historical("session-before", 50),
                historical("session-after", 150),
            ],
            &HashSet::default(),
        );
        assert_eq!(
            result,
            DiscoveredSession::Resolved(SharedString::from("session-after"))
        );
    }

    #[test]
    fn discovery_is_ambiguous_with_multiple_post_launch_candidates() {
        let result = resolve_discovered_session(
            at(100),
            &[
                historical("session-a", 150),
                historical("session-b", 200),
                historical("session-before", 50),
            ],
            &HashSet::default(),
        );
        match result {
            DiscoveredSession::Ambiguous(mut ids) => {
                ids.sort();
                assert_eq!(
                    ids,
                    vec![
                        SharedString::from("session-a"),
                        SharedString::from("session-b")
                    ]
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn discovery_finds_nothing_before_launch_or_already_bound_elsewhere() {
        let not_found = resolve_discovered_session(
            at(100),
            &[historical("session-before", 50)],
            &HashSet::default(),
        );
        assert_eq!(not_found, DiscoveredSession::NotFound);

        let mut bound = HashSet::default();
        bound.insert(SharedString::from("session-after"));
        let excluded =
            resolve_discovered_session(at(100), &[historical("session-after", 150)], &bound);
        assert_eq!(excluded, DiscoveredSession::NotFound);
    }

    #[test]
    fn discovery_includes_a_session_exactly_at_launch_time() {
        // Matches merge_threads's own `>=` boundary for "at or after launch".
        let result = resolve_discovered_session(
            at(100),
            &[historical("session-at-launch", 100)],
            &HashSet::default(),
        );
        assert_eq!(
            result,
            DiscoveredSession::Resolved(SharedString::from("session-at-launch"))
        );
    }

    #[test]
    fn snapshot_records_include_resumed_threads_and_exclude_fresh_threads() {
        let records = snapshot_records_for_workspace(
            workspace::WorkspaceId::from_i64(7),
            vec![
                live_with_kind(1, "codex", Some("session-a")),
                live_with_kind(2, "claude", None),
            ],
        );

        assert_eq!(
            records,
            vec![AgentThreadSessionRestoreRecord {
                workspace_id: workspace::WorkspaceId::from_i64(7),
                kind_id: "codex".to_string(),
                session_id: "session-a".to_string(),
                title: "live".to_string(),
                project_root: PathBuf::from("/root"),
                tied_worktree_root: Some(PathBuf::from("/root")),
                last_activity_at: 100_000,
            }]
        );
    }

    fn restore_record(
        workspace_id: i64,
        tied_worktree_root: Option<&str>,
    ) -> AgentThreadSessionRestoreRecord {
        AgentThreadSessionRestoreRecord {
            workspace_id: workspace::WorkspaceId::from_i64(workspace_id),
            kind_id: "codex".to_string(),
            session_id: "session-a".to_string(),
            title: "Restored".to_string(),
            project_root: PathBuf::from("/root"),
            tied_worktree_root: tied_worktree_root.map(PathBuf::from),
            last_activity_at: 100_000,
        }
    }

    #[test]
    fn restore_records_skip_live_resumed_sessions() {
        let records = records_to_restore_for_workspace(
            workspace::WorkspaceId::from_i64(7),
            &[PathBuf::from("/root")],
            &[restore_record(7, Some("/root"))],
            &[live_with_kind(1, "codex", Some("session-a"))],
        );

        assert!(records.is_empty());
    }

    #[test]
    fn restore_records_route_new_records_by_effective_tie_not_workspace_id() {
        // The record's original workspace_id (7) never reopens; only
        // workspace 9, whose own root matches the tie, does. A tie-based
        // record must still restore there.
        let records = records_to_restore_for_workspace(
            workspace::WorkspaceId::from_i64(9),
            &[PathBuf::from("/root-b")],
            &[restore_record(7, Some("/root-b"))],
            &[],
        );

        assert_eq!(records, vec![restore_record(7, Some("/root-b"))]);
    }

    #[test]
    fn restore_records_do_not_route_new_records_to_a_workspace_with_a_different_root() {
        let records = records_to_restore_for_workspace(
            workspace::WorkspaceId::from_i64(9),
            &[PathBuf::from("/unrelated")],
            &[restore_record(7, Some("/root-b"))],
            &[],
        );

        assert!(records.is_empty());
    }

    #[test]
    fn restore_records_route_legacy_records_by_workspace_id_only() {
        // No tied_worktree_root at all (a record written before this field
        // existed) -- must not be compared against project_root by path,
        // only workspace_id routing applies.
        let legacy = restore_record(7, None);

        let matches_original_workspace = records_to_restore_for_workspace(
            workspace::WorkspaceId::from_i64(7),
            &[PathBuf::from("/somewhere-else")],
            std::slice::from_ref(&legacy),
            &[],
        );
        assert_eq!(matches_original_workspace, vec![legacy.clone()]);

        let different_workspace_with_matching_root = records_to_restore_for_workspace(
            workspace::WorkspaceId::from_i64(9),
            &[PathBuf::from("/root")],
            &[legacy],
            &[],
        );
        assert!(
            different_workspace_with_matching_root.is_empty(),
            "a legacy record must not restore into a different workspace even if its root matches project_root"
        );
    }
}
