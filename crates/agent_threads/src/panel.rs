use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use collections::HashMap;
use fs::Fs;
use futures::StreamExt as _;
use gpui::{
    Action, Anchor, AnyElement, App, AppContext as _, AsyncWindowContext, Context, Entity,
    EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    Pixels, Point, Render, SharedString, Styled, Subscription, Task, WeakEntity, Window, anchored,
    deferred, div,
};
use settings::{DockSide, Settings, SettingsStore};
use ui::{
    Color, ContextMenu, Disclosure, Icon, IconButton, IconButtonShape, IconName, IconSize, Label,
    LabelSize, Tooltip, prelude::*,
};
use util::ResultExt as _;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::history::{self, HistoryParseCache, project_worktree_roots};
use crate::plan_usage::{PlanUsage, UsageColorBand, query_plan_usage};
use crate::store::{
    self, AgentThreadMetadata, AgentThreadRow, AgentThreadStore, AgentThreadStoreEvent,
    merge_threads,
};
use crate::{AgentKindDefinition, AgentThreadSettings, HistoricalThread, agent_kind_registry};

enum HistoricalState {
    Loading,
    Loaded(Arc<[HistoricalThread]>),
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
    history_tasks: HashMap<&'static str, Task<()>>,
    history_caches: HashMap<&'static str, Arc<HistoryParseCache>>,
    /// Filesystem watchers (local projects only) that trigger an incremental
    /// rescan when a kind's history directory changes, instead of re-sweeping
    /// every history file on each panel activation. Cleared on deactivate; the
    /// stored task owns the underlying `fs::Watcher` and stops it when dropped.
    history_watchers: HashMap<&'static str, Task<()>>,
    plan_usage: HashMap<&'static str, PlanUsage>,
    plan_usage_task: Option<Task<()>>,
    http_client: Arc<dyn http_client::HttpClient>,
    active: bool,
}

/// Debounce window for history filesystem watch events. Coalesces the burst of
/// writes an agent makes while a session is active into a single rescan.
const HISTORY_WATCH_LATENCY: Duration = Duration::from_millis(250);

fn history_cache_path(kind_id: &str) -> PathBuf {
    paths::data_dir()
        .join("agent_history_cache")
        .join(format!("{kind_id}.json"))
}

fn format_reset_countdown(reset_at: i64) -> Option<String> {
    let diff = reset_at - chrono::Utc::now().timestamp();
    if diff <= 0 {
        return None;
    }
    let hours = diff / 3600;
    let minutes = (diff % 3600) / 60;
    Some(if hours >= 24 {
        format!("{}d{}h", hours / 24, hours % 24)
    } else if hours > 0 {
        format!("{}h{}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    })
}

fn usage_color(percent: u8, cx: &App) -> Color {
    let status = cx.theme().status();
    Color::Custom(match UsageColorBand::for_percent(percent) {
        UsageColorBand::Green => status.success,
        UsageColorBand::LightGreen => status.success.blend(status.warning.opacity(0.35)),
        UsageColorBand::Yellow => status.warning,
        UsageColorBand::Orange => status.warning.blend(status.error.opacity(0.5)),
        UsageColorBand::Red => status.error,
    })
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
        let http_client = workspace.app_state().client.http_client();
        cx.new(|cx| {
            let store = AgentThreadStore::global(cx);
            let store_subscription =
                cx.subscribe(&store, |this: &mut AgentThreadsPanel, _, event, cx| {
                    this.handle_store_event(event, cx);
                });
            let settings = AgentThreadSettings::get_global(cx);
            let mut plan_usage_settings = (
                settings.show_plan_usage,
                settings.codex.clone(),
                settings.claude.clone(),
            );
            let settings_subscription = cx.observe_global::<SettingsStore>(move |this, cx| {
                let settings = AgentThreadSettings::get_global(cx);
                let new_settings = (
                    settings.show_plan_usage,
                    settings.codex.clone(),
                    settings.claude.clone(),
                );
                if plan_usage_settings != new_settings {
                    plan_usage_settings = new_settings;
                    this.sync_plan_usage_polling(cx);
                    cx.notify();
                }
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
                _subscriptions: vec![store_subscription, settings_subscription],
                history_tasks: HashMap::default(),
                history_caches: HashMap::default(),
                history_watchers: HashMap::default(),
                plan_usage: HashMap::default(),
                plan_usage_task: None,
                http_client,
                active: false,
            };
            let _ = window;
            panel
        })
    }

    fn sync_plan_usage_polling(&mut self, cx: &mut Context<Self>) {
        self.plan_usage_task.take();
        self.plan_usage.clear();
        if !self.active || !AgentThreadSettings::get_global(cx).show_plan_usage {
            return;
        }
        let settings = AgentThreadSettings::get_global(cx);
        let queries = self
            .visible_registry(cx)
            .into_iter()
            .map(|kind| (kind.id, settings.command_for_kind(kind.id).clone()))
            .collect::<Vec<_>>();
        let http_client = self.http_client.clone();
        self.plan_usage_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let tasks = queries.iter().map(|(kind_id, command)| {
                    let command = command.clone();
                    let http_client = http_client.clone();
                    let kind_id = *kind_id;
                    cx.background_spawn(async move {
                        (
                            kind_id,
                            query_plan_usage(kind_id, &command, http_client).await,
                        )
                    })
                });
                for (kind_id, result) in futures::future::join_all(tasks).await {
                    match result {
                        Ok(usage) => {
                            if this
                                .update(cx, |this, cx| {
                                    this.plan_usage.insert(kind_id, usage);
                                    cx.notify();
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(error) => {
                            log::warn!(
                                "agent_threads: failed to query {kind_id} plan usage: {error}"
                            );
                        }
                    }
                }
                cx.background_executor()
                    .timer(Duration::from_secs(5 * 60))
                    .await;
            }
        }));
    }

    fn handle_store_event(&mut self, event: &AgentThreadStoreEvent, cx: &mut Context<Self>) {
        if !self.active {
            return;
        }
        match event {
            AgentThreadStoreEvent::ThreadOpened { .. } => cx.notify(),
            AgentThreadStoreEvent::ThreadClosed { kind_id } => {
                cx.notify();
                // When a watcher is active for this kind it will catch the
                // closing session's final writes; only fall back to an explicit
                // rescan for kinds without one (e.g. remote projects).
                if !self.history_watchers.contains_key(kind_id) {
                    self.refresh_history_kind(kind_id, Some(Duration::from_millis(300)), cx);
                }
            }
        }
    }

    fn refresh_history(&mut self, cx: &mut Context<Self>) {
        for kind in self.visible_registry(cx) {
            self.refresh_history_kind(kind.id, None, cx);
        }
    }

    fn refresh_history_kind(
        &mut self,
        kind_id: &'static str,
        delay: Option<Duration>,
        cx: &mut Context<Self>,
    ) {
        if !self.active {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let project = workspace.read(cx).project().clone();
        let Some(kind) = self.registry.iter().find(|kind| kind.id == kind_id) else {
            return;
        };
        if AgentThreadSettings::get_global(cx)
            .command_for_kind(kind.id)
            .hidden
        {
            self.history_tasks.remove(kind_id);
            return;
        }
        let Some(provider) = kind.history_provider.clone() else {
            return;
        };
        // Keep any already-loaded list visible while rescanning so watch-driven
        // refreshes don't flash a spinner; only show "Loading" on a cold load.
        if let Some(section) = self.sections.get_mut(kind.id) {
            if !matches!(section.historical, HistoricalState::Loaded(_)) {
                section.historical = HistoricalState::Loading;
            }
        }
        let env_var = kind.home_env_var;
        let dir_name = kind.home_dir_name;
        let fs = self.fs.clone();
        let cache = self
            .history_caches
            .entry(kind_id)
            .or_insert_with(|| {
                Arc::new(HistoryParseCache::with_disk(
                    fs,
                    history_cache_path(kind_id),
                ))
            })
            .clone();
        let task = cx.spawn(async move |this, cx| {
            if let Some(delay) = delay {
                cx.background_executor().timer(delay).await;
            }
            let scan_result = async {
                let host =
                    history::resolve_history_host(&project, env_var, dir_name, cache, cx).await?;
                let project_roots =
                    project.read_with(cx, |project, cx| project_worktree_roots(project, cx));
                let threads = provider.scan(&host, &project_roots).await?;
                host.flush_cache().await.log_err();
                anyhow::Ok(threads)
            }
            .await;

            match scan_result {
                Ok(threads) => {
                    this.update(cx, |this, cx| {
                        if !this.active {
                            return;
                        }
                        if let Some(section) = this.sections.get_mut(kind_id) {
                            section.historical = HistoricalState::Loaded(threads.into());
                        }
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) => {
                    log::warn!("agent_threads: failed to scan {kind_id} history: {error:#}");
                    this.update(cx, |this, cx| {
                        if !this.active {
                            return;
                        }
                        if let Some(section) = this.sections.get_mut(kind_id) {
                            section.historical = HistoricalState::Unavailable;
                        }
                        cx.notify();
                    })
                    .ok();
                }
            }
        });
        self.history_tasks.insert(kind_id, task);
    }

    /// Starts a filesystem watcher for each visible kind on a local project so
    /// that new or updated history shows up without re-sweeping every file on
    /// each activation. No-op for kinds already watched or for remote projects.
    fn ensure_history_watches(&mut self, cx: &mut Context<Self>) {
        for kind in self.visible_registry(cx) {
            self.ensure_history_watch(kind.id, cx);
        }
    }

    fn ensure_history_watch(&mut self, kind_id: &'static str, cx: &mut Context<Self>) {
        if !self.active || self.history_watchers.contains_key(kind_id) {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let project = workspace.read(cx).project().clone();
        // Watching is local-only: the remote home dir lives outside the
        // worktree and there is no RPC to watch an arbitrary remote path.
        if project.read(cx).remote_client().is_some() {
            return;
        }
        let Some(kind) = self.registry.iter().find(|kind| kind.id == kind_id) else {
            return;
        };
        if kind.history_provider.is_none() {
            return;
        }
        let env_var = kind.home_env_var;
        let dir_name = kind.home_dir_name;
        let fs = self.fs.clone();
        let task = cx.spawn(async move |this, cx| {
            let Ok(base_dir) =
                history::resolve_history_base_dir(&project, env_var, dir_name, cx).await
            else {
                return;
            };
            let (mut events, _watcher) = fs.watch(&base_dir, HISTORY_WATCH_LATENCY).await;
            while events.next().await.is_some() {
                if this
                    .update(cx, |this, cx| this.refresh_history_kind(kind_id, None, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.history_watchers.insert(kind_id, task);
    }

    /// Registered agent kinds that the user hasn't hidden via
    /// `agent_threads.<kind>.hidden`.
    fn visible_registry(&self, cx: &App) -> Vec<AgentKindDefinition> {
        let settings = AgentThreadSettings::get_global(cx);
        self.registry
            .iter()
            .filter(|kind| !settings.command_for_kind(kind.id).hidden)
            .cloned()
            .collect()
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
            crate::launch_new_thread_with_default(workspace, kind, window, cx);
        });
    }

    fn deploy_new_thread_options_menu(
        &mut self,
        kind: AgentKindDefinition,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let resume_options = kind.resume_options.clone();
        let effective_label = match store::remembered_new_thread_launch_option(cx, kind.id) {
            Some(label) if label.is_empty() => None,
            Some(label) => Some(label),
            None => AgentThreadSettings::get_global(cx)
                .command_for_kind(kind.id)
                .default_launch_option
                .clone(),
        };
        let context_menu = ContextMenu::build(window, cx, move |mut context_menu, _, _| {
            {
                let workspace = workspace.clone();
                let kind = kind.clone();
                let is_selected = effective_label.is_none();
                context_menu = context_menu.toggleable_entry(
                    "New thread",
                    is_selected,
                    IconPosition::Start,
                    None,
                    move |window, cx| {
                        let Some(workspace) = workspace.upgrade() else {
                            return;
                        };
                        let kind = kind.clone();
                        store::remember_new_thread_launch_option(cx, kind.id, None);
                        workspace.update(cx, |workspace, cx| {
                            store::launch_new_thread(workspace, &kind, &[], window, cx);
                        });
                    },
                );
            }
            for option in resume_options {
                let workspace = workspace.clone();
                let kind = kind.clone();
                let label = SharedString::from(format!("New — {}", option.label));
                let args = option.args.clone();
                let is_selected = effective_label.as_deref() == Some(option.label.as_ref());
                let option_label = option.label.to_string();
                context_menu = context_menu.toggleable_entry(
                    label,
                    is_selected,
                    IconPosition::Start,
                    None,
                    move |window, cx| {
                        let Some(workspace) = workspace.upgrade() else {
                            return;
                        };
                        let kind = kind.clone();
                        let args = args.clone();
                        store::remember_new_thread_launch_option(
                            cx,
                            kind.id,
                            Some(option_label.clone()),
                        );
                        workspace.update(cx, |workspace, cx| {
                            store::launch_new_thread(workspace, &kind, &args, window, cx);
                        });
                    },
                );
            }
            context_menu
        });
        self.set_context_menu(context_menu, position, window, cx);
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
        let effective_label = match store::remembered_launch_option(cx, &thread.session_id) {
            Some(label) if label.is_empty() => None,
            Some(label) => Some(label),
            None => AgentThreadSettings::get_global(cx)
                .command_for_kind(kind.id)
                .default_launch_option
                .clone(),
        };
        let context_menu = ContextMenu::build(window, cx, move |mut context_menu, _, _| {
            {
                let workspace = workspace.clone();
                let kind = kind.clone();
                let thread = thread.clone();
                let is_selected = effective_label.is_none();
                context_menu = context_menu.toggleable_entry(
                    "Resume",
                    is_selected,
                    IconPosition::Start,
                    None,
                    move |window, cx| {
                        let Some(workspace) = workspace.upgrade() else {
                            return;
                        };
                        let kind = kind.clone();
                        let thread = thread.clone();
                        store::remember_launch_option(cx, thread.session_id.clone(), None);
                        workspace.update(cx, |workspace, cx| {
                            store::resume_thread(workspace, &kind, &thread, &[], window, cx);
                        });
                    },
                );
            }
            for option in resume_options {
                let workspace = workspace.clone();
                let kind = kind.clone();
                let thread = thread.clone();
                let label = SharedString::from(format!("Resume — {}", option.label));
                let args = option.args.clone();
                let is_selected = effective_label.as_deref() == Some(option.label.as_ref());
                let option_label = option.label.to_string();
                context_menu = context_menu.toggleable_entry(
                    label,
                    is_selected,
                    IconPosition::Start,
                    None,
                    move |window, cx| {
                        let Some(workspace) = workspace.upgrade() else {
                            return;
                        };
                        let kind = kind.clone();
                        let thread = thread.clone();
                        let args = args.clone();
                        store::remember_launch_option(
                            cx,
                            thread.session_id.clone(),
                            Some(option_label.clone()),
                        );
                        workspace.update(cx, |workspace, cx| {
                            store::resume_thread(workspace, &kind, &thread, &args, window, cx);
                        });
                    },
                );
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
        let usage = self.plan_usage.get(kind.id).copied();
        let live = self
            .store
            .read(cx)
            .live_threads_for_project(kind.id, project_roots);
        let section = self.sections.entry(kind.id).or_default();
        let collapsed = section.collapsed;
        let show_all = section.show_all;
        let (historical, scan_status) = match &section.historical {
            HistoricalState::Loaded(threads) => (Some(threads.clone()), None),
            HistoricalState::Loading => (None, Some("Scanning history…")),
            HistoricalState::Unavailable => (None, Some("Couldn't scan history")),
        };

        let rows = merge_threads(
            live,
            historical
                .iter()
                .flat_map(|threads| threads.iter().cloned()),
        );
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
                            .color(ui::brand_icon_color(kind.icon).unwrap_or(Color::Muted)),
                    )
                    .child(Label::new(kind.label.clone()).size(LabelSize::Small))
                    .child(
                        Label::new(total.to_string())
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .when_some(
                        usage.and_then(|u| u.five_hour_percent.map(|p| (p, u.five_hour_reset_at))),
                        |header, (percent, reset_at)| {
                            let label = match reset_at.and_then(format_reset_countdown) {
                                Some(t) => format!("5H:{}% {}", percent.value(), t),
                                None => format!("5H:{}%", percent.value()),
                            };
                            header.child(
                                Label::new(label)
                                    .size(LabelSize::XSmall)
                                    .color(usage_color(percent.value(), cx)),
                            )
                        },
                    )
                    .when_some(
                        usage.and_then(|u| u.weekly_percent.map(|p| (p, u.weekly_reset_at))),
                        |header, (percent, reset_at)| {
                            let label = match reset_at.and_then(format_reset_countdown) {
                                Some(t) => format!("W:{}% {}", percent.value(), t),
                                None => format!("W:{}%", percent.value()),
                            };
                            header.child(
                                Label::new(label)
                                    .size(LabelSize::XSmall)
                                    .color(usage_color(percent.value(), cx)),
                            )
                        },
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
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
                    )
                    .child(
                        IconButton::new(
                            SharedString::from(format!("agent-thread-new-options-{kind_id}")),
                            IconName::ChevronDown,
                        )
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text(format!("New {} thread options", kind.label)))
                        .on_click(cx.listener({
                            let kind = kind.clone();
                            move |this, event: &gpui::ClickEvent, window, cx| {
                                this.deploy_new_thread_options_menu(
                                    kind.clone(),
                                    event.position(),
                                    window,
                                    cx,
                                );
                            }
                        })),
                    ),
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
            AgentThreadRow::FreshLive(metadata) => self.render_live_row(metadata, cx),
            AgentThreadRow::Historical {
                thread,
                live_terminal_item_id,
            } => self.render_historical_row(kind, thread, live_terminal_item_id, cx),
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
            .child(
                Icon::new(IconName::Circle)
                    .size(IconSize::Indicator)
                    .color(Color::Success),
            )
            .child(
                Label::new(metadata.title)
                    .size(LabelSize::Small)
                    .color(Color::Success)
                    .truncate(),
            )
            .into_any_element()
    }

    fn render_historical_row(
        &mut self,
        kind: &AgentKindDefinition,
        thread: HistoricalThread,
        live_terminal_item_id: Option<gpui::EntityId>,
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
        let is_live = live_terminal_item_id.is_some();
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
            .when_some(live_terminal_item_id, |row, terminal_item_id| {
                row.on_click(cx.listener(move |this, _, window, cx| {
                    this.focus_live_thread(terminal_item_id, window, cx);
                }))
            })
            .when(!is_live, |row| {
                row.on_click(cx.listener(move |this, _, window, cx| {
                    let args = store::resolve_thread_launch_args(
                        cx,
                        &click_kind,
                        &click_thread.session_id,
                    );
                    this.resume(&click_kind, &click_thread, &args, window, cx);
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
            })
            .child(
                h_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_2()
                    .child(
                        Icon::new(if is_live {
                            IconName::Circle
                        } else {
                            IconName::HistoryRerun
                        })
                        .size(if is_live {
                            IconSize::Indicator
                        } else {
                            IconSize::Small
                        })
                        .color(if is_live {
                            Color::Success
                        } else {
                            Color::Muted
                        }),
                    )
                    .child(
                        Label::new(thread.title)
                            .size(LabelSize::Small)
                            .color(if is_live {
                                Color::Success
                            } else {
                                Color::Muted
                            })
                            .truncate(),
                    ),
            )
            .when(!is_live, |row| {
                row.child(
                    IconButton::new(options_button_id, IconName::Ellipsis)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Resume with options"))
                        .on_click(cx.listener(
                            move |this, event: &gpui::ClickEvent, window, cx| {
                                this.deploy_resume_options_menu(
                                    menu_kind_for_button.clone(),
                                    menu_thread_for_button.clone(),
                                    event.position(),
                                    window,
                                    cx,
                                );
                            },
                        )),
                )
            })
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

    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            self.sync_plan_usage_polling(cx);
            cx.spawn(async move |this: WeakEntity<Self>, cx| {
                this.update(cx, |this, cx| {
                    this.refresh_history(cx);
                    this.ensure_history_watches(cx);
                })
                .ok();
            })
            .detach();
            cx.notify();
        } else {
            self.sync_plan_usage_polling(cx);
            self.history_tasks.clear();
            self.history_watchers.clear();
        }
    }
}

impl Render for AgentThreadsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return div().size_full().into_any_element();
        };
        let project = workspace.read(cx).project().clone();
        let project_roots = project_worktree_roots(project.read(cx), cx);

        let registry = self.visible_registry(cx);
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
    use std::sync::LazyLock;
    use terminal_view::TerminalView;
    use workspace::MultiWorkspace;

    // Tests that actually spawn the echo command need a `cwd` that exists on
    // disk: a fake "/root" works for FakeFs-only assertions, but on Windows
    // there's no such absolute path, so the real PTY/process spawn fails
    // before the terminal is ever registered.
    static SPAWNING_TEST_ROOT: LazyLock<String> =
        LazyLock::new(|| std::env::temp_dir().to_string_lossy().into_owned());

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            // Give each test its own in-memory database. Without this, the
            // launch-option `db::kvp` reads/writes fall back to a single
            // process-wide test database whose shared write queue lets writes
            // from concurrently-running tests race each other.
            cx.set_global(db::AppDatabase::test_new());
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
                    show_plan_usage: None,
                    dock: None,
                });
            });
        });
    }

    fn set_show_plan_usage(cx: &mut TestAppContext, enabled: bool) {
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings
                    .agent_threads
                    .get_or_insert_default()
                    .show_plan_usage = Some(enabled);
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
            hidden: None,
            default_launch_option: None,
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
                    crate::launch_new_thread_with_default(workspace, &codex_kind(), window, cx);
                });
            })
            .expect("failed to launch codex thread");
    }

    fn set_default_launch_option(
        cx: &mut TestAppContext,
        kind_id: &'static str,
        option: Option<&str>,
    ) {
        let option = option.map(str::to_string);
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                let content = settings.agent_threads.get_or_insert_default();
                let command = match kind_id {
                    "codex" => content.codex.get_or_insert_default(),
                    "claude" => content.claude.get_or_insert_default(),
                    _ => panic!("unknown kind_id {kind_id}"),
                };
                command.default_launch_option = option;
            });
        });
    }

    fn set_agent_hidden(cx: &mut TestAppContext, kind_id: &'static str, hidden: bool) {
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                let content = settings.agent_threads.get_or_insert_default();
                let command = match kind_id {
                    "codex" => content.codex.get_or_insert_default(),
                    "claude" => content.claude.get_or_insert_default(),
                    _ => panic!("unknown kind_id {kind_id}"),
                };
                command.hidden = Some(hidden);
            });
        });
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
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root, 5);
        let window_handle = init_workspace(cx, root).await;

        launch_codex_thread(&window_handle, cx);
        wait_for_live_count(cx, root, 1).await;

        let metadata = live_codex_threads(cx, root);
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
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root, 5);
        let window_handle = init_workspace(cx, root).await;

        launch_codex_thread(&window_handle, cx);
        wait_for_live_count(cx, root, 1).await;

        let terminal_item_id = live_codex_threads(cx, root)[0].terminal_item_id;
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

    #[gpui::test]
    async fn hiding_an_agent_excludes_it_from_visible_registry(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root, 5);
        let window_handle = init_workspace(cx, root).await;

        let panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    AgentThreadsPanel::new(workspace, window, cx)
                })
            })
            .expect("failed to create panel");
        cx.run_until_parked();

        fn visible_ids(
            panel: &Entity<AgentThreadsPanel>,
            cx: &mut TestAppContext,
        ) -> Vec<&'static str> {
            panel.read_with(cx, |panel, cx| {
                panel
                    .visible_registry(cx)
                    .iter()
                    .map(|kind| kind.id)
                    .collect()
            })
        }

        assert_eq!(visible_ids(&panel, cx), vec!["codex", "claude"]);

        set_agent_hidden(cx, "codex", true);
        assert_eq!(visible_ids(&panel, cx), vec!["claude"]);

        set_agent_hidden(cx, "codex", false);
        assert_eq!(visible_ids(&panel, cx), vec!["codex", "claude"]);
    }

    #[gpui::test]
    async fn launching_a_new_thread_uses_the_persisted_default_launch_option(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        init_test(cx);
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root, 5);
        set_default_launch_option(cx, "codex", Some("Bypass approvals & sandbox"));
        let window_handle = init_workspace(cx, root).await;

        launch_codex_thread(&window_handle, cx);
        wait_for_live_count(cx, root, 1).await;
        wait_for_terminal_view_count(&window_handle, cx, 1).await;

        let args = terminal_views(&window_handle, cx)[0].read_with(cx, |view, cx| {
            view.terminal()
                .read(cx)
                .task()
                .expect("spawned terminal should have a task")
                .spawned_task
                .args
                .clone()
        });

        assert!(
            args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()),
            "expected default launch option's args in {args:?}"
        );
    }

    #[gpui::test]
    async fn remembered_per_thread_option_overrides_the_agent_default(cx: &mut TestAppContext) {
        init_test(cx);
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root, 5);
        set_default_launch_option(cx, "codex", Some("Bypass approvals & sandbox"));

        let kind = codex_kind();
        let session_id = SharedString::from("session-remember");

        // No per-thread choice recorded yet -> falls back to the agent default.
        let args = cx.update(|cx| store::resolve_thread_launch_args(cx, &kind, &session_id));
        assert_eq!(
            args,
            vec!["--dangerously-bypass-approvals-and-sandbox".to_string()]
        );

        // Explicitly choosing "plain resume" for this thread takes priority
        // over the agent-wide default.
        cx.update(|cx| store::remember_launch_option(cx, session_id.clone(), None));
        cx.run_until_parked();
        let args = cx.update(|cx| store::resolve_thread_launch_args(cx, &kind, &session_id));
        assert!(args.is_empty(), "expected no extra args, got {args:?}");
    }

    #[gpui::test]
    async fn remembered_new_thread_option_overrides_the_agent_default(cx: &mut TestAppContext) {
        init_test(cx);
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root, 5);
        set_default_launch_option(cx, "codex", Some("Bypass approvals & sandbox"));

        let kind = codex_kind();

        // No new-thread dropdown choice recorded yet -> falls back to the
        // agent default.
        let args = cx.update(|cx| store::resolve_new_thread_launch_args(cx, &kind));
        assert_eq!(
            args,
            vec!["--dangerously-bypass-approvals-and-sandbox".to_string()]
        );

        // Picking "New thread" (plain) from the dropdown takes priority over
        // the agent-wide default.
        cx.update(|cx| store::remember_new_thread_launch_option(cx, kind.id, None));
        cx.run_until_parked();
        let args = cx.update(|cx| store::resolve_new_thread_launch_args(cx, &kind));
        assert!(args.is_empty(), "expected no extra args, got {args:?}");
    }

    #[test]
    fn apply_visible_cap_truncates_until_show_all_is_set() {
        let rows = vec![
            AgentThreadRow::Historical {
                thread: HistoricalThread {
                    session_id: SharedString::from("a"),
                    title: SharedString::from("a"),
                    project_root: PathBuf::from("/root"),
                    last_activity_at: std::time::SystemTime::UNIX_EPOCH,
                },
                live_terminal_item_id: None,
            },
            AgentThreadRow::Historical {
                thread: HistoricalThread {
                    session_id: SharedString::from("b"),
                    title: SharedString::from("b"),
                    project_root: PathBuf::from("/root"),
                    last_activity_at: std::time::SystemTime::UNIX_EPOCH,
                },
                live_terminal_item_id: None,
            },
            AgentThreadRow::Historical {
                thread: HistoricalThread {
                    session_id: SharedString::from("c"),
                    title: SharedString::from("c"),
                    project_root: PathBuf::from("/root"),
                    last_activity_at: std::time::SystemTime::UNIX_EPOCH,
                },
                live_terminal_item_id: None,
            },
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
    async fn plan_usage_polling_only_exists_while_panel_is_active(cx: &mut TestAppContext) {
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

        panel.update(cx, |panel, cx| {
            assert!(panel.plan_usage_task.is_none());
            panel.active = true;
            panel.sync_plan_usage_polling(cx);
            assert!(panel.plan_usage_task.is_some());
            panel.active = false;
            panel.sync_plan_usage_polling(cx);
            assert!(panel.plan_usage_task.is_none());
        });
    }

    #[gpui::test]
    async fn disabled_plan_usage_does_not_start_polling(cx: &mut TestAppContext) {
        init_test(cx);
        configure_echo_threads(cx, "/root", 5);
        set_show_plan_usage(cx, false);
        let window_handle = init_workspace(cx, "/root").await;
        let panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    AgentThreadsPanel::new(workspace, window, cx)
                })
            })
            .expect("failed to create panel");

        panel.update(cx, |panel, cx| {
            panel.active = true;
            panel.sync_plan_usage_polling(cx);
            assert!(panel.plan_usage_task.is_none());
        });
    }

    #[gpui::test]
    async fn resume_spawns_a_terminal_with_the_resume_command(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root, 5);
        let window_handle = init_workspace(cx, root).await;

        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("Fix the bug"),
            project_root: PathBuf::from(root),
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
        assert_eq!(spawned.cwd, Some(PathBuf::from(root)));
        assert_eq!(spawned.full_label, "Fix the bug");
    }

    #[gpui::test]
    async fn resume_with_options_appends_the_extra_flag(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        init_test(cx);
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root, 5);
        let window_handle = init_workspace(cx, root).await;

        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("Fix the bug"),
            project_root: PathBuf::from(root),
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
