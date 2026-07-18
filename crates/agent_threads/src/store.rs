use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Result, anyhow};
use collections::{HashMap, HashSet};
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, SharedString,
    Subscription, Task, TaskExt, WeakEntity, Window,
};
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use task::{RevealStrategy, RevealTarget, Shell, SpawnInTerminal};
use terminal_view::{TerminalView, terminal_panel::TerminalPanel};
use util::ResultExt as _;
use workspace::{Workspace, WorkspaceId};

use crate::{
    AgentKindDefinition, AgentLaunchCommand, AgentThreadSettings, HistoricalThread,
    agent_kind_registry,
    managed_agent::{CachedAgentArtifactSource, ManagedAgentProvisioner, RemoteClientAgentHost},
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
}

struct ThreadEntry {
    metadata: AgentThreadMetadata,
    workspace: WeakEntity<Workspace>,
    terminal_view: WeakEntity<TerminalView>,
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
        });
        cx.set_global(GlobalAgentThreadStore(store));
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAgentThreadStore>().0.clone()
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
        cx: &mut Context<Self>,
    ) {
        let terminal_item_id = terminal_view.entity_id();
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
            },
        );

        let release_subscription =
            cx.observe_release(&terminal_view, move |store, _terminal_view, cx| {
                store.remove_thread(terminal_item_id, cx);
            });
        // Claude Code and Codex CLI both ring the terminal bell when a turn
        // finishes or a permission prompt needs an answer, so it's the
        // signal we have for "this agent thread needs attention" -- the
        // underlying `Terminal` (not `TerminalView`, which only re-emits
        // `Wakeup`/`UpdateTab` for a bell) is what actually re-emits it.
        let terminal = terminal_view.read(cx).terminal().clone();
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

    fn remove_thread(&mut self, terminal_item_id: EntityId, cx: &mut Context<Self>) {
        let Some(entry) = self.threads.remove(&terminal_item_id) else {
            return;
        };
        self.subscriptions.remove(&terminal_item_id);
        cx.emit(AgentThreadStoreEvent::ThreadClosed {
            kind_id: entry.metadata.kind_id,
        });
    }
}

pub fn launch_new_thread(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    extra_args: &[String],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let settings = AgentThreadSettings::get_global(cx);
    let mut command = settings.command_for_kind(kind.id).clone();
    command.args.extend(extra_args.iter().cloned());
    // Assign the session id ourselves when the CLI supports it, so the
    // thread is resumable and restorable from birth; otherwise the CLI
    // generates an id internally that Flint never learns.
    let session_id = kind.session_id_flag.map(|flag| {
        let session_id = uuid::Uuid::new_v4().to_string();
        command.args.push(flag.to_string());
        command.args.push(session_id.clone());
        SharedString::from(session_id)
    });
    spawn_thread(
        workspace,
        kind,
        kind.label.clone(),
        command,
        session_id,
        window,
        cx,
    );
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
    task.detach_and_log_err(cx);
}

fn resume_thread_task(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    thread: &HistoricalThread,
    extra_args: &[String],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<Task<Result<()>>> {
    let provider = kind.history_provider.as_ref()?;
    let settings = AgentThreadSettings::get_global(cx);
    let base = settings.command_for_kind(kind.id).clone();
    let command = provider.resume_command(&base, thread, extra_args);
    Some(spawn_thread_task(
        workspace,
        kind,
        thread.title.clone(),
        command,
        Some(thread.session_id.clone()),
        window,
        cx,
    ))
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

pub fn launch_managed_thread(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    extra_args: &[String],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();
    let Some(remote_client) = project.read(cx).remote_client() else {
        workspace.show_error(
            &anyhow!("managed agents are available only for remote projects"),
            cx,
        );
        return;
    };
    let Some(platform) = remote_client.read(cx).platform() else {
        workspace.show_error(&anyhow!("remote agent target could not be detected"), cx);
        return;
    };
    let Some(release) = kind.release_for(platform).copied() else {
        workspace.show_error(
            &anyhow!(
                "no pinned {} release supports this remote target",
                kind.label
            ),
            cx,
        );
        return;
    };
    let remote_host = match RemoteClientAgentHost::new(remote_client.read(cx)) {
        Ok(remote_host) => remote_host,
        Err(error) => {
            workspace.show_error(&error, cx);
            return;
        }
    };
    let artifacts = CachedAgentArtifactSource::new(
        project.read(cx).http_client(),
        kind.official_source_prefixes(),
    );
    let provisioner = ManagedAgentProvisioner::new(artifacts, remote_host);
    let kind = kind.clone();
    let extra_args = extra_args.to_vec();

    cx.spawn_in(window, async move |workspace, cx| {
        match provisioner.install(kind.id, &release).await {
            Ok(installation) => {
                workspace.update_in(cx, |workspace, window, cx| {
                    let mut command = AgentThreadSettings::get_global(cx)
                        .command_for_kind(kind.id)
                        .clone();
                    command.command =
                        Some(installation.executable_path.to_string_lossy().into_owned());
                    command.args.extend(
                        kind.self_update_policy()
                            .arguments
                            .iter()
                            .map(|argument| argument.to_string()),
                    );
                    command.args.extend(extra_args);
                    for (name, value) in kind.self_update_policy().environment {
                        command
                            .env
                            .insert((*name).to_string(), (*value).to_string());
                    }
                    spawn_thread(
                        workspace,
                        &kind,
                        SharedString::from(format!("New {} thread", kind.label)),
                        command,
                        None,
                        window,
                        cx,
                    );
                })?;
            }
            Err(error) => {
                workspace.update(cx, |workspace, cx| workspace.show_error(&error, cx))?;
            }
        }
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
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
    let mut tasks = Vec::new();

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
        if let Some(task) = resume_thread_task(workspace, &kind, &thread, &extra_args, window, cx) {
            tasks.push((kind.id, thread.session_id.to_string(), task));
        }
    }

    cx.spawn_in(window, async move |_workspace, _cx| {
        let mut failure_count = 0;
        for (kind_id, session_id, task) in tasks {
            if let Err(error) = task.await {
                log::error!("Failed to reopen {kind_id} agent session {session_id}: {error:#}");
                failure_count += 1;
            }
        }
        failure_count
    })
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
    use std::time::Duration;

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
