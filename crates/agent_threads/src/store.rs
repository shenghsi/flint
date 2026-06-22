use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Result, anyhow};
use collections::HashMap;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, SharedString,
    Subscription, TaskExt, WeakEntity, Window,
};
use settings::Settings as _;
use task::{RevealStrategy, RevealTarget, Shell, SpawnInTerminal};
use terminal_view::{TerminalView, terminal_panel::TerminalPanel};
use util::ResultExt as _;
use workspace::Workspace;

use crate::{
    AgentKindDefinition, AgentLaunchCommand, AgentThreadSettings, HistoricalThread,
    resolve_default_launch_args,
};

#[derive(Clone)]
pub struct AgentThreadMetadata {
    pub terminal_item_id: EntityId,
    pub kind_id: &'static str,
    pub title: SharedString,
    pub project_root: PathBuf,
    pub launched_at: SystemTime,
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

pub struct AgentThreadStore {
    threads: HashMap<EntityId, ThreadEntry>,
    subscriptions: HashMap<EntityId, Vec<Subscription>>,
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
            title,
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
        self.subscriptions
            .insert(terminal_item_id, vec![release_subscription]);
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
    spawn_thread(
        workspace,
        kind,
        kind.label.clone(),
        command,
        None,
        window,
        cx,
    );
}

/// Namespace for the per-thread "remembered launch option" key-value store
/// (`db::kvp`). The value is the chosen `ResumeOption` label, or an empty
/// string for an explicit "plain resume, no extra args" choice. Absence of
/// a key means no per-thread choice has been made yet.
const LAUNCH_OPTION_NAMESPACE: &str = "agent-thread-launch-option";

/// Reads the launch option label the user last picked for this specific
/// thread (via its "..." menu), if any.
pub fn remembered_launch_option(cx: &App, session_id: &str) -> Option<String> {
    db::kvp::KeyValueStore::global(cx)
        .scoped(LAUNCH_OPTION_NAMESPACE)
        .read(session_id)
        .log_err()
        .flatten()
}

/// Persists `label` as this thread's remembered launch option choice.
/// `None` represents an explicit "plain resume" choice (stored as an empty
/// string, distinct from no choice having been made at all).
pub fn remember_launch_option(cx: &App, session_id: SharedString, label: Option<String>) {
    let store = db::kvp::KeyValueStore::global(cx);
    db::write_and_log(cx, move || async move {
        store
            .scoped(LAUNCH_OPTION_NAMESPACE)
            .write(session_id.to_string(), label.unwrap_or_default())
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
        Some(label) if label.is_empty() => Vec::new(),
        Some(label) => kind
            .resume_options
            .iter()
            .find(|option| option.label.as_ref() == label)
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
    let Some(provider) = kind.history_provider.as_ref() else {
        return;
    };
    let settings = AgentThreadSettings::get_global(cx);
    let base = settings.command_for_kind(kind.id).clone();
    let command = provider.resume_command(&base, thread, extra_args);
    spawn_thread(
        workspace,
        kind,
        thread.title.clone(),
        command,
        Some(thread.session_id.clone()),
        window,
        cx,
    );
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
    let Some(cwd) = command
        .cwd
        .clone()
        .or_else(|| terminal_view::default_working_directory(workspace, cx))
    else {
        return;
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
    .detach_and_log_err(cx);
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
}
