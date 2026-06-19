use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Result, anyhow};
use collections::{HashMap, HashSet};
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, SharedString,
    Subscription, TaskExt, WeakEntity, Window,
};
use settings::Settings as _;
use task::{RevealStrategy, RevealTarget, Shell, SpawnInTerminal};
use terminal::Event as TerminalEvent;
use terminal_view::{TerminalView, terminal_panel::TerminalPanel};
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
};

use crate::{AgentKindDefinition, AgentLaunchCommand, AgentThreadSettings, HistoricalThread};

#[derive(Clone)]
pub struct AgentThreadMetadata {
    pub terminal_item_id: EntityId,
    pub kind_id: &'static str,
    pub title: SharedString,
    pub project_root: PathBuf,
    pub has_attention: bool,
    pub last_activity_at: SystemTime,
    pub launched_at: SystemTime,
    pub resumed_session_id: Option<SharedString>,
}

#[derive(Clone)]
pub enum AgentThreadRow {
    Live(AgentThreadMetadata),
    Historical(HistoricalThread),
}

impl AgentThreadRow {
    pub fn last_activity_at(&self) -> SystemTime {
        match self {
            AgentThreadRow::Live(metadata) => metadata.last_activity_at,
            AgentThreadRow::Historical(thread) => thread.last_activity_at,
        }
    }
}

/// Merges a kind's live and historical threads for one project into a
/// single, deduplicated, recency-sorted list. Two suppression rules keep a
/// thread from appearing twice:
/// - a resumed thread is dropped from `historical` by exact session id
/// - a brand-new (not-yet-resumed) live thread suppresses historical
///   entries for the same kind/project activity at or after its launch,
///   since the CLI hasn't necessarily written its session id anywhere yet
pub fn merge_threads(
    live: Vec<AgentThreadMetadata>,
    historical: Vec<HistoricalThread>,
) -> Vec<AgentThreadRow> {
    let resumed_ids: HashSet<SharedString> = live
        .iter()
        .filter_map(|metadata| metadata.resumed_session_id.clone())
        .collect();
    let earliest_fresh_launch = live
        .iter()
        .filter(|metadata| metadata.resumed_session_id.is_none())
        .map(|metadata| metadata.launched_at)
        .min();

    let mut rows = Vec::new();
    for thread in historical {
        if resumed_ids.contains(&thread.session_id) {
            continue;
        }
        if let Some(launch_time) = earliest_fresh_launch {
            if thread.last_activity_at >= launch_time {
                continue;
            }
        }
        rows.push(AgentThreadRow::Historical(thread));
    }
    for metadata in live {
        rows.push(AgentThreadRow::Live(metadata));
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.last_activity_at()));
    rows
}

pub enum AgentThreadStoreEvent {
    Updated,
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
                    && project_roots.iter().any(|root| root == &metadata.project_root)
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

        self.update_attention(terminal_item_id, false, cx);
        Ok(())
    }

    fn register(
        &mut self,
        kind_id: &'static str,
        project_root: PathBuf,
        resumed_session_id: Option<SharedString>,
        launched_at: SystemTime,
        workspace: Entity<Workspace>,
        terminal_view: Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let terminal_item_id = terminal_view.entity_id();
        let title = terminal_view.read(cx).tab_content_text(0, cx);
        let metadata = AgentThreadMetadata {
            terminal_item_id,
            kind_id,
            title,
            project_root,
            has_attention: terminal_view.read(cx).has_bell(),
            last_activity_at: SystemTime::now(),
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

        let item_subscription = cx.subscribe(&terminal_view, {
            move |store, terminal_view, event: &ItemEvent, cx| {
                if matches!(event, ItemEvent::UpdateTab) {
                    store.refresh_thread(terminal_view.entity_id(), terminal_view, cx);
                }
            }
        });
        let terminal_subscription = cx.subscribe(&terminal_view, {
            move |store, terminal_view, event: &TerminalEvent, cx| match event {
                TerminalEvent::Bell | TerminalEvent::TitleChanged | TerminalEvent::Wakeup => {
                    store.refresh_thread(terminal_view.entity_id(), terminal_view, cx);
                }
                _ => {}
            }
        });
        // Without this, a closed terminal's entry would stay in `self.threads`
        // forever (nothing else removes it), showing as a permanently "live"
        // row in the always-docked panel.
        let release_subscription =
            cx.observe_release(&terminal_view, move |store, _terminal_view, cx| {
                store.remove_thread(terminal_item_id, cx);
            });
        self.subscriptions.insert(
            terminal_item_id,
            vec![item_subscription, terminal_subscription, release_subscription],
        );
        cx.emit(AgentThreadStoreEvent::Updated);
        cx.notify();
    }

    fn refresh_thread(
        &mut self,
        terminal_item_id: EntityId,
        terminal_view: Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.threads.get_mut(&terminal_item_id) else {
            return;
        };
        entry.metadata.title = terminal_view.read(cx).tab_content_text(0, cx);
        entry.metadata.has_attention = terminal_view.read(cx).has_bell();
        entry.metadata.last_activity_at = SystemTime::now();
        cx.emit(AgentThreadStoreEvent::Updated);
        cx.notify();
    }

    fn update_attention(&mut self, terminal_item_id: EntityId, has_attention: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.threads.get_mut(&terminal_item_id) else {
            return;
        };
        entry.metadata.has_attention = has_attention;
        cx.emit(AgentThreadStoreEvent::Updated);
        cx.notify();
    }

    fn remove_thread(&mut self, terminal_item_id: EntityId, cx: &mut Context<Self>) {
        self.threads.remove(&terminal_item_id);
        self.subscriptions.remove(&terminal_item_id);
        cx.emit(AgentThreadStoreEvent::Updated);
        cx.notify();
    }
}

pub fn launch_new_thread(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let settings = AgentThreadSettings::get_global(cx);
    let command = settings.command_for_kind(kind.id).clone();
    spawn_thread(workspace, kind.id, kind.label.clone(), command, None, window, cx);
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
        kind.id,
        kind.label.clone(),
        command,
        Some(thread.session_id.clone()),
        window,
        cx,
    );
}

fn spawn_thread(
    workspace: &mut Workspace,
    kind_id: &'static str,
    kind_label: SharedString,
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
    let label = kind_label.to_string();
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
        let store = cx.update(|_, cx| AgentThreadStore::global(cx))?;
        store.update(cx, |store, cx| {
            store.register(
                kind_id,
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
            has_attention: false,
            last_activity_at: at(launched_at),
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
                AgentThreadRow::Live(metadata) => format!("live:{}", metadata.terminal_item_id.as_u64()),
                AgentThreadRow::Historical(thread) => format!("historical:{}", thread.session_id),
            })
            .collect()
    }

    #[test]
    fn unrelated_live_and_historical_entries_both_appear() {
        let rows = merge_threads(vec![live(1, 100, None)], vec![historical("session-old", 10)]);

        assert_eq!(
            row_session_ids(&rows),
            vec![live_label(1), "historical:session-old".to_string()]
        );
    }

    #[test]
    fn exact_session_id_match_suppresses_the_historical_duplicate() {
        let rows = merge_threads(
            vec![live(1, 100, Some("session-a"))],
            vec![historical("session-a", 50)],
        );

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

        // "resumed" is dropped by exact id; "after-fresh-launch" (>= 120) is
        // dropped by the heuristic; "before-fresh-launch" (< 120) survives.
        assert_eq!(
            row_session_ids(&rows),
            vec![
                live_label(2),
                "historical:session-before-fresh-launch".to_string(),
                live_label(1)
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
            vec![historical("session-newest", 30), historical("session-oldest", 1)],
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
