use collections::HashSet;
use git::Oid;
use git::repository::{InitialGraphCommitData, LogOrder, LogSource, SearchCommitArgs};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Task, Window, rems,
};
use picker::{Picker, PickerDelegate};
use project::git_store::{CommitDataState, Repository, RepositoryEvent};
use std::sync::Arc;
use time::OffsetDateTime;
use ui::{ListItem, ListItemSpacing, prelude::*};
use util::ResultExt;
use workspace::ModalView;

/// Caps how many commits are shown for a query, so the picker stays
/// responsive on repositories with very large histories.
const MAX_MATCHES: usize = 200;

pub type SelectCommitCallback = Arc<dyn Fn(Oid, &mut Window, &mut App)>;

pub fn select_modal(
    repository: Entity<Repository>,
    on_select: SelectCommitCallback,
    window: &mut Window,
    cx: &mut Context<CommitPicker>,
) -> CommitPicker {
    let picker = CommitPicker::new(repository, on_select, window, cx);
    picker.focus_handle(cx).focus(window, cx);
    picker
}

pub struct CommitPicker {
    pub picker: Entity<Picker<CommitPickerDelegate>>,
    picker_focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl CommitPicker {
    fn new(
        repository: Entity<Repository>,
        on_select: SelectCommitCallback,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let log_source = LogSource::All;
        let log_order = LogOrder::default();

        // Reuses the same cached graph data the Git History panel populates, so if
        // history was already loaded there this is instant; otherwise it kicks off
        // the same background load, which streams in via `RepositoryEvent::GraphEvent`.
        let all_commits = repository.update(cx, |repository, cx| {
            repository
                .graph_data(log_source.clone(), log_order, 0..usize::MAX, cx)
                .commits
                .to_vec()
        });

        let delegate = CommitPickerDelegate::new(
            repository.clone(),
            on_select,
            log_source.clone(),
            log_order,
            all_commits,
            cx,
        );
        let picker = cx.new(|cx| {
            Picker::uniform_list(delegate, window, cx)
                .show_scrollbar(true)
                .modal(true)
        });
        let picker_focus_handle = picker.focus_handle(cx);
        picker.update(cx, |picker, _| {
            picker.delegate.focus_handle = picker_focus_handle.clone();
        });

        let mut _subscriptions = vec![cx.subscribe_in(&repository, window, {
            let log_source = log_source.clone();
            move |this, repository, event, window, cx| {
                let RepositoryEvent::GraphEvent((source, order), _) = event else {
                    return;
                };
                if source != &log_source || *order != log_order {
                    return;
                }
                let all_commits = repository.update(cx, |repository, cx| {
                    repository
                        .graph_data(log_source.clone(), log_order, 0..usize::MAX, cx)
                        .commits
                        .to_vec()
                });
                this.picker.update(cx, |picker, cx| {
                    picker.delegate.all_commits = all_commits;
                    picker.refresh(window, cx);
                });
            }
        })];

        _subscriptions.push(cx.subscribe(&picker, |_, _, _, cx| {
            cx.emit(DismissEvent);
        }));

        Self {
            picker,
            picker_focus_handle,
            _subscriptions,
        }
    }
}

impl ModalView for CommitPicker {}
impl EventEmitter<DismissEvent> for CommitPicker {}

impl Focusable for CommitPicker {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.picker_focus_handle.clone()
    }
}

impl Render for CommitPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("CommitPicker")
            .w(rems(34.))
            .child(self.picker.clone())
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.picker.update(cx, |this, cx| {
                    this.cancel(&Default::default(), window, cx);
                })
            }))
    }
}

pub struct CommitPickerDelegate {
    repository: Entity<Repository>,
    on_select: SelectCommitCallback,
    log_source: LogSource,
    all_commits: Vec<Arc<InitialGraphCommitData>>,
    matches: Vec<Arc<InitialGraphCommitData>>,
    selected_index: usize,
    focus_handle: FocusHandle,
}

impl CommitPickerDelegate {
    fn new(
        repository: Entity<Repository>,
        on_select: SelectCommitCallback,
        log_source: LogSource,
        _log_order: LogOrder,
        all_commits: Vec<Arc<InitialGraphCommitData>>,
        cx: &mut Context<CommitPicker>,
    ) -> Self {
        Self {
            repository,
            on_select,
            log_source,
            all_commits,
            matches: Vec::new(),
            selected_index: 0,
            focus_handle: cx.focus_handle(),
        }
    }
}

impl PickerDelegate for CommitPickerDelegate {
    type ListItem = ListItem;

    fn placeholder_text(&self, _window: &mut Window, cx: &mut App) -> Arc<str> {
        localization::text(cx, "git-select-commit")
            .to_string()
            .into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let all_commits = self.all_commits.clone();
        let repository = self.repository.clone();
        let log_source = self.log_source.clone();

        cx.spawn_in(window, async move |picker, cx| {
            let matches: Vec<Arc<InitialGraphCommitData>> = if query.is_empty() {
                all_commits.into_iter().take(MAX_MATCHES).collect()
            } else {
                let query_lower = query.to_lowercase();
                let sha_matches: HashSet<Oid> = all_commits
                    .iter()
                    .filter(|commit| commit.sha.to_string().starts_with(&query_lower))
                    .map(|commit| commit.sha)
                    .collect();

                // Message search runs server-side (`git log --grep`) against full
                // history, the same mechanism the Git History panel's search box uses.
                let (tx, rx) = async_channel::unbounded::<Oid>();
                repository.update(cx, |repository, cx| {
                    repository.search_commits(
                        log_source.clone(),
                        SearchCommitArgs {
                            query: query.clone().into(),
                            case_sensitive: false,
                        },
                        tx,
                        cx,
                    );
                });

                let mut message_matches = HashSet::default();
                while let Ok(sha) = rx.recv().await {
                    message_matches.insert(sha);
                }

                all_commits
                    .into_iter()
                    .filter(|commit| {
                        sha_matches.contains(&commit.sha) || message_matches.contains(&commit.sha)
                    })
                    .take(MAX_MATCHES)
                    .collect()
            };

            picker
                .update(cx, |picker, cx| {
                    let delegate = &mut picker.delegate;
                    delegate.matches = matches;
                    delegate.selected_index = 0;
                    cx.notify();
                })
                .log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(commit) = self.matches.get(self.selected_index()) else {
            return;
        };
        (self.on_select)(commit.sha, window, cx);
        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let commit = self.matches.get(ix)?.clone();
        let sha = commit.sha;

        // Fetches (and caches) commit details lazily, only for rows the virtualized
        // list actually renders — the same lazy-loading `Repository` cache and
        // `CommitDataState` the Git History panel uses for its own rows.
        let data = self.repository.update(cx, |repository, cx| {
            repository.fetch_commit_data(sha, false, cx).clone()
        });

        let (subject, author_name, formatted_time) = match &data {
            CommitDataState::Loaded(data) => {
                let commit_time = OffsetDateTime::from_unix_timestamp(data.commit_timestamp)
                    .unwrap_or_else(|_| OffsetDateTime::now_utc());
                let local_offset =
                    time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
                let formatted_time = time_format::format_localized_timestamp_for_language(
                    commit_time,
                    OffsetDateTime::now_utc(),
                    local_offset,
                    time_format::TimestampFormat::Relative,
                    localization::language(cx),
                );
                (
                    data.subject.clone(),
                    Some(data.author_name.clone()),
                    Some(formatted_time),
                )
            }
            CommitDataState::Loading(_) => (SharedString::from("Loading…"), None, None),
        };

        let dot = || {
            Label::new("•")
                .alpha(0.5)
                .color(Color::Muted)
                .size(LabelSize::Small)
        };

        Some(
            ListItem::new(("commit-picker-entry", ix))
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    h_flex()
                        .w_full()
                        .gap_2p5()
                        .child(
                            Icon::new(IconName::GitCommit)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .child(
                            v_flex()
                                .w_full()
                                .min_w_0()
                                .child(Label::new(subject).single_line().truncate())
                                .child(
                                    h_flex()
                                        .gap_1p5()
                                        .child(
                                            Label::new(sha.display_short())
                                                .color(Color::Muted)
                                                .size(LabelSize::Small),
                                        )
                                        .when_some(author_name, |this, author| {
                                            this.child(dot()).child(
                                                Label::new(author)
                                                    .color(Color::Muted)
                                                    .size(LabelSize::Small),
                                            )
                                        })
                                        .when_some(formatted_time, |this, time| {
                                            this.child(dot()).child(
                                                Label::new(time)
                                                    .color(Color::Muted)
                                                    .size(LabelSize::Small),
                                            )
                                        }),
                                ),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git::repository::CommitData;
    use gpui::TestAppContext;
    use project::{FakeFs, Project};
    use serde_json::json;
    use settings::SettingsStore;
    use smallvec::smallvec;
    use std::path::Path;
    use std::str::FromStr;
    use std::sync::Mutex;
    use util::path;
    use workspace::MultiWorkspace;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            localization::init(localization::UiLanguage::English, cx)
                .expect("test localization must load");
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
    }

    fn commit(sha: &str, subject: &str) -> (Arc<InitialGraphCommitData>, CommitData) {
        let oid = Oid::from_str(sha).unwrap();
        let graph_commit = Arc::new(InitialGraphCommitData {
            sha: oid,
            parents: smallvec![],
            ref_names: Vec::new(),
        });
        let commit_data = CommitData {
            sha: oid,
            parents: smallvec![],
            author_name: "Author".into(),
            author_email: "author@example.com".into(),
            commit_timestamp: 1_700_000_000,
            subject: subject.into(),
            message: subject.into(),
        };
        (graph_commit, commit_data)
    }

    #[gpui::test]
    async fn test_commit_picker_filters_and_selects(cx: &mut TestAppContext) {
        init_test(cx);

        let (commit_a_graph, commit_a_data) = commit(&"a".repeat(40), "Fix login bug");
        let (commit_b_graph, commit_b_data) = commit(&"b".repeat(40), "Add logout button");

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({
                ".git": {},
                "a.txt": "content",
            }),
        )
        .await;
        fs.set_graph_commits(
            Path::new(path!("/project/.git")),
            vec![commit_a_graph.clone(), commit_b_graph.clone()],
        );
        fs.set_commit_data(
            Path::new(path!("/project/.git")),
            [(commit_a_data, false), (commit_b_data, false)],
        );

        let project = Project::test(fs.clone(), [path!("/project").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
        cx.run_until_parked();

        let repository = project
            .read_with(cx, |project, cx| project.active_repository(cx))
            .expect("should have a repository");

        let selected: Arc<Mutex<Option<Oid>>> = Arc::new(Mutex::new(None));
        let on_select = {
            let selected = selected.clone();
            Arc::new(move |sha: Oid, _window: &mut Window, _cx: &mut App| {
                *selected.lock().unwrap() = Some(sha);
            })
        };

        let commit_picker = workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_modal(window, cx, {
                let repository = repository.clone();
                move |window, cx| select_modal(repository, on_select, window, cx)
            });
            workspace.active_modal::<CommitPicker>(cx).unwrap()
        });
        cx.run_until_parked();

        commit_picker.update(cx, |commit_picker, cx| {
            assert_eq!(commit_picker.picker.read(cx).delegate.matches.len(), 2);
        });

        // Filters via the (faked) server-side commit message search.
        commit_picker
            .update_in(cx, |commit_picker, window, cx| {
                commit_picker.picker.update(cx, |picker, cx| {
                    picker.delegate.update_matches("logout".into(), window, cx)
                })
            })
            .await;
        cx.run_until_parked();

        commit_picker.update(cx, |commit_picker, cx| {
            let matches = &commit_picker.picker.read(cx).delegate.matches;
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].sha, commit_b_graph.sha);
        });

        commit_picker.update_in(cx, |commit_picker, window, cx| {
            commit_picker.picker.update(cx, |picker, cx| {
                picker.delegate.confirm(false, window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(*selected.lock().unwrap(), Some(commit_b_graph.sha));
        workspace.update(cx, |workspace, cx| {
            assert!(workspace.active_modal::<CommitPicker>(cx).is_none());
        });
    }
}
