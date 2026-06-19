use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use collections::HashMap;
use fs::Fs;
use futures::StreamExt;
use gpui::{
    Action, Anchor, AnyElement, App, AppContext as _, AsyncWindowContext, Context, Entity,
    EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    Pixels, Point, Render, SharedString, Styled, Subscription, Task, WeakEntity, Window, anchored,
    deferred, div,
};
use settings::{DockSide, Settings};
use ui::{
    Color, ContextMenu, Disclosure, Icon, IconButton, IconButtonShape, IconName, IconSize, Label,
    LabelSize, Tooltip, prelude::*,
};
use util::ResultExt as _;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::history::{self, project_worktree_roots};
use crate::store::{
    self, AgentThreadMetadata, AgentThreadRow, AgentThreadStore, AgentThreadStoreEvent,
    merge_threads,
};
use crate::{AgentKindDefinition, AgentThreadSettings, HistoricalThread, agent_kind_registry};

enum HistoricalState {
    Loading,
    Loaded(Vec<HistoricalThread>),
    Unavailable,
}

struct SectionState {
    collapsed: bool,
    show_all: bool,
    historical: HistoricalState,
}

impl Default for SectionState {
    fn default() -> Self {
        Self {
            collapsed: false,
            show_all: false,
            historical: HistoricalState::Loading,
        }
    }
}

/// Truncates `rows` to `cap` unless `show_all` has been toggled, returning
/// whether truncation happened (so the caller knows to render "Show more").
fn apply_visible_cap(
    mut rows: Vec<AgentThreadRow>,
    cap: usize,
    show_all: bool,
) -> (Vec<AgentThreadRow>, bool) {
    let truncated = !show_all && rows.len() > cap;
    if truncated {
        rows.truncate(cap);
    }
    (rows, truncated)
}

pub struct AgentThreadsPanel {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    fs: Arc<dyn Fs>,
    store: Entity<AgentThreadStore>,
    registry: Vec<AgentKindDefinition>,
    sections: HashMap<&'static str, SectionState>,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    _subscriptions: Vec<Subscription>,
    _history_tasks: Vec<Task<()>>,
}

impl AgentThreadsPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            Self::new(workspace, window, cx)
        })
    }

    fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let workspace_handle = cx.entity().downgrade();
        let fs = workspace.app_state().fs.clone();
        cx.new(|cx| {
            let store = AgentThreadStore::global(cx);
            let store_subscription = cx.subscribe(&store, |_, _, _: &AgentThreadStoreEvent, cx| {
                cx.notify();
            });
            let registry = agent_kind_registry();
            let mut sections = HashMap::default();
            for kind in &registry {
                sections.insert(kind.id, SectionState::default());
            }
            let panel = Self {
                focus_handle: cx.focus_handle(),
                workspace: workspace_handle,
                fs,
                store,
                registry,
                sections,
                context_menu: None,
                _subscriptions: vec![store_subscription],
                _history_tasks: Vec::new(),
            };
            // Deferred via `cx.spawn` rather than called inline: at this
            // point we're still nested inside the caller's `workspace.update`
            // (see `load`/`new`'s callers), and `refresh_history` reads the
            // workspace's project -- reading it synchronously here would
            // panic with "cannot read Workspace while it is already being
            // updated."
            cx.spawn(async move |this: WeakEntity<Self>, cx| {
                this.update(cx, |this, cx| this.refresh_history(cx)).ok();
            })
            .detach();
            let _ = window;
            panel
        })
    }

    fn refresh_history(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let project = workspace.read(cx).project().clone();

        self._history_tasks.clear();
        for kind in self.registry.clone() {
            let Some(provider) = kind.history_provider.clone() else {
                continue;
            };
            if let Some(section) = self.sections.get_mut(kind.id) {
                section.historical = HistoricalState::Loading;
            }
            let project = project.clone();
            let kind_id = kind.id;
            let env_var = kind.home_env_var;
            let dir_name = kind.home_dir_name;
            let task = cx.spawn(async move |this, cx| {
                loop {
                    let scan_result = async {
                        let host =
                            history::resolve_history_host(&project, env_var, dir_name, cx).await?;
                        let project_roots = project
                            .read_with(cx, |project, cx| project_worktree_roots(project, cx));
                        let threads = provider.scan(&host, &project_roots).await?;
                        anyhow::Ok((host, threads))
                    }
                    .await;

                    let host = match scan_result {
                        Ok((host, threads)) => {
                            this.update(cx, |this, cx| {
                                if let Some(section) = this.sections.get_mut(kind_id) {
                                    section.historical = HistoricalState::Loaded(threads);
                                }
                                cx.notify();
                            })
                            .ok();
                            host
                        }
                        Err(error) => {
                            log::warn!(
                                "agent_threads: failed to scan {kind_id} history: {error:#}"
                            );
                            this.update(cx, |this, cx| {
                                if let Some(section) = this.sections.get_mut(kind_id) {
                                    section.historical = HistoricalState::Unavailable;
                                }
                                cx.notify();
                            })
                            .ok();
                            return;
                        }
                    };

                    let (mut events, _watcher) =
                        host.fs.watch(&host.base_dir, Duration::from_secs(1)).await;
                    if events.next().await.is_none() {
                        return;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(300))
                        .await;
                }
            });
            self._history_tasks.push(task);
        }
    }

    fn launch_new(
        &mut self,
        kind: &AgentKindDefinition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            store::launch_new_thread(workspace, kind, window, cx);
        });
    }

    fn resume(
        &mut self,
        kind: &AgentKindDefinition,
        thread: &HistoricalThread,
        extra_args: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let kind = kind.clone();
        let thread = thread.clone();
        let extra_args = extra_args.to_vec();
        workspace.update(cx, |workspace, cx| {
            store::resume_thread(workspace, &kind, &thread, &extra_args, window, cx);
        });
    }

    fn focus_live_thread(
        &mut self,
        terminal_item_id: gpui::EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store
            .update(cx, |store, cx| {
                store.focus_thread(terminal_item_id, window, cx)
            })
            .log_err();
    }

    fn toggle_section_collapsed(&mut self, kind_id: &'static str) {
        if let Some(section) = self.sections.get_mut(kind_id) {
            section.collapsed = !section.collapsed;
        }
    }

    fn show_all_for_section(&mut self, kind_id: &'static str) {
        if let Some(section) = self.sections.get_mut(kind_id) {
            section.show_all = true;
        }
    }

    fn deploy_resume_options_menu(
        &mut self,
        kind: AgentKindDefinition,
        thread: HistoricalThread,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let resume_options = kind.resume_options.clone();
        let context_menu = ContextMenu::build(window, cx, move |mut context_menu, _, _| {
            {
                let workspace = workspace.clone();
                let kind = kind.clone();
                let thread = thread.clone();
                context_menu = context_menu.entry("Resume", None, move |window, cx| {
                    let Some(workspace) = workspace.upgrade() else {
                        return;
                    };
                    let kind = kind.clone();
                    let thread = thread.clone();
                    workspace.update(cx, |workspace, cx| {
                        store::resume_thread(workspace, &kind, &thread, &[], window, cx);
                    });
                });
            }
            for option in resume_options {
                let workspace = workspace.clone();
                let kind = kind.clone();
                let thread = thread.clone();
                let label = SharedString::from(format!("Resume — {}", option.label));
                let args = option.args.clone();
                context_menu = context_menu.entry(label, None, move |window, cx| {
                    let Some(workspace) = workspace.upgrade() else {
                        return;
                    };
                    let kind = kind.clone();
                    let thread = thread.clone();
                    let args = args.clone();
                    workspace.update(cx, |workspace, cx| {
                        store::resume_thread(workspace, &kind, &thread, &args, window, cx);
                    });
                });
            }
            context_menu
        });
        self.set_context_menu(context_menu, position, window, cx);
    }

    fn set_context_menu(
        &mut self,
        context_menu: Entity<ContextMenu>,
        position: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let subscription =
            cx.subscribe_in(
                &context_menu,
                window,
                |this, _, _: &gpui::DismissEvent, window, cx| {
                    if this.context_menu.as_ref().is_some_and(|(menu, _, _)| {
                        menu.focus_handle(cx).contains_focused(window, cx)
                    }) {
                        cx.focus_self(window);
                    }
                    this.context_menu.take();
                    cx.notify();
                },
            );
        self.context_menu = Some((context_menu, position, subscription));
        cx.notify();
    }

    fn render_section(
        &mut self,
        kind: &AgentKindDefinition,
        project_roots: &[PathBuf],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let live = self
            .store
            .read(cx)
            .live_threads_for_project(kind.id, project_roots);
        let section = self.sections.entry(kind.id).or_default();
        let collapsed = section.collapsed;
        let show_all = section.show_all;
        let (historical, scan_status) = match &section.historical {
            HistoricalState::Loaded(threads) => (threads.clone(), None),
            HistoricalState::Loading => (Vec::new(), Some("Scanning history…")),
            HistoricalState::Unavailable => (Vec::new(), Some("Couldn't scan history")),
        };

        let rows = merge_threads(live, historical);
        let cap = AgentThreadSettings::get_global(cx).max_visible_threads_per_agent;
        let total = rows.len();
        let (rows, truncated) = apply_visible_cap(rows, cap, show_all);

        let kind_id = kind.id;
        let header = h_flex()
            .id(SharedString::from(format!(
                "agent-thread-section-header-{kind_id}"
            )))
            .w_full()
            .justify_between()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Disclosure::new(
                            SharedString::from(format!("agent-thread-disclosure-{kind_id}")),
                            !collapsed,
                        )
                        .on_toggle_expanded(Some(Arc::new(cx.listener(
                            move |this, _, _, cx| {
                                this.toggle_section_collapsed(kind_id);
                                cx.notify();
                            },
                        ))
                            as Arc<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>)),
                    )
                    .child(
                        Icon::new(kind.icon)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new(kind.label.clone()).size(LabelSize::Small))
                    .child(
                        Label::new(total.to_string())
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
            .child(
                IconButton::new(
                    SharedString::from(format!("agent-thread-new-{kind_id}")),
                    IconName::Plus,
                )
                .shape(IconButtonShape::Square)
                .icon_size(IconSize::Small)
                .tooltip(Tooltip::text(format!("New {} thread", kind.label)))
                .on_click(cx.listener({
                    let kind = kind.clone();
                    move |this, _, window, cx| {
                        this.launch_new(&kind, window, cx);
                    }
                })),
            );

        let mut body_children: Vec<AnyElement> = Vec::new();
        if !collapsed {
            if rows.is_empty() {
                let message = scan_status
                    .map(SharedString::new_static)
                    .unwrap_or_else(|| {
                        SharedString::from(format!("No {} threads yet", kind.label))
                    });
                body_children.push(
                    Label::new(message)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .into_any_element(),
                );
            } else {
                for row in rows {
                    body_children.push(self.render_row(kind, row, cx));
                }
                if truncated {
                    let remaining = total - cap;
                    body_children.push(
                        Button::new(
                            SharedString::from(format!("agent-thread-show-more-{kind_id}")),
                            format!("Show {remaining} more"),
                        )
                        .size(ButtonSize::Compact)
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.show_all_for_section(kind_id);
                            cx.notify();
                        }))
                        .into_any_element(),
                    );
                }
            }
        }

        v_flex()
            .id(SharedString::from(format!(
                "agent-thread-section-{kind_id}"
            )))
            .w_full()
            .child(header)
            .children(body_children)
            .into_any_element()
    }

    fn render_row(
        &mut self,
        kind: &AgentKindDefinition,
        row: AgentThreadRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            AgentThreadRow::Live(metadata) => self.render_live_row(metadata, cx),
            AgentThreadRow::Historical(thread) => self.render_historical_row(kind, thread, cx),
        }
    }

    fn render_live_row(
        &mut self,
        metadata: AgentThreadMetadata,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let terminal_item_id = metadata.terminal_item_id;
        h_flex()
            .id(("agent-thread-live-row", terminal_item_id.as_u64()))
            .w_full()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_sm()
            .hover(|style| style.bg(cx.theme().colors().element_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus_live_thread(terminal_item_id, window, cx);
            }))
            .child(Icon::new(IconName::Circle).size(IconSize::Indicator).color(
                if metadata.has_attention {
                    Color::Accent
                } else {
                    Color::Success
                },
            ))
            .child(Label::new(metadata.title).size(LabelSize::Small).truncate())
            .into_any_element()
    }

    fn render_historical_row(
        &mut self,
        kind: &AgentKindDefinition,
        thread: HistoricalThread,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row_id =
            SharedString::from(format!("agent-thread-historical-row-{}", thread.session_id));
        let options_button_id =
            SharedString::from(format!("agent-thread-resume-options-{}", thread.session_id));
        let click_kind = kind.clone();
        let click_thread = thread.clone();
        let menu_kind = kind.clone();
        let menu_thread = thread.clone();
        let menu_kind_for_button = kind.clone();
        let menu_thread_for_button = thread.clone();
        h_flex()
            .id(row_id)
            .w_full()
            .justify_between()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_sm()
            .hover(|style| style.bg(cx.theme().colors().element_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.resume(&click_kind, &click_thread, &[], window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.deploy_resume_options_menu(
                        menu_kind.clone(),
                        menu_thread.clone(),
                        event.position,
                        window,
                        cx,
                    );
                }),
            )
            .child(
                h_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_2()
                    .child(
                        Icon::new(IconName::HistoryRerun)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(thread.title)
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    ),
            )
            .child(
                IconButton::new(options_button_id, IconName::Ellipsis)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Resume with options"))
                    .on_click(
                        cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                            this.deploy_resume_options_menu(
                                menu_kind_for_button.clone(),
                                menu_thread_for_button.clone(),
                                event.position(),
                                window,
                                cx,
                            );
                        }),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for AgentThreadsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for AgentThreadsPanel {}

impl Panel for AgentThreadsPanel {
    fn persistent_name() -> &'static str {
        "Agent Threads Panel"
    }

    fn panel_key() -> &'static str {
        "AgentThreadsPanel"
    }

    fn position(&self, _: &Window, cx: &App) -> DockPosition {
        match AgentThreadSettings::get_global(cx).dock {
            DockSide::Left => DockPosition::Left,
            DockSide::Right => DockPosition::Right,
        }
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            let dock = match position {
                DockPosition::Left | DockPosition::Bottom => DockSide::Left,
                DockPosition::Right => DockSide::Right,
            };
            settings.agent_threads.get_or_insert_default().dock = Some(dock);
        });
    }

    fn default_size(&self, _: &Window, _: &App) -> gpui::Pixels {
        gpui::px(280.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Sparkle)
    }

    fn icon_tooltip(&self, _window: &Window, _: &App) -> Option<&'static str> {
        Some("Agent Threads")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(flint_actions::agent_threads::ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        7
    }
}

impl Render for AgentThreadsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return div().size_full().into_any_element();
        };
        let project = workspace.read(cx).project().clone();
        let project_roots = project_worktree_roots(project.read(cx), cx);

        let registry = self.registry.clone();
        let mut sections = Vec::new();
        for kind in &registry {
            sections.push(self.render_section(kind, &project_roots, cx));
        }

        v_flex()
            .id("agent-threads-panel")
            .key_context("AgentThreadsPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(
                v_flex()
                    .id("agent-thread-sections")
                    .flex_1()
                    .overflow_y_scroll()
                    .py_1()
                    .children(sections),
            )
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, WindowHandle};
    use pretty_assertions::assert_eq;
    use project::{FakeFs, Project};
    use settings::{AgentThreadCommandContent, AgentThreadSettingsContent, SettingsStore};
    use std::path::Path;
    use terminal_view::TerminalView;
    use workspace::MultiWorkspace;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            terminal_view::init(cx);
            crate::init(cx);
        });
    }

    fn configure_echo_threads(
        cx: &mut TestAppContext,
        root_path: &str,
        max_visible_threads_per_agent: usize,
    ) {
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings.agent_threads = Some(AgentThreadSettingsContent {
                    codex: Some(echo_command("codex", root_path)),
                    claude: Some(echo_command("claude", root_path)),
                    max_visible_threads_per_agent: Some(max_visible_threads_per_agent),
                    dock: None,
                });
            });
        });
    }

    // `cwd` is pinned explicitly here rather than left to fall back to
    // `default_working_directory` -- that falls back to the real home
    // directory when a freshly created test project has no "active entry"
    // yet, which wouldn't match the fixture's root path.
    fn echo_command(label: &str, root_path: &str) -> AgentThreadCommandContent {
        AgentThreadCommandContent {
            command: Some("echo".to_string()),
            args: Some(vec![label.to_string()]),
            env: Some(collections::HashMap::default()),
            cwd: Some(PathBuf::from(root_path)),
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

    fn codex_kind() -> AgentKindDefinition {
        agent_kind_registry()
            .into_iter()
            .find(|kind| kind.id == "codex")
            .expect("codex should be registered")
    }

    fn launch_codex_thread(window_handle: &WindowHandle<MultiWorkspace>, cx: &mut TestAppContext) {
        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    store::launch_new_thread(workspace, &codex_kind(), window, cx);
                });
            })
            .expect("failed to launch codex thread");
    }

    fn live_codex_threads(cx: &mut TestAppContext, project_root: &str) -> Vec<AgentThreadMetadata> {
        cx.update(|cx| {
            AgentThreadStore::global(cx)
                .read(cx)
                .live_threads_for_project("codex", &[PathBuf::from(project_root)])
        })
    }

    // Spawning the underlying process (and on Windows, setting up its ConPTY)
    // happens on a real OS thread, so registration can lag behind a plain
    // `run_until_parked()` by more than a few milliseconds under CI load.
    // Interleave real timer ticks so that background completion has a chance
    // to land between polls instead of racing a tight, time-frozen loop.
    async fn wait_for_live_count(cx: &mut TestAppContext, project_root: &str, expected: usize) {
        for _ in 0..50 {
            cx.run_until_parked();
            if live_codex_threads(cx, project_root).len() >= expected {
                return;
            }
            cx.executor().timer(Duration::from_millis(50)).await;
        }
    }

    fn terminal_views(
        window_handle: &WindowHandle<MultiWorkspace>,
        cx: &mut TestAppContext,
    ) -> Vec<Entity<TerminalView>> {
        window_handle
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().read(cx);
                let mut views = Vec::new();
                for pane in workspace.panes() {
                    views.extend(pane.read(cx).items_of_type::<TerminalView>());
                }
                views
            })
            .expect("failed to collect terminal views")
    }

    // See `wait_for_live_count` for why this can't be a single `run_until_parked()`.
    async fn wait_for_terminal_view_count(
        window_handle: &WindowHandle<MultiWorkspace>,
        cx: &mut TestAppContext,
        expected: usize,
    ) {
        for _ in 0..50 {
            cx.run_until_parked();
            if terminal_views(window_handle, cx).len() >= expected {
                return;
            }
            cx.executor().timer(Duration::from_millis(50)).await;
        }
    }

    fn active_item_id(
        window_handle: &WindowHandle<MultiWorkspace>,
        cx: &mut TestAppContext,
    ) -> gpui::EntityId {
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

    #[gpui::test]
    async fn launching_a_new_thread_registers_it_as_live(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);
        configure_echo_threads(cx, "/root", 5);
        let window_handle = init_workspace(cx, "/root").await;

        launch_codex_thread(&window_handle, cx);
        wait_for_live_count(cx, "/root", 1).await;

        let metadata = live_codex_threads(cx, "/root");
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].kind_id, "codex");
        assert!(metadata[0].resumed_session_id.is_none());
    }

    #[gpui::test]
    async fn clicking_a_live_thread_focuses_its_terminal_without_duplicating(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        init_test(cx);
        configure_echo_threads(cx, "/root", 5);
        let window_handle = init_workspace(cx, "/root").await;

        launch_codex_thread(&window_handle, cx);
        wait_for_live_count(cx, "/root", 1).await;

        let terminal_item_id = live_codex_threads(cx, "/root")[0].terminal_item_id;
        assert_eq!(terminal_views(&window_handle, cx).len(), 1);

        // Switch focus away, then verify the panel's focus action brings the
        // existing terminal back instead of spawning a duplicate.
        let editor_item = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    let editor_item =
                        cx.new(|cx| workspace::item::test::TestItem::new(cx).with_label("editor"));
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

        window_handle
            .update(cx, |_, window, cx| {
                AgentThreadStore::global(cx).update(cx, |store, cx| {
                    store.focus_thread(terminal_item_id, window, cx)
                })
            })
            .expect("failed to focus thread")
            .expect("focus_thread should succeed");

        assert_eq!(active_item_id(&window_handle, cx), terminal_item_id);
        assert_eq!(terminal_views(&window_handle, cx).len(), 1);
    }

    #[test]
    fn apply_visible_cap_truncates_until_show_all_is_set() {
        let rows = vec![
            AgentThreadRow::Historical(HistoricalThread {
                session_id: SharedString::from("a"),
                title: SharedString::from("a"),
                project_root: PathBuf::from("/root"),
                last_activity_at: std::time::SystemTime::UNIX_EPOCH,
            }),
            AgentThreadRow::Historical(HistoricalThread {
                session_id: SharedString::from("b"),
                title: SharedString::from("b"),
                project_root: PathBuf::from("/root"),
                last_activity_at: std::time::SystemTime::UNIX_EPOCH,
            }),
            AgentThreadRow::Historical(HistoricalThread {
                session_id: SharedString::from("c"),
                title: SharedString::from("c"),
                project_root: PathBuf::from("/root"),
                last_activity_at: std::time::SystemTime::UNIX_EPOCH,
            }),
        ];

        let (capped, truncated) = apply_visible_cap(rows.clone(), 2, false);
        assert_eq!(capped.len(), 2);
        assert!(truncated);

        let (all, truncated) = apply_visible_cap(rows, 2, true);
        assert_eq!(all.len(), 3);
        assert!(!truncated);
    }

    #[gpui::test]
    async fn toggling_section_collapsed_and_show_all_updates_state(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);
        configure_echo_threads(cx, "/root", 5);
        let window_handle = init_workspace(cx, "/root").await;

        let panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    AgentThreadsPanel::new(workspace, window, cx)
                })
            })
            .expect("failed to create panel");

        let (collapsed_before, show_all_before) = panel.update(cx, |panel, _| {
            let section = panel.sections.get("codex").unwrap();
            (section.collapsed, section.show_all)
        });
        assert!(!collapsed_before);
        assert!(!show_all_before);

        panel.update(cx, |panel, _| {
            panel.toggle_section_collapsed("codex");
            panel.show_all_for_section("codex");
        });

        let (collapsed_after, show_all_after) = panel.update(cx, |panel, _| {
            let section = panel.sections.get("codex").unwrap();
            (section.collapsed, section.show_all)
        });
        assert!(collapsed_after);
        assert!(show_all_after);
    }

    #[gpui::test]
    async fn resume_spawns_a_terminal_with_the_resume_command(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);
        configure_echo_threads(cx, "/root", 5);
        let window_handle = init_workspace(cx, "/root").await;

        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("Fix the bug"),
            project_root: PathBuf::from("/root"),
            last_activity_at: std::time::SystemTime::UNIX_EPOCH,
        };

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    store::resume_thread(workspace, &codex_kind(), &thread, &[], window, cx);
                });
            })
            .expect("failed to resume thread");
        wait_for_terminal_view_count(&window_handle, cx, 1).await;

        let terminal_views = terminal_views(&window_handle, cx);
        assert_eq!(terminal_views.len(), 1);
        let terminal = terminal_views[0].read_with(cx, |view, _| view.terminal().clone());
        let spawned = terminal.read_with(cx, |terminal, _| {
            terminal
                .task()
                .expect("terminal should have a task")
                .spawned_task
                .clone()
        });
        assert_eq!(spawned.command, Some("echo".to_string()));
        assert_eq!(spawned.args, vec!["resume", "session-a"]);
        assert_eq!(spawned.cwd, Some(PathBuf::from("/root")));
    }

    #[gpui::test]
    async fn resume_with_options_appends_the_extra_flag(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);
        configure_echo_threads(cx, "/root", 5);
        let window_handle = init_workspace(cx, "/root").await;

        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("Fix the bug"),
            project_root: PathBuf::from("/root"),
            last_activity_at: std::time::SystemTime::UNIX_EPOCH,
        };

        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    store::resume_thread(
                        workspace,
                        &codex_kind(),
                        &thread,
                        &["--dangerously-bypass-approvals-and-sandbox".to_string()],
                        window,
                        cx,
                    );
                });
            })
            .expect("failed to resume thread with options");
        wait_for_terminal_view_count(&window_handle, cx, 1).await;

        let terminal_views = terminal_views(&window_handle, cx);
        assert_eq!(terminal_views.len(), 1);
        let terminal = terminal_views[0].read_with(cx, |view, _| view.terminal().clone());
        let spawned = terminal.read_with(cx, |terminal, _| {
            terminal
                .task()
                .expect("terminal should have a task")
                .spawned_task
                .clone()
        });
        assert_eq!(
            spawned.args,
            vec![
                "resume",
                "session-a",
                "--dangerously-bypass-approvals-and-sandbox"
            ]
        );
    }
}
