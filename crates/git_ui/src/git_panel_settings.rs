use collections::HashMap;
use editor::{EditorSettings, ui_scrollbar_settings_from_raw};
use gpui::Pixels;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::{GitPanelGroupBy, GitPanelSortBy, RegisterSetting, Settings, StatusStyle};
use std::{path::PathBuf, time::Duration};
use ui::{
    px,
    scrollbars::{ScrollbarVisibility, ShowScrollbar},
};
use workspace::dock::DockPosition;

#[derive(Copy, Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScrollbarSettings {
    pub show: Option<ShowScrollbar>,
}

#[derive(Debug, Clone, PartialEq, RegisterSetting)]
pub struct GitPanelSettings {
    pub button: bool,
    pub dock: DockPosition,
    pub default_width: Pixels,
    pub status_style: StatusStyle,
    pub file_icons: bool,
    pub folder_icons: bool,
    pub scrollbar: ScrollbarSettings,
    pub fallback_branch_name: String,
    pub sort_by: GitPanelSortBy,
    pub group_by: GitPanelGroupBy,
    pub collapse_untracked_diff: bool,
    pub tree_view: bool,
    pub diff_stats: bool,
    pub show_count_badge: bool,
    pub starts_open: bool,
    pub commit_title_max_length: usize,
}

#[derive(Clone, Debug, PartialEq, RegisterSetting)]
pub struct CommitMessageGeneratorSettings {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    pub max_diff_bytes: usize,
    pub instructions: Option<String>,
}

impl CommitMessageGeneratorSettings {
    pub fn is_configured(&self) -> bool {
        self.command
            .as_ref()
            .is_some_and(|command| !command.trim().is_empty())
    }

    pub fn command_label(&self) -> Option<String> {
        self.command.as_ref().map(|command| {
            std::iter::once(command.as_str())
                .chain(self.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        })
    }
}

#[derive(Default)]
pub(crate) struct GitPanelScrollbarAccessor;

impl ScrollbarVisibility for GitPanelScrollbarAccessor {
    fn visibility(&self, cx: &ui::App) -> ShowScrollbar {
        // TODO: This PR should have defined Editor's `scrollbar.axis`
        // as an Option<ScrollbarAxis>, not a ScrollbarAxes as it would allow you to
        // `.unwrap_or(EditorSettings::get_global(cx).scrollbar.show)`.
        //
        // Once this is fixed we can extend the GitPanelSettings with a `scrollbar.axis`
        // so we can show each axis based on the settings.
        //
        // We should fix this. PR: https://github.com/zed-industries/flint/pull/19495
        GitPanelSettings::get_global(cx)
            .scrollbar
            .show
            .unwrap_or_else(|| EditorSettings::get_global(cx).scrollbar.show)
    }
}

impl Settings for GitPanelSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let git_panel = content.git_panel.clone().unwrap();
        Self {
            button: git_panel.button.unwrap(),
            dock: git_panel.dock.unwrap().into(),
            default_width: px(git_panel.default_width.unwrap()),
            status_style: git_panel.status_style.unwrap(),
            file_icons: git_panel.file_icons.unwrap(),
            folder_icons: git_panel.folder_icons.unwrap(),
            scrollbar: ScrollbarSettings {
                show: git_panel
                    .scrollbar
                    .unwrap()
                    .show
                    .map(ui_scrollbar_settings_from_raw),
            },
            fallback_branch_name: git_panel.fallback_branch_name.unwrap(),
            sort_by: git_panel.sort_by.unwrap(),
            group_by: git_panel.group_by.unwrap(),
            collapse_untracked_diff: git_panel.collapse_untracked_diff.unwrap(),
            tree_view: git_panel.tree_view.unwrap(),
            diff_stats: git_panel.diff_stats.unwrap(),
            show_count_badge: git_panel.show_count_badge.unwrap(),
            starts_open: git_panel.starts_open.unwrap(),
            commit_title_max_length: git_panel.commit_title_max_length.unwrap(),
        }
    }
}

impl Settings for CommitMessageGeneratorSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let generator = content
            .git
            .as_ref()
            .and_then(|git| git.commit_message_generator.clone())
            .unwrap_or_default();

        Self {
            command: generator.command,
            args: generator.args.unwrap_or_default(),
            env: generator.env.unwrap_or_default().into_iter().collect(),
            cwd: generator.cwd,
            timeout: Duration::from_secs(generator.timeout_seconds.unwrap_or(30).max(1)),
            max_diff_bytes: generator.max_diff_bytes.unwrap_or(20_000).max(1),
            instructions: generator.instructions,
        }
    }
}
