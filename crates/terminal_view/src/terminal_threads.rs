use std::{path::PathBuf, time::SystemTime};

use anyhow::{Result, anyhow};
use collections::HashMap;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, Global,
    Render, SharedString, StatefulInteractiveElement as _, Subscription, TaskExt, WeakEntity,
    Window, actions,
};
use settings::{RegisterSetting, Settings, TerminalThreadCommandContent};
use task::{RevealStrategy, RevealTarget, Shell, SpawnInTerminal};
use terminal::Event as TerminalEvent;
use ui::prelude::*;
use util::ResultExt as _;
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
};

use crate::{TerminalView, default_working_directory, terminal_panel::TerminalPanel};

actions!(
    terminal_thread,
    [
        /// Opens the terminal thread organizer.
        OpenTerminalThreads,
        /// Starts a Codex terminal thread.
        NewCodexThread,
        /// Starts a Claude terminal thread.
        NewClaudeThread,
        /// Starts a shell terminal thread.
        NewShellThread
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalThreadKind {
    Codex,
    Claude,
    Shell,
}

impl TerminalThreadKind {
    fn label(self) -> &'static str {
        match self {
            TerminalThreadKind::Codex => "Codex",
            TerminalThreadKind::Claude => "Claude",
            TerminalThreadKind::Shell => "Shell",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalThreadCommand {
    command: Option<String>,
    args: Vec<String>,
    env: HashMap<String, String>,
    cwd: Option<PathBuf>,
}

impl TerminalThreadCommand {
    fn from_content(
        content: Option<TerminalThreadCommandContent>,
        default_command: Option<&'static str>,
    ) -> Self {
        let content = content.unwrap_or_default();
        Self {
            command: content
                .command
                .or_else(|| default_command.map(ToOwned::to_owned)),
            args: content.args.unwrap_or_default(),
            env: content.env.unwrap_or_default().into_iter().collect(),
            cwd: content.cwd,
        }
    }

    fn command_label(&self, fallback: &str) -> String {
        let Some(command) = self.command.as_ref() else {
            return fallback.to_string();
        };

        std::iter::once(command.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Clone, Debug, RegisterSetting)]
pub struct TerminalThreadSettings {
    pub codex: TerminalThreadCommand,
    pub claude: TerminalThreadCommand,
    pub shell: TerminalThreadCommand,
}

impl Settings for TerminalThreadSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.terminal_threads.clone().unwrap_or_default();
        Self {
            codex: TerminalThreadCommand::from_content(content.codex, Some("codex")),
            claude: TerminalThreadCommand::from_content(content.claude, Some("claude")),
            shell: TerminalThreadCommand::from_content(content.shell, None),
        }
    }
}

#[derive(Clone)]
pub struct TerminalThreadMetadata {
    pub terminal_item_id: EntityId,
    pub kind: TerminalThreadKind,
    pub title: SharedString,
    pub project_name: SharedString,
    pub has_attention: bool,
    pub last_activity_at: SystemTime,
}

pub enum TerminalThreadStoreEvent {
    Updated,
}

pub struct TerminalThreadStore {
    threads: HashMap<EntityId, TerminalThreadEntry>,
    subscriptions: HashMap<EntityId, Vec<Subscription>>,
}

struct TerminalThreadEntry {
    metadata: TerminalThreadMetadata,
    workspace: WeakEntity<Workspace>,
    terminal_view: WeakEntity<TerminalView>,
}

struct GlobalTerminalThreadStore(Entity<TerminalThreadStore>);
impl Global for GlobalTerminalThreadStore {}

impl EventEmitter<TerminalThreadStoreEvent> for TerminalThreadStore {}

impl TerminalThreadStore {
    pub fn init_global(cx: &mut App) {
        if cx.has_global::<GlobalTerminalThreadStore>() {
            return;
        }

        let store = cx.new(|_| Self {
            threads: HashMap::default(),
            subscriptions: HashMap::default(),
        });
        cx.set_global(GlobalTerminalThreadStore(store));
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalTerminalThreadStore>().0.clone()
    }

    pub fn entries(&self) -> impl Iterator<Item = &TerminalThreadMetadata> {
        self.threads.values().map(|entry| &entry.metadata)
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
            .ok_or_else(|| anyhow!("terminal thread no longer exists"))?;
        let workspace = entry
            .workspace
            .upgrade()
            .ok_or_else(|| anyhow!("terminal thread workspace closed"))?;
        let terminal_view = entry
            .terminal_view
            .upgrade()
            .ok_or_else(|| anyhow!("terminal thread terminal closed"))?;

        workspace.update(cx, |workspace, cx| {
            let pane = workspace
                .pane_for_item_id(terminal_view.entity_id())
                .ok_or_else(|| anyhow!("terminal thread pane closed"))?;
            pane.update(cx, |pane, cx| {
                let index = pane
                    .index_for_item(&terminal_view)
                    .ok_or_else(|| anyhow!("terminal thread item closed"))?;
                pane.activate_item(index, true, true, window, cx);
                anyhow::Ok(())
            })
        })?;

        self.update_attention(terminal_item_id, false, cx);
        Ok(())
    }

    fn register(
        &mut self,
        kind: TerminalThreadKind,
        workspace: Entity<Workspace>,
        terminal_view: Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let terminal_item_id = terminal_view.entity_id();
        let title = terminal_view.read(cx).tab_content_text(0, cx);
        let project_name = project_name(workspace.read(cx), cx);
        let metadata = TerminalThreadMetadata {
            terminal_item_id,
            kind,
            title,
            project_name,
            has_attention: terminal_view.read(cx).has_bell(),
            last_activity_at: SystemTime::now(),
        };
        self.threads.insert(
            terminal_item_id,
            TerminalThreadEntry {
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
        self.subscriptions.insert(
            terminal_item_id,
            vec![item_subscription, terminal_subscription],
        );
        cx.emit(TerminalThreadStoreEvent::Updated);
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
        cx.emit(TerminalThreadStoreEvent::Updated);
        cx.notify();
    }

    fn update_attention(
        &mut self,
        terminal_item_id: EntityId,
        has_attention: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.threads.get_mut(&terminal_item_id) else {
            return;
        };
        entry.metadata.has_attention = has_attention;
        entry.metadata.last_activity_at = SystemTime::now();
        cx.emit(TerminalThreadStoreEvent::Updated);
        cx.notify();
    }
}

pub struct TerminalThreadOrganizer {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    store: Entity<TerminalThreadStore>,
    _subscription: Subscription,
}

impl TerminalThreadOrganizer {
    fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let store = TerminalThreadStore::global(cx);
        let subscription = cx.subscribe(&store, |_, _, _: &TerminalThreadStoreEvent, cx| {
            cx.notify();
        });
        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            store,
            _subscription: subscription,
        }
    }

    fn launch_thread(
        &mut self,
        kind: TerminalThreadKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let settings = TerminalThreadSettings::get_global(cx);
        let command = match kind {
            TerminalThreadKind::Codex => settings.codex.clone(),
            TerminalThreadKind::Claude => settings.claude.clone(),
            TerminalThreadKind::Shell => settings.shell.clone(),
        };
        workspace.update(cx, |workspace, cx| {
            launch_thread(workspace, kind, command, window, cx);
        });
    }

    fn focus_thread(
        &mut self,
        terminal_item_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store
            .update(cx, |store, cx| {
                store.focus_thread(terminal_item_id, window, cx)
            })
            .log_err();
    }

    fn render_thread_row(
        &self,
        metadata: TerminalThreadMetadata,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let terminal_item_id = metadata.terminal_item_id;
        h_flex()
            .id(("terminal-thread-row", terminal_item_id.as_u64()))
            .w_full()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded_sm()
            .border_1()
            .border_color(cx.theme().colors().border)
            .hover(|style| style.bg(cx.theme().colors().element_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus_thread(terminal_item_id, window, cx);
            }))
            .child(
                Icon::new(match metadata.kind {
                    TerminalThreadKind::Codex | TerminalThreadKind::Claude => IconName::Sparkle,
                    TerminalThreadKind::Shell => IconName::Terminal,
                })
                .size(IconSize::Small)
                .color(if metadata.has_attention {
                    Color::Accent
                } else {
                    Color::Muted
                }),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .child(
                        Label::new(metadata.title.clone())
                            .size(LabelSize::Small)
                            .truncate(),
                    )
                    .child(
                        Label::new(format!(
                            "{} - {}",
                            metadata.kind.label(),
                            metadata.project_name
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                        .truncate(),
                    ),
            )
            .when(metadata.has_attention, |this| {
                this.child(
                    Icon::new(IconName::Circle)
                        .size(IconSize::Indicator)
                        .color(Color::Accent),
                )
            })
            .into_any_element()
    }
}

impl Focusable for TerminalThreadOrganizer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<()> for TerminalThreadOrganizer {}

impl Item for TerminalThreadOrganizer {
    type Event = ();

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        SharedString::new_static("Terminal Threads")
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Terminal))
    }
}

impl Render for TerminalThreadOrganizer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut entries = self.store.read(cx).entries().cloned().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            left.project_name
                .as_ref()
                .cmp(right.project_name.as_ref())
                .then_with(|| right.last_activity_at.cmp(&left.last_activity_at))
                .then_with(|| left.kind.label().cmp(right.kind.label()))
        });

        let mut entry_elements = Vec::new();
        let mut current_project: Option<SharedString> = None;
        for metadata in entries {
            let is_new_project = current_project
                .as_ref()
                .is_none_or(|project| project.as_ref() != metadata.project_name.as_ref());
            if is_new_project {
                current_project = Some(metadata.project_name.clone());
                entry_elements.push(
                    Label::new(metadata.project_name.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .mt_2()
                        .into_any_element(),
                );
            }
            entry_elements.push(self.render_thread_row(metadata, cx));
        }

        v_flex()
            .id("terminal-thread-organizer")
            .key_context("TerminalThreadOrganizer")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new("Terminal Threads").size(LabelSize::Small))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("new-codex-thread", "Codex")
                                    .size(ButtonSize::Compact)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.launch_thread(TerminalThreadKind::Codex, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("new-claude-thread", "Claude")
                                    .size(ButtonSize::Compact)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.launch_thread(TerminalThreadKind::Claude, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("new-shell-thread", "Shell")
                                    .size(ButtonSize::Compact)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.launch_thread(TerminalThreadKind::Shell, window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .id("terminal-thread-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_3()
                    .gap_1()
                    .when(entry_elements.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .gap_1()
                                .child(Label::new("No terminal threads").color(Color::Muted))
                                .child(
                                    Label::new("Start Codex, Claude, or shell from this view.")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        )
                    })
                    .children(entry_elements),
            )
    }
}

pub fn init(cx: &mut App) {
    TerminalThreadSettings::register(cx);
    TerminalThreadStore::init_global(cx);

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(open_thread_organizer);
        workspace.register_action(new_codex_thread);
        workspace.register_action(new_claude_thread);
        workspace.register_action(new_shell_thread);
    })
    .detach();
}

fn open_thread_organizer(
    workspace: &mut Workspace,
    _: &OpenTerminalThreads,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(organizer) = find_open_organizer(workspace, cx) {
        if let Some(pane) = workspace.pane_for_item_id(organizer.entity_id()) {
            pane.update(cx, |pane, cx| {
                if let Some(index) = pane.index_for_item(&organizer) {
                    pane.activate_item(index, true, true, window, cx);
                }
            });
        }
        return;
    }

    let workspace_handle = workspace.weak_handle();
    let organizer = cx.new(|cx| TerminalThreadOrganizer::new(workspace_handle, cx));
    workspace.add_item_to_active_pane(Box::new(organizer), None, true, window, cx);
}

fn find_open_organizer(workspace: &Workspace, cx: &App) -> Option<Entity<TerminalThreadOrganizer>> {
    workspace.panes().iter().find_map(|pane| {
        pane.read(cx)
            .items_of_type::<TerminalThreadOrganizer>()
            .next()
    })
}

fn new_codex_thread(
    workspace: &mut Workspace,
    _: &NewCodexThread,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let settings = TerminalThreadSettings::get_global(cx);
    launch_thread(
        workspace,
        TerminalThreadKind::Codex,
        settings.codex.clone(),
        window,
        cx,
    );
}

fn new_claude_thread(
    workspace: &mut Workspace,
    _: &NewClaudeThread,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let settings = TerminalThreadSettings::get_global(cx);
    launch_thread(
        workspace,
        TerminalThreadKind::Claude,
        settings.claude.clone(),
        window,
        cx,
    );
}

fn new_shell_thread(
    workspace: &mut Workspace,
    _: &NewShellThread,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let settings = TerminalThreadSettings::get_global(cx);
    launch_thread(
        workspace,
        TerminalThreadKind::Shell,
        settings.shell.clone(),
        window,
        cx,
    );
}

fn launch_thread(
    workspace: &mut Workspace,
    kind: TerminalThreadKind,
    command: TerminalThreadCommand,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let cwd = command
        .cwd
        .clone()
        .or_else(|| default_working_directory(workspace, cx));
    let label = kind.label().to_string();
    let command_label = command.command_label(&label);
    let task = SpawnInTerminal {
        full_label: label.clone(),
        label,
        command: command.command,
        args: command.args,
        command_label,
        cwd,
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
    let terminal_view =
        TerminalPanel::add_center_terminal_view(workspace, window, cx, |project, cx| {
            project.create_terminal_task(task, cx)
        });
    cx.spawn_in(window, async move |_workspace, cx| {
        let (_, terminal_view) = terminal_view.await?;
        let terminal_view = terminal_view
            .upgrade()
            .ok_or_else(|| anyhow!("terminal thread terminal closed before registration"))?;
        let store = cx.update(|_, cx| TerminalThreadStore::global(cx))?;
        store.update(cx, |store, cx| {
            store.register(kind, workspace_entity, terminal_view, cx);
        });
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn project_name(workspace: &Workspace, cx: &App) -> SharedString {
    workspace
        .project()
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|worktree| SharedString::from(worktree.read(cx).root_name_str().to_string()))
        .unwrap_or_else(|| SharedString::new_static("Project"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use gpui::{TestAppContext, WindowHandle};
    use pretty_assertions::assert_eq;
    use project::{FakeFs, Project};
    use settings::{
        SettingsContent, SettingsStore, TerminalThreadCommandContent, TerminalThreadSettingsContent,
    };
    use workspace::{MultiWorkspace, item::test::TestItem};

    #[test]
    fn terminal_thread_settings_default_and_override() {
        let mut content = SettingsContent::default();
        let mut env = HashMap::default();
        env.insert("CODEX_HOME".to_string(), "/tmp/codex".to_string());

        content.terminal_threads = Some(TerminalThreadSettingsContent {
            codex: Some(TerminalThreadCommandContent {
                command: Some("codex-dev".to_string()),
                args: Some(vec!["--full-auto".to_string()]),
                env: Some(env.clone()),
                cwd: Some(PathBuf::from("/work/repo")),
            }),
            claude: None,
            shell: Some(TerminalThreadCommandContent {
                command: Some("zsh".to_string()),
                args: Some(vec!["-l".to_string()]),
                env: Some(HashMap::default()),
                cwd: None,
            }),
        });

        let settings = TerminalThreadSettings::from_settings(&content);

        assert_eq!(settings.codex.command, Some("codex-dev".to_string()));
        assert_eq!(settings.codex.args, vec!["--full-auto".to_string()]);
        assert_eq!(settings.codex.env, env);
        assert_eq!(settings.codex.cwd, Some(PathBuf::from("/work/repo")));
        assert_eq!(settings.claude.command, Some("claude".to_string()));
        assert_eq!(settings.shell.command, Some("zsh".to_string()));
        assert_eq!(settings.shell.args, vec!["-l".to_string()]);
    }

    #[gpui::test]
    async fn terminal_thread_launches_commands_and_groups_by_project(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);
        configure_echo_threads(cx);

        let root_a = init_workspace(cx, "/root_a").await;
        let root_b = init_workspace(cx, "/root_b").await;

        invoke_thread(&root_a, TerminalThreadKind::Codex, cx);
        wait_for_thread_count(cx, 1);
        invoke_thread(&root_b, TerminalThreadKind::Claude, cx);
        wait_for_thread_count(cx, 2);
        invoke_thread(&root_a, TerminalThreadKind::Shell, cx);
        wait_for_thread_count(cx, 3);

        let entries = thread_entries(cx);
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().any(|entry| {
            entry.kind == TerminalThreadKind::Codex && entry.project_name.as_ref() == "root_a"
        }));
        assert!(entries.iter().any(|entry| {
            entry.kind == TerminalThreadKind::Shell && entry.project_name.as_ref() == "root_a"
        }));
        assert!(entries.iter().any(|entry| {
            entry.kind == TerminalThreadKind::Claude && entry.project_name.as_ref() == "root_b"
        }));

        assert_eq!(terminal_views(&root_a, cx).len(), 2);
        assert_eq!(terminal_views(&root_b, cx).len(), 1);
    }

    #[gpui::test]
    async fn terminal_thread_metadata_tracks_title_attention_and_refocus(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);
        configure_echo_threads(cx);

        let window_handle = init_workspace(cx, "/root").await;
        invoke_thread(&window_handle, TerminalThreadKind::Codex, cx);
        wait_for_thread_count(cx, 1);

        let terminal_view = terminal_views(&window_handle, cx)
            .into_iter()
            .next()
            .expect("terminal thread view should exist");
        let terminal_item_id = terminal_view.entity_id();

        terminal_view.update(cx, |terminal_view, cx| {
            terminal_view.set_custom_title(Some("codex-plan".to_string()), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            thread_entries(cx)
                .into_iter()
                .find(|entry| entry.terminal_item_id == terminal_item_id)
                .expect("thread metadata should exist")
                .title
                .as_ref(),
            "codex-plan"
        );

        let editor_item = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    let editor_item = cx.new(|cx| TestItem::new(cx).with_label("editor"));
                    workspace.add_item_to_active_pane(
                        Box::new(editor_item.clone()),
                        None,
                        true,
                        window,
                        cx,
                    );
                    editor_item
                })
            })
            .expect("failed to add editor item");
        cx.run_until_parked();
        assert_eq!(active_item_id(&window_handle, cx), editor_item.entity_id());

        let terminal =
            terminal_view.read_with(cx, |terminal_view, _| terminal_view.terminal().clone());
        terminal.update(cx, |_, cx| cx.emit(TerminalEvent::Bell));
        cx.run_until_parked();
        assert!(
            thread_entries(cx)
                .into_iter()
                .find(|entry| entry.terminal_item_id == terminal_item_id)
                .expect("thread metadata should exist")
                .has_attention
        );

        window_handle
            .update(cx, |_, window, cx| {
                TerminalThreadStore::global(cx).update(cx, |store, cx| {
                    store.focus_thread(terminal_item_id, window, cx)
                })
            })
            .expect("failed to focus thread")
            .expect("thread focus should succeed");
        assert_eq!(active_item_id(&window_handle, cx), terminal_item_id);
        assert!(
            !thread_entries(cx)
                .into_iter()
                .find(|entry| entry.terminal_item_id == terminal_item_id)
                .expect("thread metadata should exist")
                .has_attention
        );
    }

    #[gpui::test]
    async fn terminal_thread_organizer_opens_once(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);

        let window_handle = init_workspace(cx, "/root").await;
        for _ in 0..2 {
            window_handle
                .update(cx, |multi_workspace, window, cx| {
                    multi_workspace.workspace().update(cx, |workspace, cx| {
                        open_thread_organizer(workspace, &OpenTerminalThreads, window, cx);
                    })
                })
                .expect("failed to open terminal thread organizer");
        }

        assert_eq!(organizer_count(&window_handle, cx), 1);
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    fn configure_echo_threads(cx: &mut TestAppContext) {
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings.terminal_threads = Some(TerminalThreadSettingsContent {
                    codex: Some(echo_command("codex")),
                    claude: Some(echo_command("claude")),
                    shell: Some(echo_command("shell")),
                });
            });
        });
    }

    fn echo_command(label: &str) -> TerminalThreadCommandContent {
        TerminalThreadCommandContent {
            command: Some("echo".to_string()),
            args: Some(vec![label.to_string()]),
            env: Some(HashMap::default()),
            cwd: None,
        }
    }

    async fn init_workspace(
        cx: &mut TestAppContext,
        root_path: &'static str,
    ) -> WindowHandle<MultiWorkspace> {
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [Path::new(root_path)], cx).await;
        cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx))
    }

    fn invoke_thread(
        window_handle: &WindowHandle<MultiWorkspace>,
        kind: TerminalThreadKind,
        cx: &mut TestAppContext,
    ) {
        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace
                    .workspace()
                    .update(cx, |workspace, cx| match kind {
                        TerminalThreadKind::Codex => {
                            new_codex_thread(workspace, &NewCodexThread, window, cx)
                        }
                        TerminalThreadKind::Claude => {
                            new_claude_thread(workspace, &NewClaudeThread, window, cx)
                        }
                        TerminalThreadKind::Shell => {
                            new_shell_thread(workspace, &NewShellThread, window, cx)
                        }
                    });
            })
            .expect("failed to invoke terminal thread action");
    }

    fn wait_for_thread_count(cx: &mut TestAppContext, expected_count: usize) {
        for _ in 0..10 {
            cx.run_until_parked();
            if thread_entries(cx).len() >= expected_count {
                return;
            }
        }
    }

    fn thread_entries(cx: &mut TestAppContext) -> Vec<TerminalThreadMetadata> {
        cx.update(|cx| {
            TerminalThreadStore::global(cx)
                .read(cx)
                .entries()
                .cloned()
                .collect()
        })
    }

    fn terminal_views(
        window_handle: &WindowHandle<MultiWorkspace>,
        cx: &mut TestAppContext,
    ) -> Vec<Entity<TerminalView>> {
        window_handle
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().read(cx);
                let mut terminal_views = Vec::new();
                for pane in workspace.panes() {
                    terminal_views.extend(pane.read(cx).items_of_type::<TerminalView>());
                }
                terminal_views
            })
            .expect("failed to collect terminal views")
    }

    fn organizer_count(
        window_handle: &WindowHandle<MultiWorkspace>,
        cx: &mut TestAppContext,
    ) -> usize {
        window_handle
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().read(cx);
                workspace
                    .panes()
                    .iter()
                    .map(|pane| {
                        pane.read(cx)
                            .items_of_type::<TerminalThreadOrganizer>()
                            .count()
                    })
                    .sum()
            })
            .expect("failed to count organizers")
    }

    fn active_item_id(
        window_handle: &WindowHandle<MultiWorkspace>,
        cx: &mut TestAppContext,
    ) -> EntityId {
        window_handle
            .update(cx, |multi_workspace, _, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .active_pane()
                    .read(cx)
                    .active_item()
                    .expect("active item should exist")
                    .item_id()
            })
            .expect("failed to inspect active item")
    }
}
