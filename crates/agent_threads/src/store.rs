use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Result, anyhow};
use collections::{HashMap, HashSet};
use futures::StreamExt as _;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, PromptLevel,
    SharedString, Subscription, Task, TaskExt, WeakEntity, Window, WindowHandle,
};
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

pub enum AgentThreadStoreEvent {
    ThreadOpened { kind_id: &'static str },
    ThreadClosed { kind_id: &'static str },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentThreadSessionRestoreRecord {
    pub workspace_id: WorkspaceId,
    pub kind_id: String,
    pub session_id: String,
    pub title: String,
    pub project_root: PathBuf,
    pub last_activity_at: u64,
}

const SESSION_RESTORE_NAMESPACE: &str = "agent-thread-session-restore";

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
        let store = cx.new(|_| Self {
            threads: HashMap::default(),
            subscriptions: HashMap::default(),
            restore_attempted: HashSet::default(),
            egress_manager: std::sync::Arc::new(AgentEgressManager::new()),
            route_changes: HashSet::default(),
            agent_artifact_cache: None,
            managed_provisioning: ManagedAgentProvisioningCoordinator::default(),
        });
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

    pub fn live_threads_for_project(
        &self,
        kind_id: &str,
        project_roots: &[PathBuf],
    ) -> Vec<AgentThreadMetadata> {
        self.threads
            .values()
            .map(|entry| &entry.metadata)
            .filter(|metadata| {
                metadata.kind_id == kind_id
                    && project_roots
                        .iter()
                        .any(|root| root == &metadata.project_root)
            })
            .cloned()
            .collect()
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

    fn register(
        &mut self,
        kind_id: &'static str,
        title: SharedString,
        project_root: PathBuf,
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
        let metadata = AgentThreadMetadata {
            terminal_item_id,
            kind_id,
            title: title.clone(),
            project_root,
            launched_at,
            resumed_session_id,
        };
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
            cx.show_desktop_notification(&title, Some(&format!("{kind_label} is waiting for you")));
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
    let kind_id = kind.id;
    let kind_icon = kind.icon;
    let title = summary.clone();
    let label = summary.to_string();
    let command_label = command_label(&command, &label);
    let is_windows = workspace.project().read(cx).path_style(cx).is_windows();
    let remote_process = match prepare_remote_thread_process(
        &mut command,
        remote_connection,
        is_windows,
        uuid::Uuid::new_v4(),
    ) {
        Ok(remote_process) => remote_process,
        Err(error) => return Task::ready(Err(error)),
    };
    let task = SpawnInTerminal {
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
        });
        let store = cx.update(|_, cx| AgentThreadStore::global(cx))?;
        store.update(cx, |store, cx| {
            store.register(
                kind_id,
                title,
                cwd,
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
    let key_value_store = db::kvp::KeyValueStore::global(cx);
    cx.background_spawn(async move {
        let records_json = serde_json::to_string(&records)?;
        key_value_store
            .scoped(SESSION_RESTORE_NAMESPACE)
            .write(session_id, records_json)
            .await
    })
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
    let records = records_to_restore_for_workspace(workspace_id, &records, &live_threads);
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
                last_activity_at: system_time_to_millis(thread.launched_at),
            })
        })
        .collect()
}

fn records_to_restore_for_workspace(
    workspace_id: WorkspaceId,
    records: &[AgentThreadSessionRestoreRecord],
    live_threads: &[AgentThreadMetadata],
) -> Vec<AgentThreadSessionRestoreRecord> {
    records
        .iter()
        .filter(|record| record.workspace_id == workspace_id)
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
    use pretty_assertions::assert_eq;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn live(id: u64, launched_at: u64, resumed_session_id: Option<&str>) -> AgentThreadMetadata {
        AgentThreadMetadata {
            terminal_item_id: EntityId::from(id),
            kind_id: "codex",
            title: SharedString::from("live"),
            project_root: PathBuf::from("/root"),
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
            let mut expected_prefix = vec!["base".to_string()];
            expected_prefix.extend(option_arguments);
            assert!(launch.command.args.starts_with(&expected_prefix));
            let mut expected = launch.command.clone();
            apply_self_update_policy(&mut expected, &kind);
            assert_eq!(launch.command, expected);
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
            ..AgentLaunchCommand::default()
        };

        let command = build_credential_command(&base, &["logout"]);

        assert_eq!(command.command.as_deref(), Some("custom-codex"));
        assert_eq!(command.args, vec!["logout".to_string()]);
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
                last_activity_at: 100_000,
            }]
        );
    }

    #[test]
    fn restore_records_skip_live_resumed_sessions() {
        let records = records_to_restore_for_workspace(
            workspace::WorkspaceId::from_i64(7),
            &[AgentThreadSessionRestoreRecord {
                workspace_id: workspace::WorkspaceId::from_i64(7),
                kind_id: "codex".to_string(),
                session_id: "session-a".to_string(),
                title: "Restored".to_string(),
                project_root: PathBuf::from("/root"),
                last_activity_at: 100_000,
            }],
            &[live_with_kind(1, "codex", Some("session-a"))],
        );

        assert!(records.is_empty());
    }
}
