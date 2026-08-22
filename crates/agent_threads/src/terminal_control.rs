use std::collections::HashMap;
use std::path::PathBuf;

use agent_control_protocol::{TerminalControlId, TerminalMetadata};
use anyhow::{Result, anyhow};
use gpui::{App, AppContext as _, Entity, EntityId, Global, Subscription, WeakEntity, Window};
use terminal::Terminal;
use terminal_view::TerminalView;
use workspace::Workspace;

#[derive(Clone)]
pub(crate) struct TerminalControlRecord {
    pub id: TerminalControlId,
    pub creation_sequence: u64,
    pub generation: u64,
    pub root_process_id: u32,
    pub working_directory: Option<PathBuf>,
    pub terminal: WeakEntity<Terminal>,
    pub view: WeakEntity<TerminalView>,
    pub workspace: WeakEntity<Workspace>,
}

#[derive(Default)]
pub(crate) struct TerminalControlRegistry {
    next_sequence: u64,
    records: HashMap<TerminalControlId, TerminalControlRecord>,
    subscriptions: HashMap<TerminalControlId, Subscription>,
}

impl TerminalControlRegistry {
    fn allocate_id(&mut self) -> TerminalControlId {
        self.next_sequence += 1;
        TerminalControlId(format!(
            "terminal-{}-{}",
            self.next_sequence,
            uuid::Uuid::new_v4()
        ))
    }

    fn register(
        &mut self,
        root_process_id: u32,
        working_directory: Option<PathBuf>,
        terminal: WeakEntity<Terminal>,
        view: WeakEntity<TerminalView>,
        workspace: WeakEntity<Workspace>,
    ) -> TerminalControlId {
        let id = self.allocate_id();
        let sequence = self.next_sequence;
        self.records.insert(
            id.clone(),
            TerminalControlRecord {
                id: id.clone(),
                creation_sequence: sequence,
                generation: sequence,
                root_process_id,
                working_directory,
                terminal,
                view,
                workspace,
            },
        );
        id
    }

    fn live_records(&mut self, cx: &App) -> Vec<TerminalControlRecord> {
        self.records.retain(|_, record| {
            record.terminal.upgrade().is_some() && record.view.upgrade().is_some()
        });
        self.subscriptions
            .retain(|id, _| self.records.contains_key(id));
        for record in self.records.values_mut() {
            record.working_directory = record
                .terminal
                .upgrade()
                .and_then(|terminal| terminal.read(cx).working_directory());
        }
        let mut records = self.records.values().cloned().collect::<Vec<_>>();
        records.sort_by_key(|record| record.creation_sequence);
        records
    }
}

struct GlobalTerminalControlRegistry(Entity<TerminalControlRegistry>);

impl Global for GlobalTerminalControlRegistry {}

pub(crate) fn init(cx: &mut App) {
    if cx.has_global::<GlobalTerminalControlRegistry>() {
        return;
    }
    let registry = cx.new(|_| TerminalControlRegistry::default());
    cx.set_global(GlobalTerminalControlRegistry(registry.clone()));
    cx.observe_new(move |view: &mut TerminalView, _window, cx| {
        let terminal = view.terminal().clone();
        let terminal_state = terminal.read(cx);
        if terminal_state.is_remote() {
            return;
        }
        let Some(root_process_id) = terminal_state.pid().map(|pid| pid.as_u32()) else {
            return;
        };
        let working_directory = terminal_state.working_directory();
        let terminal = terminal.downgrade();
        let workspace = view.workspace_handle();
        let view = cx.entity().downgrade();
        registry.update(cx, |registry, cx| {
            let id = registry.register(
                root_process_id,
                working_directory,
                terminal.clone(),
                view,
                workspace,
            );
            if let Some(terminal) = terminal.upgrade() {
                let subscription = cx.subscribe(&terminal, |_registry, _, event, cx| {
                    if matches!(
                        event,
                        terminal::Event::Wakeup
                            | terminal::Event::Bell
                            | terminal::Event::CloseTerminal
                    ) {
                        cx.notify();
                    }
                });
                registry.subscriptions.insert(id, subscription);
            }
            cx.notify();
        });
    })
    .detach();
}

pub(crate) fn registry(cx: &App) -> Entity<TerminalControlRegistry> {
    cx.global::<GlobalTerminalControlRegistry>().0.clone()
}

#[derive(Clone, Copy)]
pub(crate) struct RegularTerminalSummary {
    pub status: crate::store::ProjectAttentionStatus,
    pub terminal_item_id: EntityId,
    pub creation_sequence: u64,
}

pub(crate) fn regular_terminal_summaries(
    cx: &mut App,
) -> HashMap<EntityId, Vec<RegularTerminalSummary>> {
    if !cx.has_global::<GlobalTerminalControlRegistry>() {
        return HashMap::default();
    }
    let mut summaries: HashMap<EntityId, Vec<RegularTerminalSummary>> = HashMap::default();
    for record in records(cx) {
        let Some(view) = record.view.upgrade() else {
            continue;
        };
        if view.read(cx).is_agent_thread() {
            continue;
        }
        let Some(workspace_id) = workspace_id(&record, cx) else {
            continue;
        };
        let Some(terminal) = record.terminal.upgrade() else {
            continue;
        };
        let terminal = terminal.read(cx);
        let screen_tail = terminal
            .last_n_non_empty_lines(crate::attention_detection::SCREEN_TAIL_LINE_COUNT)
            .join("\n");
        let state =
            crate::attention_detection::classify_any(crate::attention_detection::DetectionInput {
                screen_tail: &screen_tail,
                osc_title: &terminal.breadcrumb_text,
            });
        let status = match state {
            crate::attention_detection::AttentionState::Working => {
                crate::store::ProjectAttentionStatus::Working
            }
            crate::attention_detection::AttentionState::Idle => {
                crate::store::ProjectAttentionStatus::Idle
            }
            crate::attention_detection::AttentionState::Blocked => {
                crate::store::ProjectAttentionStatus::Blocked
            }
            crate::attention_detection::AttentionState::Unknown => continue,
        };
        summaries
            .entry(workspace_id)
            .or_default()
            .push(RegularTerminalSummary {
                status,
                terminal_item_id: view.entity_id(),
                creation_sequence: record.creation_sequence,
            });
    }
    summaries
}

pub(crate) fn focus_terminal(
    terminal_item_id: EntityId,
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    let record = records(cx)
        .into_iter()
        .find(|record| {
            record
                .view
                .upgrade()
                .is_some_and(|view| view.entity_id() == terminal_item_id)
        })
        .ok_or_else(|| anyhow!("terminal no longer exists"))?;
    let workspace = record
        .view
        .upgrade()
        .and_then(|view| view.read(cx).workspace_handle().upgrade())
        .or_else(|| record.workspace.upgrade())
        .ok_or_else(|| anyhow!("terminal workspace closed"))?;
    let terminal_view = record
        .view
        .upgrade()
        .ok_or_else(|| anyhow!("terminal closed"))?;
    workspace.update(cx, |workspace, cx| {
        let pane = workspace
            .pane_for_item_id(terminal_item_id)
            .ok_or_else(|| anyhow!("terminal pane closed"))?;
        pane.update(cx, |pane, cx| {
            let index = pane
                .index_for_item(&terminal_view)
                .ok_or_else(|| anyhow!("terminal item closed"))?;
            pane.activate_item(index, true, true, window, cx);
            Ok(())
        })
    })
}

pub(crate) fn records(cx: &mut App) -> Vec<TerminalControlRecord> {
    let registry = cx.global::<GlobalTerminalControlRegistry>().0.clone();
    registry.update(cx, |registry, cx| registry.live_records(cx))
}

pub(crate) fn observe_output(
    record: &TerminalControlRecord,
    cx: &mut App,
) -> Option<(async_channel::Receiver<()>, Subscription)> {
    let terminal = record.terminal.upgrade()?;
    let registry = cx.global::<GlobalTerminalControlRegistry>().0.clone();
    let (sender, receiver) = async_channel::bounded(1);
    let subscription = registry.update(cx, |_registry, cx| {
        cx.subscribe(&terminal, move |_registry, _terminal, event, _cx| {
            if matches!(
                event,
                terminal::Event::Wakeup | terminal::Event::CloseTerminal
            ) {
                sender.try_send(()).ok();
            }
        })
    });
    Some((receiver, subscription))
}

pub(crate) fn metadata(record: &TerminalControlRecord, cx: &App) -> Option<TerminalMetadata> {
    let terminal = record.terminal.upgrade()?;
    let view = record.view.upgrade()?;
    Some(TerminalMetadata {
        id: record.id.clone(),
        title: terminal.read(cx).title(false),
        working_directory: record.working_directory.clone(),
        is_agent_thread: view.read(cx).is_agent_thread(),
        has_exited: terminal.read(cx).has_exited(),
    })
}

pub(crate) fn workspace_id(record: &TerminalControlRecord, cx: &App) -> Option<EntityId> {
    let current = record.view.upgrade()?.read(cx).workspace_handle().upgrade();
    current
        .or_else(|| record.workspace.upgrade())
        .map(|workspace| workspace.entity_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_ordered_and_never_reused() {
        let mut registry = TerminalControlRegistry::default();
        let first = registry.allocate_id();
        let second = registry.allocate_id();
        assert_ne!(first, second);
        assert_eq!(registry.next_sequence, 2);
    }
}
