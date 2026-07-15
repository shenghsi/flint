use auto_update::{AutoUpdater, release_notes_url};
use editor::{Editor, MultiBuffer};
use flint_actions::ShowUpdateNotification;
use gpui::{App, DismissEvent, Entity, TaskExt, Window, actions, prelude::*};
use markdown_preview::markdown_preview_view::{MarkdownPreviewMode, MarkdownPreviewView};
use release_channel::{AppVersion, ReleaseChannel};
use util::{ResultExt as _, maybe};
use workspace::{
    Workspace,
    notifications::{
        ErrorMessagePrompt, NotificationId, show_app_notification,
        simple_message_notification::MessageNotification,
    },
};

actions!(
    auto_update,
    [
        /// Opens the release notes for the current version in a new tab.
        ViewReleaseNotesLocally
    ]
);

pub fn init(cx: &mut App) {
    notify_if_app_was_updated(cx);
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        workspace.register_action(|workspace, _: &ViewReleaseNotesLocally, window, cx| {
            view_release_notes_locally(workspace, window, cx);
        });

        if matches!(
            ReleaseChannel::global(cx),
            ReleaseChannel::Nightly | ReleaseChannel::Dev
        ) {
            workspace.register_action(|_workspace, _: &ShowUpdateNotification, _window, cx| {
                show_update_notification(cx);
            });
        }
    })
    .detach();
}

fn notify_release_notes_failed_to_show(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    struct ViewReleaseNotesError;
    workspace.show_notification(
        NotificationId::unique::<ViewReleaseNotesError>(),
        cx,
        |cx| {
            cx.new(move |cx| {
                let url = release_notes_url(cx);
                let mut prompt = ErrorMessagePrompt::new("Couldn't load release notes", cx);
                if let Some(url) = url {
                    prompt = prompt.with_link_button("View in Browser".to_string(), url);
                }
                prompt
            })
        },
    );
}

fn view_release_notes_locally(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let release_channel = ReleaseChannel::global(cx);

    if matches!(
        release_channel,
        ReleaseChannel::Nightly | ReleaseChannel::Dev
    ) {
        if let Some(url) = release_notes_url(cx) {
            cx.open_url(&url);
        }
        return;
    }

    let mut version = AppVersion::global(cx).to_string();
    if let Ok(mut semantic_version) = version.parse::<semver::Version>() {
        semantic_version.pre = semver::Prerelease::EMPTY;
        semantic_version.build = semver::BuildMetadata::EMPTY;
        version = semantic_version.to_string();
    }
    let tag = format!("v{version}");
    let http_client = cx.http_client();

    let markdown = workspace
        .app_state()
        .languages
        .language_for_name("Markdown");

    cx.spawn_in(window, async move |workspace, cx| {
        let markdown = markdown.await.log_err();
        let release =
            http_client::github::get_release_by_tag_name("shenghsi/flint", &tag, http_client).await;
        let Some(release) = release.log_err() else {
            workspace
                .update_in(cx, notify_release_notes_failed_to_show)
                .log_err();
            return;
        };
        let title = release.name.unwrap_or(release.tag_name);
        let Some(release_notes) = release.body else {
            workspace
                .update_in(cx, notify_release_notes_failed_to_show)
                .log_err();
            return;
        };

        let res: Option<()> = maybe!(async {
            let project = workspace
                .read_with(cx, |workspace, _| workspace.project().clone())
                .ok()?;
            let (language_registry, buffer) = project.update(cx, |project, cx| {
                (
                    project.languages().clone(),
                    project.create_buffer(markdown, false, cx),
                )
            });
            let buffer = buffer.await.ok()?;
            buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..0, release_notes)], None, cx)
            });

            let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx).with_title(title));

            let ws_handle = workspace.clone();
            workspace
                .update_in(cx, |workspace, window, cx| {
                    let editor =
                        cx.new(|cx| Editor::for_multibuffer(buffer, Some(project), window, cx));
                    let markdown_preview: Entity<MarkdownPreviewView> = MarkdownPreviewView::new(
                        MarkdownPreviewMode::Default,
                        editor,
                        ws_handle,
                        language_registry,
                        window,
                        cx,
                    );
                    workspace.add_item_to_active_pane(
                        Box::new(markdown_preview),
                        None,
                        true,
                        window,
                        cx,
                    );
                    cx.notify();
                })
                .ok()
        })
        .await;
        if res.is_none() {
            workspace
                .update_in(cx, notify_release_notes_failed_to_show)
                .log_err();
        }
    })
    .detach();
}

struct UpdateNotification;

fn show_update_notification(cx: &mut App) {
    let Some(updater) = AutoUpdater::get(cx) else {
        return;
    };

    let mut version = updater.read(cx).current_version();
    version.pre = semver::Prerelease::EMPTY;
    version.build = semver::BuildMetadata::EMPTY;
    let app_name = ReleaseChannel::global(cx).display_name();

    show_app_notification(
        NotificationId::unique::<UpdateNotification>(),
        cx,
        move |cx| {
            let workspace_handle = cx.entity().downgrade();
            cx.new(|cx| {
                MessageNotification::new(format!("Updated to {app_name} {}", version), cx)
                    .primary_message("View Release Notes")
                    .primary_on_click(move |window, cx| {
                        if let Some(workspace) = workspace_handle.upgrade() {
                            workspace.update(cx, |workspace, cx| {
                                crate::view_release_notes_locally(workspace, window, cx);
                            })
                        }
                        cx.emit(DismissEvent);
                    })
                    .show_suppress_button(false)
            })
        },
    );
}

/// Shows a notification across all workspaces if an update was previously automatically installed
/// and this notification had not yet been shown.
pub fn notify_if_app_was_updated(cx: &mut App) {
    let Some(updater) = AutoUpdater::get(cx) else {
        return;
    };

    if let ReleaseChannel::Nightly = ReleaseChannel::global(cx) {
        return;
    }

    let should_show_notification = updater.read(cx).should_show_update_notification(cx);

    cx.spawn(async move |cx| {
        let should_show_notification = should_show_notification.await?;

        if should_show_notification {
            cx.update(|cx| {
                show_update_notification(cx);
                updater.update(cx, |updater, cx| {
                    updater
                        .set_should_show_update_notification(false, cx)
                        .detach_and_log_err(cx);
                });
            });
        }
        anyhow::Ok(())
    })
    .detach();
}
