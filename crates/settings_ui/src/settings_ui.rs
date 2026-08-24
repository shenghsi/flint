mod components;
mod page_data;
pub mod pages;

use anyhow::{Context as _, Result};
use editor::{Editor, EditorEvent};
use futures::{StreamExt, channel::mpsc};
use fuzzy::StringMatchCandidate;
use gpui::{
    Action, App, AsyncApp, ClipboardItem, DEFAULT_ADDITIONAL_WINDOW_SIZE, Div, Entity, FocusHandle,
    Focusable, Global, KeyContext, ListState, ReadGlobal as _, ScrollHandle, Stateful,
    Subscription, Task, Tiling, TitlebarOptions, UniformListScrollHandle, WeakEntity, Window,
    WindowBounds, WindowHandle, WindowOptions, actions, div, list, point, prelude::*, px,
    uniform_list,
};

use language::Buffer;
use platform_title_bar::PlatformTitleBar;
use project::{Project, ProjectPath, Worktree, WorktreeId};
use release_channel::ReleaseChannel;
use schemars::JsonSchema;
use serde::Deserialize;
use settings::{
    IntoGpui, Settings, SettingsContent, SettingsStore, UiLanguage,
    initial_project_settings_content,
};
use std::{
    any::{Any, TypeId, type_name},
    cell::RefCell,
    collections::HashMap,
    num::{NonZero, NonZeroU32},
    ops::Range,
    rc::Rc,
    sync::{Arc, LazyLock, RwLock},
    time::Duration,
};
use theme_settings::ThemeSettings;
use ui::{
    Banner, ContextMenu, Divider, DropdownMenu, DropdownStyle, IconButtonShape, KeyBinding,
    KeybindingHint, PopoverMenu, Scrollbars, Switch, Tooltip, TreeViewItem, WithScrollbar,
    prelude::*,
};

use flint_actions::{OpenProjectSettings, OpenSettings, OpenSettingsAt, OpenSettingsAtTarget};
use util::{ResultExt as _, paths::PathStyle, rel_path::RelPath};
use workspace::{
    AppState, MultiWorkspace, OpenOptions, OpenVisible, Workspace, WorkspaceSettings,
    client_side_decorations,
};

use crate::components::{
    EnumVariantDropdown, NumberField, NumberFieldMode, NumberFieldType, SettingsInputField,
    SettingsSectionHeader, font_picker, icon_theme_picker, theme_picker,
};

const NAVBAR_CONTAINER_TAB_INDEX: isize = 0;
const NAVBAR_GROUP_TAB_INDEX: isize = 1;

const HEADER_CONTAINER_TAB_INDEX: isize = 2;
const HEADER_GROUP_TAB_INDEX: isize = 3;

const CONTENT_CONTAINER_TAB_INDEX: isize = 4;
const CONTENT_GROUP_TAB_INDEX: isize = 5;

actions!(
    settings_editor,
    [
        /// Minimizes the settings UI window.
        Minimize,
        /// Toggles focus between the navbar and the main content.
        ToggleFocusNav,
        /// Expands the navigation entry.
        ExpandNavEntry,
        /// Collapses the navigation entry.
        CollapseNavEntry,
        /// Focuses the next file in the file list.
        FocusNextFile,
        /// Focuses the previous file in the file list.
        FocusPreviousFile,
        /// Opens an editor for the current file
        OpenCurrentFile,
        /// Focuses the previous root navigation entry.
        FocusPreviousRootNavEntry,
        /// Focuses the next root navigation entry.
        FocusNextRootNavEntry,
        /// Focuses the first navigation entry.
        FocusFirstNavEntry,
        /// Focuses the last navigation entry.
        FocusLastNavEntry,
        /// Focuses and opens the next navigation entry without moving focus to content.
        FocusNextNavEntry,
        /// Focuses and opens the previous navigation entry without moving focus to content.
        FocusPreviousNavEntry
    ]
);

#[derive(Action, PartialEq, Eq, Clone, Copy, Debug, JsonSchema, Deserialize)]
#[action(namespace = settings_editor)]
struct FocusFile(pub u32);

struct SettingField<T: 'static> {
    pick: fn(&SettingsContent) -> Option<&T>,
    write: fn(&mut SettingsContent, Option<T>, &App),
    /// A json-path-like string that gives a unique-ish string that identifies
    /// where in the JSON the setting is defined.
    ///
    /// The syntax is `jq`-like, but modified slightly to be URL-safe (and
    /// without the leading dot), e.g. `foo.bar`.
    ///
    /// They are URL-safe (this is important since links are the main use-case
    /// for these paths).
    ///
    /// There are a couple of special cases:
    /// - discrimminants are represented with a trailing `$`, for example
    /// `terminal.working_directory$`. This is to distinguish the discrimminant
    /// setting (i.e. the setting that changes whether the value is a string or
    /// an object) from the setting in the case that it is a string.
    /// - language-specific settings begin `languages.$(language)`. Links
    /// targeting these settings should take the form `languages/Rust/...`, for
    /// example, but are not currently supported.
    json_path: Option<&'static str>,
}

impl<T: 'static> Clone for SettingField<T> {
    fn clone(&self) -> Self {
        *self
    }
}

// manual impl because derive puts a Copy bound on T, which is inaccurate in our case
impl<T: 'static> Copy for SettingField<T> {}

/// Helper for unimplemented settings, used in combination with `SettingField::unimplemented`
/// to keep the setting around in the UI with valid pick and write implementations, but don't actually try to render it.
/// TODO(settings_ui): In non-dev builds (`#[cfg(not(debug_assertions))]`) make this render as edit-in-json
#[derive(Clone, Copy)]
struct UnimplementedSettingField;

impl PartialEq for UnimplementedSettingField {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl<T: 'static> SettingField<T> {
    /// Helper for settings with types that are not yet implemented.
    #[allow(unused)]
    fn unimplemented(self) -> SettingField<UnimplementedSettingField> {
        SettingField {
            pick: |_| Some(&UnimplementedSettingField),
            write: |_, _, _| unreachable!(),
            json_path: self.json_path,
        }
    }
}

trait AnySettingField {
    fn as_any(&self) -> &dyn Any;
    fn type_name(&self) -> &'static str;
    fn type_id(&self) -> TypeId;
    // Returns the file this value was set in and true, or File::Default and false to indicate it was not found in any file (missing default)
    fn file_set_in(&self, file: SettingsUiFile, cx: &App) -> (settings::SettingsFile, bool);
    fn reset_to_default_fn(
        &self,
        current_file: &SettingsUiFile,
        file_set_in: &settings::SettingsFile,
        cx: &App,
    ) -> Option<Box<dyn Fn(&mut Window, &mut App)>>;

    fn json_path(&self) -> Option<&'static str>;
}

impl<T: PartialEq + Clone + Send + Sync + 'static> AnySettingField for SettingField<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn type_name(&self) -> &'static str {
        type_name::<T>()
    }

    fn type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }

    fn file_set_in(&self, file: SettingsUiFile, cx: &App) -> (settings::SettingsFile, bool) {
        let (file, value) = cx
            .global::<SettingsStore>()
            .get_value_from_file(file.to_settings(), self.pick);
        return (file, value.is_some());
    }

    fn reset_to_default_fn(
        &self,
        current_file: &SettingsUiFile,
        file_set_in: &settings::SettingsFile,
        cx: &App,
    ) -> Option<Box<dyn Fn(&mut Window, &mut App)>> {
        if file_set_in == &settings::SettingsFile::Default {
            return None;
        }
        if file_set_in != &current_file.to_settings() {
            return None;
        }
        let this = *self;
        let store = SettingsStore::global(cx);
        let default_value = (this.pick)(store.raw_default_settings());
        let is_default = store
            .get_content_for_file(file_set_in.clone())
            .map_or(None, this.pick)
            == default_value;
        if is_default {
            return None;
        }
        let current_file = current_file.clone();

        return Some(Box::new(move |window, cx| {
            let store = SettingsStore::global(cx);
            let default_value = (this.pick)(store.raw_default_settings());
            let is_set_somewhere_other_than_default = store
                .get_value_up_to_file(current_file.to_settings(), this.pick)
                .0
                != settings::SettingsFile::Default;
            let value_to_set = if is_set_somewhere_other_than_default {
                default_value.cloned()
            } else {
                None
            };
            update_settings_file(
                current_file.clone(),
                None,
                window,
                cx,
                move |settings, app| {
                    (this.write)(settings, value_to_set, app);
                },
            )
            // todo(settings_ui): Don't log err
            .log_err();
        }));
    }

    fn json_path(&self) -> Option<&'static str> {
        self.json_path
    }
}

#[derive(Default, Clone)]
struct SettingFieldRenderer {
    renderers: Rc<
        RefCell<
            HashMap<
                TypeId,
                Box<
                    dyn Fn(
                        &SettingsWindow,
                        &SettingItem,
                        SettingsUiFile,
                        Option<&SettingsFieldMetadata>,
                        bool,
                        &mut Window,
                        &mut Context<SettingsWindow>,
                    ) -> Stateful<Div>,
                >,
            >,
        >,
    >,
}

impl Global for SettingFieldRenderer {}

impl SettingFieldRenderer {
    fn add_basic_renderer<T: 'static>(
        &mut self,
        render_control: impl Fn(
            SettingField<T>,
            SettingsUiFile,
            Option<&SettingsFieldMetadata>,
            &mut Window,
            &mut App,
        ) -> AnyElement
        + 'static,
    ) -> &mut Self {
        self.add_renderer(
            move |settings_window: &SettingsWindow,
                  item: &SettingItem,
                  field: SettingField<T>,
                  settings_file: SettingsUiFile,
                  metadata: Option<&SettingsFieldMetadata>,
                  sub_field: bool,
                  window: &mut Window,
                  cx: &mut Context<SettingsWindow>| {
                render_settings_item(
                    settings_window,
                    item,
                    settings_file.clone(),
                    render_control(field, settings_file, metadata, window, cx),
                    sub_field,
                    cx,
                )
            },
        )
    }

    fn add_renderer<T: 'static>(
        &mut self,
        renderer: impl Fn(
            &SettingsWindow,
            &SettingItem,
            SettingField<T>,
            SettingsUiFile,
            Option<&SettingsFieldMetadata>,
            bool,
            &mut Window,
            &mut Context<SettingsWindow>,
        ) -> Stateful<Div>
        + 'static,
    ) -> &mut Self {
        let key = TypeId::of::<T>();
        let renderer = Box::new(
            move |settings_window: &SettingsWindow,
                  item: &SettingItem,
                  settings_file: SettingsUiFile,
                  metadata: Option<&SettingsFieldMetadata>,
                  sub_field: bool,
                  window: &mut Window,
                  cx: &mut Context<SettingsWindow>| {
                let field = *item
                    .field
                    .as_ref()
                    .as_any()
                    .downcast_ref::<SettingField<T>>()
                    .unwrap();
                renderer(
                    settings_window,
                    item,
                    field,
                    settings_file,
                    metadata,
                    sub_field,
                    window,
                    cx,
                )
            },
        );
        self.renderers.borrow_mut().insert(key, renderer);
        self
    }
}

struct NonFocusableHandle {
    handle: FocusHandle,
    _subscription: Subscription,
}

impl NonFocusableHandle {
    fn new(tab_index: isize, tab_stop: bool, window: &mut Window, cx: &mut App) -> Entity<Self> {
        let handle = cx.focus_handle().tab_index(tab_index).tab_stop(tab_stop);
        Self::from_handle(handle, window, cx)
    }

    fn from_handle(handle: FocusHandle, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let _subscription = cx.on_focus(&handle, window, {
                move |_, window, cx| {
                    window.focus_next(cx);
                }
            });
            Self {
                handle,
                _subscription,
            }
        })
    }
}

impl Focusable for NonFocusableHandle {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.handle.clone()
    }
}

#[derive(Default)]
struct SettingsFieldMetadata {
    placeholder: Option<&'static str>,
    should_do_titlecase: Option<bool>,
}

pub fn init(cx: &mut App) {
    init_renderers(cx);
    let queue = ProjectSettingsUpdateQueue::new(cx);
    cx.set_global(queue);

    cx.on_action(|_: &OpenSettings, cx| {
        open_settings_editor(None, None, None, cx);
    });

    cx.observe_new(|workspace: &mut workspace::Workspace, _, _| {
        workspace
            .register_action(|_, action: &OpenSettingsAt, window, cx| {
                let window_handle = window.window_handle().downcast::<MultiWorkspace>();
                open_settings_editor_at_target(
                    Some(&action.path),
                    action.target.as_ref().map(SettingsFileTarget::from),
                    window_handle,
                    cx,
                );
            })
            .register_action(|_, _: &OpenSettings, window, cx| {
                let window_handle = window.window_handle().downcast::<MultiWorkspace>();
                open_settings_editor(None, None, window_handle, cx);
            })
            .register_action(|workspace, _: &OpenProjectSettings, window, cx| {
                let window_handle = window.window_handle().downcast::<MultiWorkspace>();
                let target_worktree_id = workspace
                    .project()
                    .read(cx)
                    .visible_worktrees(cx)
                    .find_map(|tree| {
                        tree.read(cx)
                            .root_entry()?
                            .is_dir()
                            .then_some(tree.read(cx).id())
                    });
                open_settings_editor(None, target_worktree_id, window_handle, cx);
            });
    })
    .detach();
}

fn init_renderers(cx: &mut App) {
    cx.default_global::<SettingFieldRenderer>()
        .add_renderer::<UnimplementedSettingField>(
            |settings_window, item, _, settings_file, _, sub_field, _, cx| {
                render_settings_item(
                    settings_window,
                    item,
                    settings_file,
                    Button::new(
                        "open-in-settings-file",
                        localization::text(cx, "settings-edit-json"),
                    )
                        .style(ButtonStyle::Outlined)
                        .size(ButtonSize::Medium)
                        .tab_index(0_isize)
                        .tooltip(Tooltip::for_action_title_in(
                            localization::text(cx, "settings-edit-json"),
                            &OpenCurrentFile,
                            &settings_window.focus_handle,
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_current_settings_file(window, cx);
                        }))
                        .into_any_element(),
                    sub_field,
                    cx,
                )
            },
        )
        .add_basic_renderer::<bool>(render_toggle_button)
        .add_basic_renderer::<String>(render_text_field)
        .add_basic_renderer::<SharedString>(render_text_field)
        .add_basic_renderer::<settings::CursorShape>(render_dropdown)
        .add_basic_renderer::<settings::RestoreOnStartupBehavior>(render_dropdown)
        .add_basic_renderer::<settings::BottomDockLayout>(render_dropdown)
        .add_basic_renderer::<settings::OnLastWindowClosed>(render_dropdown)
        .add_basic_renderer::<settings::CliDefaultOpenBehavior>(render_dropdown)
        .add_basic_renderer::<settings::CloseWindowWhenNoItems>(render_dropdown)
        .add_basic_renderer::<settings::TextRenderingMode>(render_dropdown)
        .add_basic_renderer::<settings::FontFamilyName>(render_font_picker)
        .add_basic_renderer::<settings::BaseKeymapContent>(render_dropdown)
        .add_basic_renderer::<settings::MultiCursorModifier>(render_dropdown)
        .add_basic_renderer::<settings::HideMouseMode>(render_dropdown)
        .add_basic_renderer::<settings::CurrentLineHighlight>(render_dropdown)
        .add_basic_renderer::<settings::ShowWhitespaceSetting>(render_dropdown)
        .add_basic_renderer::<settings::SoftWrap>(render_dropdown)
        .add_basic_renderer::<settings::AutoIndentMode>(render_dropdown)
        .add_basic_renderer::<settings::ScrollBeyondLastLine>(render_dropdown)
        .add_basic_renderer::<settings::SnippetSortOrder>(render_dropdown)
        .add_basic_renderer::<settings::ClosePosition>(render_dropdown)
        .add_basic_renderer::<settings::DockSide>(render_dropdown)
        .add_basic_renderer::<settings::AgentThreadReopenSessionsOnStartup>(render_dropdown)
        .add_basic_renderer::<settings::TerminalDockPosition>(render_dropdown)
        .add_basic_renderer::<settings::DockPosition>(render_dropdown)
        .add_basic_renderer::<settings::GitGutterSetting>(render_dropdown)
        .add_basic_renderer::<settings::GitHunkStyleSetting>(render_dropdown)
        .add_basic_renderer::<settings::GitPathStyle>(render_dropdown)
        .add_basic_renderer::<settings::CommitMessageGeneratorAgent>(render_dropdown)
        .add_basic_renderer::<settings::DiagnosticSeverityContent>(render_dropdown)
        .add_basic_renderer::<settings::SeedQuerySetting>(render_dropdown)
        .add_basic_renderer::<settings::DoubleClickInMultibuffer>(render_dropdown)
        .add_basic_renderer::<settings::GoToDefinitionFallback>(render_dropdown)
        .add_basic_renderer::<settings::GoToDefinitionScrollStrategy>(render_dropdown)
        .add_basic_renderer::<settings::OpenResultsIn>(render_dropdown)
        .add_basic_renderer::<settings::ActivateOnClose>(render_dropdown)
        .add_basic_renderer::<settings::ShowDiagnostics>(render_dropdown)
        .add_basic_renderer::<settings::ShowCloseButton>(render_dropdown)
        .add_basic_renderer::<settings::ProjectPanelEntrySpacing>(render_dropdown)
        .add_basic_renderer::<settings::ProjectPanelSortMode>(render_dropdown)
        .add_basic_renderer::<settings::ProjectPanelSortOrder>(render_dropdown)
        .add_basic_renderer::<settings::RewrapBehavior>(render_dropdown)
        .add_basic_renderer::<settings::FormatOnSave>(render_dropdown)
        .add_basic_renderer::<settings::LineEndingSetting>(render_dropdown)
        .add_basic_renderer::<settings::IndentGuideColoring>(render_dropdown)
        .add_basic_renderer::<settings::IndentGuideBackgroundColoring>(render_dropdown)
        .add_basic_renderer::<settings::FileFinderWidthContent>(render_dropdown)
        .add_basic_renderer::<settings::ShowDiagnostics>(render_dropdown)
        .add_basic_renderer::<settings::WordsCompletionMode>(render_dropdown)
        .add_basic_renderer::<settings::LspInsertMode>(render_dropdown)
        .add_basic_renderer::<settings::CompletionDetailAlignment>(render_dropdown)
        .add_basic_renderer::<settings::CompletionMenuItemKind>(render_dropdown)
        .add_basic_renderer::<settings::DiffViewStyle>(render_dropdown)
        .add_basic_renderer::<settings::AlternateScroll>(render_dropdown)
        .add_basic_renderer::<settings::TerminalBlink>(render_dropdown)
        .add_basic_renderer::<settings::CursorShapeContent>(render_dropdown)
        .add_basic_renderer::<f32>(render_editable_number_field)
        .add_basic_renderer::<u32>(render_editable_number_field)
        .add_basic_renderer::<u64>(render_editable_number_field)
        .add_basic_renderer::<usize>(render_editable_number_field)
        .add_basic_renderer::<NonZero<usize>>(render_editable_number_field)
        .add_basic_renderer::<NonZeroU32>(render_editable_number_field)
        .add_basic_renderer::<settings::CodeFade>(render_editable_number_field)
        .add_basic_renderer::<settings::DelayMs>(render_editable_number_field)
        .add_basic_renderer::<settings::FontWeightContent>(render_editable_number_field)
        .add_basic_renderer::<settings::CenteredPaddingSettings>(render_editable_number_field)
        .add_basic_renderer::<settings::InactiveOpacity>(render_editable_number_field)
        .add_basic_renderer::<settings::MinimumContrast>(render_editable_number_field)
        .add_basic_renderer::<settings::ShowScrollbar>(render_dropdown)
        .add_basic_renderer::<settings::ScrollbarDiagnostics>(render_dropdown)
        .add_basic_renderer::<settings::ShowMinimap>(render_dropdown)
        .add_basic_renderer::<settings::DisplayIn>(render_dropdown)
        .add_basic_renderer::<settings::MinimapThumb>(render_dropdown)
        .add_basic_renderer::<settings::MinimapThumbBorder>(render_dropdown)
        .add_basic_renderer::<settings::ModeContent>(render_dropdown)
        .add_basic_renderer::<settings::UseSystemClipboard>(render_dropdown)
        .add_basic_renderer::<settings::VimInsertModeCursorShape>(render_dropdown)
        .add_basic_renderer::<settings::ImageFileSizeUnit>(render_dropdown)
        .add_basic_renderer::<settings::StatusStyle>(render_dropdown)
        .add_basic_renderer::<settings::GitPanelSortBy>(render_dropdown)
        .add_basic_renderer::<settings::GitPanelGroupBy>(render_dropdown)
        .add_basic_renderer::<settings::EncodingDisplayOptions>(render_dropdown)
        .add_basic_renderer::<settings::PaneSplitDirectionHorizontal>(render_dropdown)
        .add_basic_renderer::<settings::PaneSplitDirectionVertical>(render_dropdown)
        .add_basic_renderer::<settings::PaneSplitDirectionVertical>(render_dropdown)
        .add_basic_renderer::<settings::CodeLens>(render_dropdown)
        .add_basic_renderer::<settings::DocumentColorsRenderMode>(render_dropdown)
        .add_basic_renderer::<settings::ThemeSelectionDiscriminants>(render_dropdown)
        .add_basic_renderer::<settings::ThemeAppearanceMode>(render_dropdown)
        .add_basic_renderer::<settings::ThemeName>(render_theme_picker)
        .add_basic_renderer::<settings::IconThemeSelectionDiscriminants>(render_dropdown)
        .add_basic_renderer::<settings::IconThemeName>(render_icon_theme_picker)
        .add_basic_renderer::<settings::BufferLineHeightDiscriminants>(render_dropdown)
        .add_basic_renderer::<settings::AutosaveSettingDiscriminants>(render_dropdown)
        .add_basic_renderer::<settings::WorkingDirectoryDiscriminants>(render_dropdown)
        .add_basic_renderer::<settings::IncludeIgnoredContent>(render_dropdown)
        .add_basic_renderer::<settings::ShowIndentGuides>(render_dropdown)
        .add_basic_renderer::<settings::ShellDiscriminants>(render_dropdown)
        .add_basic_renderer::<settings::RelativeLineNumbers>(render_dropdown)
        .add_basic_renderer::<settings::WindowDecorations>(render_dropdown)
        .add_basic_renderer::<settings::WindowButtonLayoutContentDiscriminants>(render_dropdown)
        .add_basic_renderer::<settings::ScanSymlinksSetting>(render_dropdown)
        .add_basic_renderer::<settings::FontSize>(render_editable_number_field)
        .add_basic_renderer::<settings::SemanticTokens>(render_dropdown)
        .add_basic_renderer::<settings::DocumentFoldingRanges>(render_dropdown)
        .add_basic_renderer::<settings::DocumentSymbols>(render_dropdown)
        .add_basic_renderer::<settings::TerminalBell>(render_dropdown)
        // please semicolon stay on next line
        ;
}

#[derive(Clone, Copy)]
enum SettingsFileTarget {
    User,
    Project(WorktreeId),
}

impl From<&OpenSettingsAtTarget> for SettingsFileTarget {
    fn from(target: &OpenSettingsAtTarget) -> Self {
        match target {
            OpenSettingsAtTarget::User => Self::User,
            OpenSettingsAtTarget::Project { worktree_id } => {
                Self::Project(WorktreeId::from_usize(*worktree_id))
            }
        }
    }
}

pub fn open_settings_editor(
    path: Option<&str>,
    target_worktree_id: Option<WorktreeId>,
    workspace_handle: Option<WindowHandle<MultiWorkspace>>,
    cx: &mut App,
) {
    open_settings_editor_at_target(
        path,
        target_worktree_id.map(SettingsFileTarget::Project),
        workspace_handle,
        cx,
    );
}

fn open_settings_editor_at_target(
    path: Option<&str>,
    target_file: Option<SettingsFileTarget>,
    workspace_handle: Option<WindowHandle<MultiWorkspace>>,
    cx: &mut App,
) {
    fn select_target_file(
        target_file: SettingsFileTarget,
        settings_window: &mut SettingsWindow,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) {
        let file_index = settings_window
            .files
            .iter()
            .position(|(file, _)| match target_file {
                SettingsFileTarget::User => matches!(file, SettingsUiFile::User),
                SettingsFileTarget::Project(worktree_id) => file.worktree_id() == Some(worktree_id),
            });
        if let Some(file_index) = file_index {
            settings_window.change_file(file_index, window, cx);
        }
    }

    /// Assumes a settings GUI window is already open
    fn open_path(
        path: &str,
        settings_window: &mut SettingsWindow,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) {
        if path.starts_with("languages.$(language)") {
            log::error!("language-specific settings links are not currently supported");
            return;
        }

        let query = format!("#{path}");
        let indices = settings_window.filter_by_json_path(&query);

        settings_window.opening_link = true;
        settings_window.search_bar.update(cx, |editor, cx| {
            editor.set_text(query.clone(), window, cx);
        });
        settings_window.apply_match_indices(indices.iter().copied(), &query);

        if indices.len() == 1
            && let Some(search_index) = settings_window.search_index.as_ref()
        {
            let SearchKeyLUTEntry {
                page_index,
                item_index,
                header_index,
                ..
            } = search_index.key_lut[indices[0]];
            let page = &settings_window.pages[page_index];
            let item = &page.items[item_index];

            if settings_window.filter_table[page_index][item_index]
                && let SettingsPageItem::SubPageLink(link) = item
                && let SettingsPageItem::SectionHeader(header) = page.items[header_index]
            {
                settings_window.push_sub_page(link.clone(), SharedString::from(header), window, cx);
            }
        }

        cx.notify();
    }

    let existing_window = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<SettingsWindow>());

    if let Some(existing_window) = existing_window {
        existing_window
            .update(cx, |settings_window, window, cx| {
                settings_window.original_window = workspace_handle;

                window.activate_window();
                if let Some(target_file) = target_file {
                    select_target_file(target_file, settings_window, window, cx);
                }
                if let Some(path) = path {
                    open_path(path, settings_window, window, cx);
                } else if target_file.is_some() {
                    cx.notify();
                }
            })
            .ok();
        return;
    }

    // We have to defer this to get the workspace off the stack.
    let path = path.map(ToOwned::to_owned);
    cx.defer(move |cx| {
        let current_rem_size: f32 = theme_settings::ThemeSettings::get_global(cx)
            .ui_font_size(cx)
            .into();

        let default_bounds = DEFAULT_ADDITIONAL_WINDOW_SIZE;
        let default_rem_size = 16.0;
        let scale_factor = current_rem_size / default_rem_size;
        let scaled_bounds: gpui::Size<Pixels> = default_bounds.map(|axis| axis * scale_factor);

        let app_id = ReleaseChannel::global(cx).app_id();
        let window_decorations = match std::env::var("ZED_WINDOW_DECORATIONS") {
            Ok(val) if val == "server" => gpui::WindowDecorations::Server,
            Ok(val) if val == "client" => gpui::WindowDecorations::Client,
            _ => match WorkspaceSettings::get_global(cx).window_decorations {
                settings::WindowDecorations::Server => gpui::WindowDecorations::Server,
                settings::WindowDecorations::Client => gpui::WindowDecorations::Client,
            },
        };

        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(localization::text(cx, "settings-window-title")),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.0), px(12.0))),
                }),
                focus: true,
                show: true,
                is_movable: true,
                kind: gpui::WindowKind::Normal,
                window_background: cx.theme().window_background_appearance(),
                app_id: Some(app_id.to_owned()),
                window_decorations: Some(window_decorations),
                window_min_size: Some(gpui::Size {
                    // Don't make the settings window thinner than this,
                    // otherwise, it gets unusable. Users with smaller res monitors
                    // can customize the height, but not the width.
                    width: px(900.0),
                    height: px(240.0),
                }),
                window_bounds: Some(WindowBounds::centered(scaled_bounds, cx)),
                ..Default::default()
            },
            |window, cx| {
                let settings_window =
                    cx.new(|cx| SettingsWindow::new(workspace_handle, window, cx));
                settings_window.update(cx, |settings_window, cx| {
                    if let Some(target_file) = target_file {
                        select_target_file(target_file, settings_window, window, cx);
                    }
                    if let Some(path) = path {
                        open_path(&path, settings_window, window, cx);
                    }
                });

                settings_window
            },
        )
        .log_err();
    });
}

/// The current sub page path that is selected.
/// If this is empty the selected page is rendered,
/// otherwise the last sub page gets rendered.
///
/// Global so that `pick` and `write` callbacks can access it
/// and use it to dynamically render sub pages (e.g. for language settings)
static ACTIVE_LANGUAGE: LazyLock<RwLock<Option<SharedString>>> =
    LazyLock::new(|| RwLock::new(Option::None));

fn active_language() -> Option<SharedString> {
    ACTIVE_LANGUAGE
        .read()
        .ok()
        .and_then(|language| language.clone())
}

fn active_language_mut() -> Option<std::sync::RwLockWriteGuard<'static, Option<SharedString>>> {
    ACTIVE_LANGUAGE.write().ok()
}

pub struct SettingsWindow {
    title_bar: Option<Entity<PlatformTitleBar>>,
    original_window: Option<WindowHandle<MultiWorkspace>>,
    files: Vec<(SettingsUiFile, FocusHandle)>,
    worktree_root_dirs: HashMap<WorktreeId, String>,
    current_file: SettingsUiFile,
    pages: Vec<SettingsPage>,
    sub_page_stack: Vec<SubPage>,
    opening_link: bool,
    search_bar: Entity<Editor>,
    search_task: Option<Task<()>>,
    /// Cached settings file buffers to avoid repeated disk I/O on each settings change
    project_setting_file_buffers: HashMap<ProjectPath, Entity<Buffer>>,
    /// Index into navbar_entries
    navbar_entry: usize,
    navbar_entries: Vec<NavBarEntry>,
    navbar_scroll_handle: UniformListScrollHandle,
    /// [page_index][page_item_index] will be false
    /// when the item is filtered out either by searches
    /// or by the current file
    navbar_focus_subscriptions: Vec<gpui::Subscription>,
    filter_table: Vec<Vec<bool>>,
    has_query: bool,
    content_handles: Vec<Vec<Entity<NonFocusableHandle>>>,
    focus_handle: FocusHandle,
    navbar_focus_handle: Entity<NonFocusableHandle>,
    content_focus_handle: Entity<NonFocusableHandle>,
    files_focus_handle: FocusHandle,
    search_index: Option<Arc<SearchIndex>>,
    list_state: ListState,
    pub(crate) regex_validation_error: Option<String>,
    last_copied_link_path: Option<&'static str>,
}

struct SearchDocument {
    id: usize,
    words: Vec<String>,
}

struct SearchIndex {
    documents: Vec<SearchDocument>,
    fuzzy_match_candidates: Vec<StringMatchCandidate>,
    key_lut: Vec<SearchKeyLUTEntry>,
}

struct SearchKeyLUTEntry {
    page_index: usize,
    header_index: usize,
    item_index: usize,
    json_path: Option<&'static str>,
}

struct SubPage {
    link: SubPageLink,
    section_header: SharedString,
    scroll_handle: ScrollHandle,
}

impl SubPage {
    fn new(link: SubPageLink, section_header: SharedString) -> Self {
        if link.r#type == SubPageType::Language
            && let Some(mut active_language_global) = active_language_mut()
        {
            active_language_global.replace(link.title.clone());
        }

        SubPage {
            link,
            section_header,
            scroll_handle: ScrollHandle::new(),
        }
    }
}

impl Drop for SubPage {
    fn drop(&mut self) {
        if self.link.r#type == SubPageType::Language
            && let Some(mut active_language_global) = active_language_mut()
            && active_language_global
                .as_ref()
                .is_some_and(|language_name| language_name == &self.link.title)
        {
            active_language_global.take();
        }
    }
}

#[derive(Debug)]
struct NavBarEntry {
    title: &'static str,
    is_root: bool,
    expanded: bool,
    page_index: usize,
    item_index: Option<usize>,
    focus_handle: FocusHandle,
}

struct SettingsPage {
    title: &'static str,
    items: Box<[SettingsPageItem]>,
}

#[derive(PartialEq)]
enum SettingsPageItem {
    SectionHeader(&'static str),
    SettingItem(SettingItem),
    UserLanguageSetting(UserLanguageSettingItem),
    SubPageLink(SubPageLink),
    DynamicItem(DynamicItem),
    ActionLink(ActionLink),
}

impl std::fmt::Debug for SettingsPageItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsPageItem::SectionHeader(header) => write!(f, "SectionHeader({})", header),
            SettingsPageItem::SettingItem(setting_item) => {
                write!(f, "SettingItem({})", setting_item.title)
            }
            SettingsPageItem::UserLanguageSetting(setting_item) => {
                write!(f, "UserLanguageSetting({})", setting_item.title)
            }
            SettingsPageItem::SubPageLink(sub_page_link) => {
                write!(f, "SubPageLink({})", sub_page_link.title)
            }
            SettingsPageItem::DynamicItem(dynamic_item) => {
                write!(f, "DynamicItem({})", dynamic_item.discriminant.title)
            }
            SettingsPageItem::ActionLink(action_link) => {
                write!(f, "ActionLink({})", action_link.title)
            }
        }
    }
}

impl SettingsPageItem {
    fn header_text(&self) -> Option<&'static str> {
        match self {
            SettingsPageItem::SectionHeader(header) => Some(header),
            _ => None,
        }
    }

    fn render(
        &self,
        settings_window: &SettingsWindow,
        item_index: usize,
        bottom_border: bool,
        extra_bottom_padding: bool,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> AnyElement {
        let file = settings_window.current_file.clone();

        let apply_padding = |element: Stateful<Div>| -> Stateful<Div> {
            let element = element.pt_4();
            if extra_bottom_padding {
                element.pb_10()
            } else {
                element.pb_4()
            }
        };

        let mut render_setting_item_inner =
            |setting_item: &SettingItem,
             padding: bool,
             sub_field: bool,
             cx: &mut Context<SettingsWindow>| {
                let renderer = cx.default_global::<SettingFieldRenderer>().clone();
                let (_, found) = setting_item.field.file_set_in(file.clone(), cx);

                let renderers = renderer.renderers.borrow();

                let field_renderer =
                    renderers.get(&AnySettingField::type_id(setting_item.field.as_ref()));
                let field_renderer_or_warning =
                    field_renderer.ok_or("NO RENDERER").and_then(|renderer| {
                        if cfg!(debug_assertions) && !found {
                            Err("NO DEFAULT")
                        } else {
                            Ok(renderer)
                        }
                    });

                let field = match field_renderer_or_warning {
                    Ok(field_renderer) => window.with_id(item_index, |window| {
                        field_renderer(
                            settings_window,
                            setting_item,
                            file.clone(),
                            setting_item.metadata.as_deref(),
                            sub_field,
                            window,
                            cx,
                        )
                    }),
                    Err(warning) => render_settings_item(
                        settings_window,
                        setting_item,
                        file.clone(),
                        Button::new("error-warning", warning)
                            .style(ButtonStyle::Outlined)
                            .size(ButtonSize::Medium)
                            .start_icon(Icon::new(IconName::Debug).color(Color::Error))
                            .tab_index(0_isize)
                            .tooltip(Tooltip::text(setting_item.field.type_name()))
                            .into_any_element(),
                        sub_field,
                        cx,
                    ),
                };

                let field = if padding {
                    field.map(apply_padding)
                } else {
                    field
                };

                (field, field_renderer_or_warning.is_ok())
            };

        match self {
            SettingsPageItem::SectionHeader(header) => {
                SettingsSectionHeader::new(settings_source_text(cx, header)).into_any_element()
            }
            SettingsPageItem::SettingItem(setting_item) => {
                let (field_with_padding, _) =
                    render_setting_item_inner(setting_item, true, false, cx);

                v_flex()
                    .group("setting-item")
                    .px_8()
                    .child(field_with_padding)
                    .when(bottom_border, |this| this.child(Divider::horizontal()))
                    .into_any_element()
            }
            SettingsPageItem::UserLanguageSetting(setting_item) => {
                const LANGUAGE_LABELS: &[&str] = &["English", "简体中文"];
                let current_language = SettingsStore::global(cx).ui_language();
                let control = EnumVariantDropdown::new(
                    "ui-language-dropdown",
                    current_language,
                    &UiLanguage::ALL,
                    LANGUAGE_LABELS,
                    move |language, _, cx| {
                        if language != current_language {
                            SettingsStore::global(cx).update_user_settings_file(
                                <dyn fs::Fs>::global(cx),
                                move |settings, _| settings.ui_language = Some(language),
                            );
                        }
                    },
                )
                .title_case(false)
                .tab_index(0)
                .into_any_element();

                v_flex()
                    .group("setting-item")
                    .px_8()
                    .child(
                        h_flex()
                            .id(setting_item.title.clone())
                            .min_w_0()
                            .justify_between()
                            .map(apply_padding)
                            .child(
                                v_flex()
                                    .relative()
                                    .w_full()
                                    .max_w_2_3()
                                    .min_w_0()
                                    .child(Label::new(setting_item.title.clone()))
                                    .child(
                                        Label::new(setting_item.description.clone())
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .child(render_settings_item_link(
                                        setting_item.description.clone(),
                                        Some("ui_language"),
                                        false,
                                        settings_window,
                                        cx,
                                    )),
                            )
                            .child(control),
                    )
                    .when(bottom_border, |this| this.child(Divider::horizontal()))
                    .into_any_element()
            }
            SettingsPageItem::SubPageLink(sub_page_link) => v_flex()
                .group("setting-item")
                .px_8()
                .child(
                    h_flex()
                        .id(sub_page_link.title.clone())
                        .w_full()
                        .min_w_0()
                        .justify_between()
                        .map(apply_padding)
                        .child(
                            v_flex()
                                .relative()
                                .w_full()
                                .max_w_1_2()
                                .child(Label::new(settings_source_text(cx, &sub_page_link.title)))
                                .when_some(
                                    sub_page_link.description.as_ref(),
                                    |this, description| {
                                        this.child(
                                            Label::new(settings_source_text(cx, description))
                                                .size(LabelSize::Small)
                                                .color(Color::Muted),
                                        )
                                    },
                                ),
                        )
                        .child(
                            Button::new(
                                ("sub-page".into(), sub_page_link.title.clone()),
                                localization::text(cx, "settings-configure"),
                            )
                            .tab_index(0_isize)
                            .end_icon(
                                Icon::new(IconName::ChevronRight)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .style(ButtonStyle::OutlinedGhost)
                            .size(ButtonSize::Medium)
                            .on_click({
                                let sub_page_link = sub_page_link.clone();
                                cx.listener(move |this, _, window, cx| {
                                    let header_text = this
                                        .sub_page_stack
                                        .last()
                                        .map(|sub_page| sub_page.link.title.clone())
                                        .or_else(|| {
                                            this.current_page()
                                                .items
                                                .iter()
                                                .take(item_index)
                                                .rev()
                                                .find_map(|item| {
                                                    item.header_text().map(SharedString::new_static)
                                                })
                                        });

                                    let Some(header) = header_text else {
                                        unreachable!(
                                            "All items always have a section header above them"
                                        )
                                    };

                                    this.push_sub_page(sub_page_link.clone(), header, window, cx)
                                })
                            }),
                        )
                        .child(render_settings_item_link(
                            sub_page_link.title.clone(),
                            sub_page_link.json_path,
                            false,
                            settings_window,
                            cx,
                        )),
                )
                .when(bottom_border, |this| this.child(Divider::horizontal()))
                .into_any_element(),
            SettingsPageItem::DynamicItem(DynamicItem {
                discriminant: discriminant_setting_item,
                pick_discriminant,
                fields,
            }) => {
                let file = file.to_settings();
                let discriminant = SettingsStore::global(cx)
                    .get_value_from_file(file, *pick_discriminant)
                    .1;

                let (discriminant_element, rendered_ok) =
                    render_setting_item_inner(discriminant_setting_item, true, false, cx);

                let has_sub_fields =
                    rendered_ok && discriminant.is_some_and(|d| !fields[d].is_empty());

                let mut content = v_flex()
                    .id("dynamic-item")
                    .child(
                        div()
                            .group("setting-item")
                            .px_8()
                            .child(discriminant_element.when(has_sub_fields, |this| this.pb_4())),
                    )
                    .when(!has_sub_fields && bottom_border, |this| {
                        this.child(h_flex().px_8().child(Divider::horizontal()))
                    });

                if rendered_ok {
                    let discriminant =
                        discriminant.expect("This should be Some if rendered_ok is true");
                    let sub_fields = &fields[discriminant];
                    let sub_field_count = sub_fields.len();

                    for (index, field) in sub_fields.iter().enumerate() {
                        let is_last_sub_field = index == sub_field_count - 1;
                        let (raw_field, _) = render_setting_item_inner(field, false, true, cx);

                        content = content.child(
                            raw_field
                                .group("setting-sub-item")
                                .mx_8()
                                .p_4()
                                .border_t_1()
                                .when(is_last_sub_field, |this| this.border_b_1())
                                .when(is_last_sub_field && extra_bottom_padding, |this| {
                                    this.mb_8()
                                })
                                .border_dashed()
                                .border_color(cx.theme().colors().border_variant)
                                .bg(cx.theme().colors().element_background.opacity(0.2)),
                        );
                    }
                }

                return content.into_any_element();
            }
            SettingsPageItem::ActionLink(action_link) => v_flex()
                .group("setting-item")
                .px_8()
                .child(
                    h_flex()
                        .id(action_link.title.clone())
                        .w_full()
                        .min_w_0()
                        .justify_between()
                        .map(apply_padding)
                        .child(
                            v_flex()
                                .relative()
                                .w_full()
                                .max_w_1_2()
                                .child(Label::new(settings_source_text(cx, &action_link.title)))
                                .when_some(
                                    action_link.description.as_ref(),
                                    |this, description| {
                                        this.child(
                                            Label::new(settings_source_text(cx, description))
                                                .size(LabelSize::Small)
                                                .color(Color::Muted),
                                        )
                                    },
                                ),
                        )
                        .child(
                            Button::new(
                                ("action-link".into(), action_link.title.clone()),
                                settings_source_text(cx, &action_link.button_text),
                            )
                            .tab_index(0_isize)
                            .end_icon(
                                Icon::new(IconName::ArrowUpRight)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .style(ButtonStyle::OutlinedGhost)
                            .size(ButtonSize::Medium)
                            .on_click({
                                let on_click = action_link.on_click.clone();
                                cx.listener(move |this, _, window, cx| {
                                    on_click(this, window, cx);
                                })
                            }),
                        ),
                )
                .when(bottom_border, |this| this.child(Divider::horizontal()))
                .into_any_element(),
        }
    }
}

fn render_settings_item(
    settings_window: &SettingsWindow,
    setting_item: &SettingItem,
    file: SettingsUiFile,
    control: AnyElement,
    sub_field: bool,
    cx: &mut Context<'_, SettingsWindow>,
) -> Stateful<Div> {
    let (found_in_file, _) = setting_item.field.file_set_in(file.clone(), cx);
    let file_set_in = SettingsUiFile::from_settings(found_in_file.clone());

    h_flex()
        .id(setting_item.title)
        .min_w_0()
        .justify_between()
        .child(
            v_flex()
                .relative()
                .w_full()
                .max_w_2_3()
                .min_w_0()
                .child(
                    h_flex()
                        .w_full()
                        .gap_1()
                        .child(Label::new(settings_source_text(cx, setting_item.title)))
                        .when_some(
                            if sub_field {
                                None
                            } else {
                                setting_item
                                    .field
                                    .reset_to_default_fn(&file, &found_in_file, cx)
                            },
                            |this, reset_to_default| {
                                this.child(
                                    IconButton::new("reset-to-default-btn", IconName::Undo)
                                        .icon_color(Color::Muted)
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text(localization::text(
                                            cx,
                                            "settings-reset-default",
                                        )))
                                        .on_click({
                                            move |_, window, cx| {
                                                reset_to_default(window, cx);
                                            }
                                        }),
                                )
                            },
                        )
                        .when_some(
                            file_set_in.filter(|file_set_in| file_set_in != &file),
                            |this, file_set_in| {
                                this.child(
                                    Label::new(localization::tr!(
                                        cx,
                                        "settings-modified-in",
                                        scope = settings_window
                                            .display_name(&file_set_in)
                                            .expect("File name should exist")
                                    ))
                                    .color(Color::Muted)
                                    .size(LabelSize::Small),
                                )
                            },
                        ),
                )
                .child(
                    Label::new(settings_source_text(cx, setting_item.description))
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .render_code_spans(),
                ),
        )
        .child(control)
        .when(settings_window.sub_page_stack.is_empty(), |this| {
            this.child(render_settings_item_link(
                setting_item.description,
                setting_item.field.json_path(),
                sub_field,
                settings_window,
                cx,
            ))
        })
}

fn render_settings_item_link(
    id: impl Into<ElementId>,
    json_path: Option<&'static str>,
    sub_field: bool,
    settings_window: &SettingsWindow,
    cx: &mut Context<'_, SettingsWindow>,
) -> impl IntoElement {
    let copied_link_matches =
        json_path.is_some() && json_path == settings_window.last_copied_link_path;

    let (link_icon, link_icon_color) = if copied_link_matches {
        (IconName::Check, Color::Success)
    } else {
        (IconName::Link, Color::Muted)
    };

    div()
        .absolute()
        .top(rems_from_px(18.))
        .map(|this| {
            if sub_field {
                this.visible_on_hover("setting-sub-item")
                    .left(rems_from_px(-8.5))
            } else {
                this.visible_on_hover("setting-item")
                    .left(rems_from_px(-22.))
            }
        })
        .child(
            IconButton::new((id.into(), "copy-link-btn"), link_icon)
                .icon_color(link_icon_color)
                .icon_size(IconSize::Small)
                .shape(IconButtonShape::Square)
                .tooltip(Tooltip::text(localization::text(cx, "settings-copy-link")))
                .when_some(json_path, |this, path| {
                    this.on_click(cx.listener(move |this, _, _, cx| {
                        let link = format!("flint://settings/{}", path);
                        cx.write_to_clipboard(ClipboardItem::new_string(link));
                        this.last_copied_link_path = Some(path);
                        cx.notify();
                    }))
                }),
        )
}

struct SettingItem {
    title: &'static str,
    description: &'static str,
    field: Box<dyn AnySettingField>,
    metadata: Option<Box<SettingsFieldMetadata>>,
    files: FileMask,
}

#[derive(PartialEq)]
struct UserLanguageSettingItem {
    title: SharedString,
    description: SharedString,
}

fn user_language_setting(cx: &App) -> UserLanguageSettingItem {
    UserLanguageSettingItem {
        title: localization::tr!(cx, "settings-language-title"),
        description: localization::tr!(cx, "settings-language-description"),
    }
}

fn settings_source_text(cx: &App, source: &str) -> SharedString {
    if let Some(identifier) = settings_item_message_id(source) {
        return localization::text(cx, identifier);
    }

    let mut arguments = localization::FluentArgs::new();
    arguments.set("source", source);
    localization::text_with_args(cx, "settings-source", &arguments)
}

/// Maps setting item titles, descriptions, and section headers to their Fluent
/// message identifiers. These can't be expressed as `settings-source` select
/// variants because Fluent identifiers forbid spaces and punctuation, so each
/// string gets its own message instead.
fn settings_item_message_id(source: &str) -> Option<&'static str> {
    Some(match source {
        "Languages & Tools" => "settings-page-languages-and-tools",
        "Search & Files" => "settings-page-search-and-files",
        "Window & Layout" => "settings-page-window-and-layout",
        "Panels" => "settings-page-panels",
        "Version Control" => "settings-page-version-control",
        "Network" => "settings-page-network",
        "Developer" => "settings-page-developer",
        "General Settings" => "settings-general-section-general",
        "Security" => "settings-general-section-security",
        "Workspace Restoration" => "settings-general-section-workspace-restoration",
        "Scoped Settings" => "settings-general-section-scoped-settings",
        "Auto Update" => "settings-general-auto-update-title",
        "When Closing With No Tabs" => "settings-general-when-closing-with-no-tabs-title",
        "What to do when using the 'close active item' action with no tabs." => {
            "settings-general-when-closing-with-no-tabs-description"
        }
        "On Last Window Closed" => "settings-general-on-last-window-closed-title",
        "What to do when the last window is closed." => {
            "settings-general-on-last-window-closed-description"
        }
        "Use System Path Prompts" => "settings-general-use-system-path-prompts-title",
        "Use native OS dialogs for 'Open' and 'Save As'." => {
            "settings-general-use-system-path-prompts-description"
        }
        "Use System Prompts" => "settings-general-use-system-prompts-title",
        "Use native OS dialogs for confirmations." => {
            "settings-general-use-system-prompts-description"
        }
        "Redact Private Values" => "settings-general-redact-private-values-title",
        "Hide the values of variables in private files." => {
            "settings-general-redact-private-values-description"
        }
        "Private Files" => "settings-general-private-files-title",
        "Globs to match against file paths to determine if a file is private." => {
            "settings-general-private-files-description"
        }
        "CLI Default Open Behavior" => "settings-general-cli-default-open-behavior-title",
        "How `flint <path>` opens directories when no flag is specified." => {
            "settings-general-cli-default-open-behavior-description"
        }
        "Trust All Projects By Default" => "settings-general-trust-all-projects-title",
        "When opening Flint, avoid Restricted Mode by auto-trusting all projects, enabling use of all features without having to give permission to each new project." => {
            "settings-general-trust-all-projects-description"
        }
        "Restore Unsaved Buffers" => "settings-general-restore-unsaved-buffers-title",
        "Whether or not to restore unsaved buffers on restart." => {
            "settings-general-restore-unsaved-buffers-description"
        }
        "Restore On Startup" => "settings-general-restore-on-startup-title",
        "What to restore from the previous session when opening Flint." => {
            "settings-general-restore-on-startup-description"
        }
        "Preview Channel" => "settings-general-preview-channel-title",
        "Which settings should be activated only in Preview build of Flint." => {
            "settings-general-preview-channel-description"
        }
        "Settings Profiles" => "settings-general-settings-profiles-title",
        "Any number of settings profiles that are temporarily applied on top of your existing user settings." => {
            "settings-general-settings-profiles-description"
        }
        "Feature Flags" => "settings-feature-flags",
        "Performance Profiler" => "settings-performance-profiler",
        "Collect timing data for foreground and background executor tasks so they can be inspected via `flint: open performance profiler`. May lead to increased memory usage." => {
            "settings-collect-timing-data-for-foreground-and"
        }
        "Instrumentation" => "settings-instrumentation",
        "Base Keymap" => "settings-base-keymap",
        "Edit Keybindings" => "settings-edit-keybindings",
        "Helix Mode" => "settings-helix-mode",
        "Vim Mode" => "settings-vim-mode",
        "Enable Helix mode and key bindings." => "settings-enable-helix-mode-and-key-bindings",
        "Enable Vim mode and key bindings." => "settings-enable-vim-mode-and-key-bindings",
        "The name of a base set of key bindings to use." => {
            "settings-the-name-of-a-base-set-of-key-bindings-to-use"
        }
        "Customize keybindings in the keymap editor." => {
            "settings-customize-keybindings-in-the-keymap-editor"
        }
        "Open Keymap" => "settings-open-keymap",
        "Keybindings" => "settings-keybindings",
        "Modal Editing" => "settings-modal-editing",
        "Debounce" => "settings-debounce",
        "Enabled" => "settings-enabled",
        "File Type Associations" => "settings-file-type-associations",
        "Include Warnings" => "settings-include-warnings",
        "Max Severity" => "settings-max-severity",
        "Minimum Column" => "settings-minimum-column",
        "Padding" => "settings-padding",
        "Update Debounce" => "settings-update-debounce",
        "A mapping from languages to files and file extensions that should be treated as that language." => {
            "settings-a-mapping-from-languages-to-files-and-file"
        }
        "Minimum time to wait before pulling diagnostics from the language server(s)." => {
            "settings-minimum-time-to-wait-before-pulling-diagnostics"
        }
        "The amount of padding between the end of the source line and the start of the inline diagnostic." => {
            "settings-the-amount-of-padding-between-the-end-of-the"
        }
        "The debounce delay before querying highlights from the language." => {
            "settings-the-debounce-delay-before-querying-highlights"
        }
        "The delay in milliseconds to show inline diagnostics after the last diagnostic update." => {
            "settings-the-delay-in-milliseconds-to-show-inline"
        }
        "The minimum column at which to display inline diagnostics." => {
            "settings-the-minimum-column-at-which-to-display-inline"
        }
        "Whether to pull for language server-powered diagnostics or not." => {
            "settings-whether-to-pull-for-language-server-powered"
        }
        "Whether to show diagnostics inline or not." => {
            "settings-whether-to-show-diagnostics-inline-or-not"
        }
        "Whether to show warnings or not by default." => {
            "settings-whether-to-show-warnings-or-not-by-default"
        }
        "Which level to use to filter out diagnostics displayed in the editor." => {
            "settings-which-level-to-use-to-filter-out-diagnostics"
        }
        "Diagnostics" => "settings-diagnostics",
        "File Types" => "settings-file-types",
        "Inline Diagnostics" => "settings-inline-diagnostics",
        "LSP Highlights" => "settings-lsp-highlights",
        "LSP Pull Diagnostics" => "settings-lsp-pull-diagnostics",
        "File Finder" => "settings-file-finder",
        "File Scan" => "settings-file-scan",
        "Case Sensitive" => "settings-case-sensitive",
        "Center on Match" => "settings-center-on-match",
        "Close on File Delete" => "settings-close-on-file-delete",
        "File Icons" => "settings-file-icons",
        "File Scan Exclusions" => "settings-file-scan-exclusions",
        "File Scan Inclusions" => "settings-file-scan-inclusions",
        "Include Ignored" => "settings-include-ignored",
        "Include Ignored in Search" => "settings-include-ignored-in-search",
        "Modal Max Width" => "settings-modal-max-width",
        "Regex" => "settings-regex",
        "Restore File State" => "settings-restore-file-state",
        "Scan Symbolic Links" => "settings-scan-symbolic-links",
        "Search Wrap" => "settings-search-wrap",
        "Seed Search Query From Cursor" => "settings-seed-search-query-from-cursor",
        "Skip Focus For Active In Search" => "settings-skip-focus-for-active-in-search",
        "Use Smartcase Search" => "settings-use-smartcase-search",
        "Whole Word" => "settings-whole-word",
        "Automatically close files that have been deleted." => {
            "settings-automatically-close-files-that-have-been"
        }
        "Determines how much space the file finder can take up in relation to the available window width." => {
            "settings-determines-how-much-space-the-file-finder-can"
        }
        "Files or globs of files that will be excluded by Flint entirely. They will be skipped during file scans, file searches, and not be displayed in the project file tree. Takes precedence over \"File Scan Inclusions\"" => {
            "settings-files-or-globs-of-files-that-will-be-excluded"
        }
        "Files or globs of files that will be included by Flint, even when ignored by git. This is useful for files that are not tracked by git, but are still important to your project. Note that globs that are overly broad can slow down Flint's file scanning. \"File Scan Exclusions\" takes precedence over these inclusions" => {
            "settings-files-or-globs-of-files-that-will-be-included"
        }
        "Include ignored files in search results by default." => {
            "settings-include-ignored-files-in-search-results-by"
        }
        "Restore previous file state when reopening." => {
            "settings-restore-previous-file-state-when-reopening"
        }
        "Search case-sensitively by default." => "settings-search-case-sensitively-by-default",
        "Search for whole words by default." => "settings-search-for-whole-words-by-default",
        "Show file icons in the file finder." => "settings-show-file-icons-in-the-file-finder",
        "Use gitignored files when searching." => "settings-use-gitignored-files-when-searching",
        "Use regex search by default." => "settings-use-regex-search-by-default",
        "When to populate a new search's query based on the text under the cursor." => {
            "settings-when-to-populate-a-new-searchs-query-based-on"
        }
        "When to scan content of linked directories" => {
            "settings-when-to-scan-content-of-linked-directories"
        }
        "Whether the editor search results will loop." => {
            "settings-whether-the-editor-search-results-will-loop"
        }
        "Whether the file finder should skip focus for the active file in search results." => {
            "settings-whether-the-file-finder-should-skip-focus-for"
        }
        "Whether to automatically enable case-sensitive search based on the search query." => {
            "settings-whether-to-automatically-enable-case-sensitive"
        }
        "Whether to center the current match in the editor" => {
            "settings-whether-to-center-the-current-match-in-the"
        }
        "Branch Picker" => "settings-branch-picker",
        "Commit Message Generator" => "settings-commit-message-generator",
        "Git Blame View" => "settings-git-blame-view",
        "Git Gutter" => "settings-git-gutter",
        "Git Hunks" => "settings-git-hunks",
        "Git Integration" => "settings-git-integration",
        "Inline Git Blame" => "settings-inline-git-blame",
        "Arguments" => "settings-arguments",
        "Delay" => "settings-delay",
        "Disable Git Integration" => "settings-disable-git-integration",
        "Enable Git Diff" => "settings-enable-git-diff",
        "Enable Git Status" => "settings-enable-git-status",
        "Environment Variables" => "settings-environment-variables",
        "Hunk Style" => "settings-hunk-style",
        "Instructions" => "settings-instructions",
        "Max Diff Bytes" => "settings-max-diff-bytes",
        "Manage Skill" => "settings-manage-skill",
        "Model" => "settings-model",
        "Path Style" => "settings-path-style",
        "Show Author Name" => "settings-show-author-name",
        "Show Avatar" => "settings-show-avatar",
        "Show Commit Summary" => "settings-show-commit-summary",
        "Show Stage/Restore Buttons" => "settings-show-stage-restore-buttons",
        "Timeout (Seconds)" => "settings-timeout-seconds",
        "Visibility" => "settings-visibility",
        "Working Directory" => "settings-working-directory",
        "Additional user instructions for commit message generation." => {
            "settings-additional-user-instructions-for-commit-message"
        }
        "Arguments passed to the generator command." => {
            "settings-arguments-passed-to-the-generator-command"
        }
        "Control whether Git status is shown in the editor's gutter." => {
            "settings-control-whether-git-status-is-shown-in-the"
        }
        "Debounce threshold in milliseconds after which changes are reflected in the Git gutter." => {
            "settings-debounce-threshold-in-milliseconds-after-which"
        }
        "Disable all Git integration features in Flint." => {
            "settings-disable-all-git-integration-features-in-flint"
        }
        "Environment variables added to the generator command." => {
            "settings-environment-variables-added-to-the-generator"
        }
        "How Git hunks are displayed visually in the editor." => {
            "settings-how-git-hunks-are-displayed-visually-in-the"
        }
        "Maximum diff bytes included in the prompt before compression." => {
            "settings-maximum-diff-bytes-included-in-the-prompt"
        }
        "Model passed to the generator: as \"--model <value>\" for the Claude and Pi agents, or \"-m <value>\" for the Codex agent. Leave blank to use the command's own default model." => {
            "settings-model-passed-to-the-generator-as-model-value"
        }
        "Padding between the end of the source line and the start of the inline blame in columns." => {
            "settings-padding-between-the-end-of-the-source-line-and"
        }
        "Preview, install, update, or uninstall the optional Flint control skill for Codex, Pi, OpenCode, or Claude Code. Flint does not add this skill to global instruction files." => {
            "settings-preview-install-update-or-uninstall-flint-control-skill"
        }
        "Should the name or path be displayed first in the git view." => {
            "settings-should-the-name-or-path-be-displayed-first-in"
        }
        "Show Git diff information in the editor." => {
            "settings-show-git-diff-information-in-the-editor"
        }
        "Show Git status information in the editor." => {
            "settings-show-git-status-information-in-the-editor"
        }
        "Show author name as part of the commit information in branch picker." => {
            "settings-show-author-name-as-part-of-the-commit"
        }
        "Show commit summary as part of the inline blame." => {
            "settings-show-commit-summary-as-part-of-the-inline-blame"
        }
        "Show the avatar of the author of the commit." => {
            "settings-show-the-avatar-of-the-author-of-the-commit"
        }
        "The delay after which the inline blame information is shown." => {
            "settings-the-delay-after-which-the-inline-blame"
        }
        "The minimum column number at which to show the inline blame information." => {
            "settings-the-minimum-column-number-at-which-to-show-the"
        }
        "Timeout in seconds before the generator command is killed." => {
            "settings-timeout-in-seconds-before-the-generator-command"
        }
        "Whether or not to show Git blame data inline in the currently focused line." => {
            "settings-whether-or-not-to-show-git-blame-data-inline-in"
        }
        "Whether to show the stage and restore buttons on diff hunks." => {
            "settings-whether-to-show-the-stage-and-restore-buttons"
        }
        "Which coding agent generates commit messages. Selecting one fills Command, Arguments, and Model with the preset tested for that agent, overwriting any custom values." => {
            "settings-which-coding-agent-generates-commit-messages"
        }
        "Working directory for the generator command. Defaults to the repository root." => {
            "settings-working-directory-for-the-generator-command"
        }
        "Advanced Settings" => "settings-advanced-settings",
        "Behavior Settings" => "settings-behavior-settings",
        "Display Settings" => "settings-display-settings",
        "Environment" => "settings-environment",
        "Font" => "settings-font",
        "Flint Control Skill" => "settings-flint-control-skill",
        "Layout Settings" => "settings-layout-settings",
        "Scrollbar" => "settings-scrollbar",
        "Toolbar" => "settings-toolbar",
        "Alternate Scroll" => "settings-alternate-scroll",
        "Audible Bell" => "settings-audible-bell",
        "Breadcrumbs" => "settings-breadcrumbs",
        "Copy On Select" => "settings-copy-on-select",
        "Cursor Blinking" => "settings-cursor-blinking",
        "Cursor Shape" => "settings-cursor-shape",
        "Dedicated SSH Connection" => "settings-dedicated-ssh-connection",
        "Default Height" => "settings-default-height",
        "Default Width" => "settings-default-width",
        "Detect Virtual Environment" => "settings-detect-virtual-environment",
        "Directory" => "settings-directory",
        "Font Fallbacks" => "settings-font-fallbacks",
        "Font Family" => "settings-font-family",
        "Font Features" => "settings-font-features",
        "Font Size" => "settings-font-size",
        "Font Weight" => "settings-font-weight",
        "Keep Selection On Copy" => "settings-keep-selection-on-copy",
        "Line Height" => "settings-line-height",
        "Max Scroll History Lines" => "settings-max-scroll-history-lines",
        "Minimum Contrast" => "settings-minimum-contrast",
        "Open Links In Mouse Mode" => "settings-open-links-in-mouse-mode",
        "Option As Meta" => "settings-option-as-meta",
        "Program" => "settings-program",
        "Scroll Multiplier" => "settings-scroll-multiplier",
        "Shell" => "settings-shell",
        "Show Scrollbar" => "settings-show-scrollbar",
        "Title Override" => "settings-title-override",
        "Activates the Python virtual environment, if one is found, in the terminal's working directory." => {
            "settings-activates-the-python-virtual-environment-if-one"
        }
        "An optional string to override the title of the terminal tab." => {
            "settings-an-optional-string-to-override-the-title-of-the"
        }
        "Default cursor shape for the terminal (bar, block, underline, or hollow)." => {
            "settings-default-cursor-shape-for-the-terminal-bar-block"
        }
        "Default height when the terminal is docked to the bottom (in pixels)." => {
            "settings-default-height-when-the-terminal-is-docked-to"
        }
        "Default width when the terminal is docked to the left or right (in pixels)." => {
            "settings-default-width-when-the-terminal-is-docked-to"
        }
        "Display the terminal title in breadcrumbs inside the terminal pane." => {
            "settings-display-the-terminal-title-in-breadcrumbs"
        }
        "Font fallbacks for terminal text. If not set, defaults to buffer font fallbacks." => {
            "settings-font-fallbacks-for-terminal-text-if-not-set"
        }
        "Font family for terminal text. If not set, defaults to buffer font family." => {
            "settings-font-family-for-terminal-text-if-not-set"
        }
        "Font features for terminal text." => "settings-font-features-for-terminal-text",
        "Font size for terminal text. If not set, defaults to buffer font size." => {
            "settings-font-size-for-terminal-text-if-not-set-defaults"
        }
        "Font weight for terminal text in CSS weight units (100-900)." => {
            "settings-font-weight-for-terminal-text-in-css-weight"
        }
        "Give terminals opened on an SSH remote their own connection instead of sharing the multiplexed connection used by file sync, language servers, and agents. Avoids typing lag from that traffic, at the cost of an extra connection (and a re-authentication if the host does not use key/agent auth). No effect on local, WSL, or Docker terminals." => {
            "settings-give-terminals-opened-on-an-ssh-remote-their"
        }
        "Key-value pairs to add to the terminal's environment." => {
            "settings-key-value-pairs-to-add-to-the-terminals"
        }
        "Line height for terminal text." => "settings-line-height-for-terminal-text",
        "Maximum number of lines to keep in scrollback history (max: 100,000; 0 disables scrolling)." => {
            "settings-maximum-number-of-lines-to-keep-in-scrollback"
        }
        "Sets the cursor blinking behavior in the terminal." => {
            "settings-sets-the-cursor-blinking-behavior-in-the"
        }
        "The arguments to pass to the shell program." => {
            "settings-the-arguments-to-pass-to-the-shell-program"
        }
        "The directory path to use (will be shell expanded)." => {
            "settings-the-directory-path-to-use-will-be-shell"
        }
        "The minimum APCA perceptual contrast between foreground and background colors (0-106)." => {
            "settings-the-minimum-apca-perceptual-contrast-between"
        }
        "The multiplier for scrolling in the terminal with the mouse wheel" => {
            "settings-the-multiplier-for-scrolling-in-the-terminal"
        }
        "The shell program to run." => "settings-the-shell-program-to-run",
        "The shell program to use." => "settings-the-shell-program-to-use",
        "What shell to use when opening a terminal." => {
            "settings-what-shell-to-use-when-opening-a-terminal"
        }
        "What working directory to use when launching the terminal." => {
            "settings-what-working-directory-to-use-when-launching"
        }
        "When to show the scrollbar in the terminal." => {
            "settings-when-to-show-the-scrollbar-in-the-terminal"
        }
        "Whether alternate scroll mode is active by default (converts mouse scroll to arrow keys in apps like Vim)." => {
            "settings-whether-alternate-scroll-mode-is-active-by"
        }
        "Whether cmd-click (ctrl-click on Linux and Windows) opens hyperlinks when the terminal application has enabled mouse reporting. When disabled, these clicks are forwarded to the application." => {
            "settings-whether-cmd-click-ctrl-click-on-linux-and"
        }
        "Whether selecting text in the terminal automatically copies to the system clipboard." => {
            "settings-whether-selecting-text-in-the-terminal"
        }
        "Whether the option key behaves as the meta key." => {
            "settings-whether-the-option-key-behaves-as-the-meta-key"
        }
        "Whether to keep the text selection after copying it to the clipboard." => {
            "settings-whether-to-keep-the-text-selection-after"
        }
        "Whether to play a sound when the BEL character (`\\a`, `0x07`) is printed" => {
            "settings-whether-to-play-a-sound-when-the-bel-character"
        }
        "Buffer Font" => "settings-buffer-font",
        "Cursor" => "settings-cursor",
        "Guides" => "settings-guides",
        "Highlighting" => "settings-highlighting",
        "Text Rendering" => "settings-text-rendering",
        "Theme" => "settings-theme",
        "UI Font" => "settings-ui-font",
        "Current Line Highlight" => "settings-current-line-highlight",
        "Cursor Blink" => "settings-cursor-blink",
        "Custom Line Height" => "settings-custom-line-height",
        "Dark Icon Theme" => "settings-dark-icon-theme",
        "Dark Theme" => "settings-dark-theme",
        "Hide Mouse" => "settings-hide-mouse",
        "Icon Theme" => "settings-icon-theme",
        "Icon Theme Name" => "settings-icon-theme-name",
        "Light Icon Theme" => "settings-light-icon-theme",
        "Light Theme" => "settings-light-theme",
        "Minimum Contrast For Highlights" => "settings-minimum-contrast-for-highlights",
        "Mode" => "settings-mode",
        "Multi Cursor Modifier" => "settings-multi-cursor-modifier",
        "Rounded Selection" => "settings-rounded-selection",
        "Selection Highlight" => "settings-selection-highlight",
        "Show Wrap Guides" => "settings-show-wrap-guides",
        "Text Rendering Mode" => "settings-text-rendering-mode",
        "Theme Mode" => "settings-theme-mode",
        "Theme Name" => "settings-theme-name",
        "Unnecessary Code Fade" => "settings-unnecessary-code-fade",
        "Wrap Guides" => "settings-wrap-guides",
        "Character counts at which to show wrap guides." => {
            "settings-character-counts-at-which-to-show-wrap-guides"
        }
        "Choose a static, fixed theme or dynamically select themes based on appearance and light/dark modes." => {
            "settings-choose-a-static-fixed-theme-or-dynamically"
        }
        "Choose whether to use the selected light or dark icon theme or to follow your OS appearance configuration." => {
            "settings-choose-whether-to-use-the-selected-light-or"
        }
        "Choose whether to use the selected light or dark theme or to follow your OS appearance configuration." => {
            "settings-choose-whether-to-use-the-selected-light-or-2"
        }
        "Cursor shape for the editor." => "settings-cursor-shape-for-the-editor",
        "Custom line height value (must be at least 1.0)." => {
            "settings-custom-line-height-value-must-be-at-least-1-0"
        }
        "Font family for UI elements." => "settings-font-family-for-ui-elements",
        "Font family for editor text." => "settings-font-family-for-editor-text",
        "Font size for UI elements." => "settings-font-size-for-ui-elements",
        "Font size for editor text." => "settings-font-size-for-editor-text",
        "Font weight for UI elements (100-900)." => "settings-font-weight-for-ui-elements-100-900",
        "Font weight for editor text (100-900)." => "settings-font-weight-for-editor-text-100-900",
        "Highlight all occurrences of selected text." => {
            "settings-highlight-all-occurrences-of-selected-text"
        }
        "How much to fade out unused code (0.0 - 0.9)." => {
            "settings-how-much-to-fade-out-unused-code-0-0-0-9"
        }
        "How to highlight the current line." => "settings-how-to-highlight-the-current-line",
        "Line height for editor text." => "settings-line-height-for-editor-text",
        "Modifier key for adding multiple cursors." => {
            "settings-modifier-key-for-adding-multiple-cursors"
        }
        "Show wrap guides (vertical rulers)." => "settings-show-wrap-guides-vertical-rulers",
        "The OpenType features to enable for rendering in UI elements." => {
            "settings-the-opentype-features-to-enable-for-rendering"
        }
        "The OpenType features to enable for rendering in text buffers." => {
            "settings-the-opentype-features-to-enable-for-rendering-2"
        }
        "The custom set of icons Flint will associate with files and directories." => {
            "settings-the-custom-set-of-icons-flint-will-associate"
        }
        "The font fallbacks to use for rendering in text buffers." => {
            "settings-the-font-fallbacks-to-use-for-rendering-in-text"
        }
        "The font fallbacks to use for rendering in the UI." => {
            "settings-the-font-fallbacks-to-use-for-rendering-in-the"
        }
        "The icon theme to use when mode is set to dark, or when mode is set to system and it is in dark mode." => {
            "settings-the-icon-theme-to-use-when-mode-is-set-to-dark"
        }
        "The icon theme to use when mode is set to light, or when mode is set to system and it is in light mode." => {
            "settings-the-icon-theme-to-use-when-mode-is-set-to-light"
        }
        "The minimum APCA perceptual contrast to maintain when rendering text over highlight backgrounds." => {
            "settings-the-minimum-apca-perceptual-contrast-to"
        }
        "The name of your selected icon theme." => "settings-the-name-of-your-selected-icon-theme",
        "The name of your selected theme." => "settings-the-name-of-your-selected-theme",
        "The text rendering mode to use." => "settings-the-text-rendering-mode-to-use",
        "The theme to use when mode is set to dark, or when mode is set to system and it is in dark mode." => {
            "settings-the-theme-to-use-when-mode-is-set-to-dark-or"
        }
        "The theme to use when mode is set to light, or when mode is set to system and it is in light mode." => {
            "settings-the-theme-to-use-when-mode-is-set-to-light-or"
        }
        "When to hide the mouse cursor." => "settings-when-to-hide-the-mouse-cursor",
        "Whether the cursor blinks in the editor." => {
            "settings-whether-the-cursor-blinks-in-the-editor"
        }
        "Whether the text selection should have rounded corners." => {
            "settings-whether-the-text-selection-should-have-rounded"
        }
        "Layout" => "settings-layout",
        "Pane Modifiers" => "settings-pane-modifiers",
        "Pane Split Direction" => "settings-pane-split-direction",
        "Preview Tabs" => "settings-preview-tabs",
        "Status Bar" => "settings-status-bar",
        "Tab Bar" => "settings-tab-bar",
        "Tab Settings" => "settings-tab-settings",
        "Title Bar" => "settings-title-bar",
        "Window" => "settings-window",
        "Activate On Close" => "settings-activate-on-close",
        "Active Encoding Button" => "settings-active-encoding-button",
        "Active File Name" => "settings-active-file-name",
        "Active Language Button" => "settings-active-language-button",
        "Border Size" => "settings-border-size",
        "Bottom Dock Layout" => "settings-bottom-dock-layout",
        "Button Layout" => "settings-button-layout",
        "Centered Layout Left Padding" => "settings-centered-layout-left-padding",
        "Centered Layout Right Padding" => "settings-centered-layout-right-padding",
        "Cursor Position Button" => "settings-cursor-position-button",
        "Custom Button Layout" => "settings-custom-button-layout",
        "Diagnostics Button" => "settings-diagnostics-button",
        "Enable Keep Preview On Code Navigation" => {
            "settings-enable-keep-preview-on-code-navigation"
        }
        "Enable Preview File From Code Navigation" => {
            "settings-enable-preview-file-from-code-navigation"
        }
        "Enable Preview From File Finder" => "settings-enable-preview-from-file-finder",
        "Enable Preview From Multibuffer" => "settings-enable-preview-from-multibuffer",
        "Enable Preview From Project Panel" => "settings-enable-preview-from-project-panel",
        "Enable Preview Multibuffer From Code Navigation" => {
            "settings-enable-preview-multibuffer-from-code-navigation"
        }
        "Focus Follows Mouse" => "settings-focus-follows-mouse",
        "Focus Follows Mouse Debounce ms" => "settings-focus-follows-mouse-debounce-ms",
        "Horizontal Split Direction" => "settings-horizontal-split-direction",
        "Inactive Opacity" => "settings-inactive-opacity",
        "Line Endings Button" => "settings-line-endings-button",
        "Maximum Tabs" => "settings-maximum-tabs",
        "Pinned Tabs Layout" => "settings-pinned-tabs-layout",
        "Preview Tabs Enabled" => "settings-preview-tabs-enabled",
        "Project Panel Button" => "settings-project-panel-button",
        "Project Search Button" => "settings-project-search-button",
        "Show Branch Name" => "settings-show-branch-name",
        "Show Branch Status Icon" => "settings-show-branch-status-icon",
        "Show Close Button" => "settings-show-close-button",
        "Show File Icons In Tabs" => "settings-show-file-icons-in-tabs",
        "Show Git Status In Tabs" => "settings-show-git-status-in-tabs",
        "Show Menus" => "settings-show-menus",
        "Show Navigation History Buttons" => "settings-show-navigation-history-buttons",
        "Show Onboarding Banner" => "settings-show-onboarding-banner",
        "Show Project Items" => "settings-show-project-items",
        "Show Tab Bar" => "settings-show-tab-bar",
        "Show Tab Bar Buttons" => "settings-show-tab-bar-buttons",
        "Tab Close Position" => "settings-tab-close-position",
        "Tab Show Diagnostics" => "settings-tab-show-diagnostics",
        "Terminal Button" => "settings-terminal-button",
        "Use System Window Tabs" => "settings-use-system-window-tabs",
        "Vertical Split Direction" => "settings-vertical-split-direction",
        "Window Decorations" => "settings-window-decorations",
        "Zoomed Padding" => "settings-zoomed-padding",
        "(Linux only) choose how window control buttons are laid out in the titlebar." => {
            "settings-linux-only-choose-how-window-control-buttons"
        }
        "(Linux only) whether Flint or your compositor should draw window decorations." => {
            "settings-linux-only-whether-flint-or-your-compositor"
        }
        "(macOS only) whether to allow Windows to tab together." => {
            "settings-macos-only-whether-to-allow-windows-to-tab"
        }
        "Amount of time to wait before changing focus." => {
            "settings-amount-of-time-to-wait-before-changing-focus"
        }
        "Control when to show the active encoding in the status bar." => {
            "settings-control-when-to-show-the-active-encoding-in-the"
        }
        "Controls the appearance behavior of the tab's close button." => {
            "settings-controls-the-appearance-behavior-of-the-tabs"
        }
        "Direction to split horizontally." => "settings-direction-to-split-horizontally",
        "Direction to split vertically." => "settings-direction-to-split-vertically",
        "GNOME-style layout string such as \"close:minimize,maximize\"." => {
            "settings-gnome-style-layout-string-such-as-close"
        }
        "Layout mode for the bottom dock." => "settings-layout-mode-for-the-bottom-dock",
        "Left padding for centered layout." => "settings-left-padding-for-centered-layout",
        "Maximum open tabs in a pane. Will not close an unsaved tab." => {
            "settings-maximum-open-tabs-in-a-pane-will-not-close-an"
        }
        "Opacity of inactive panels (0.0 - 1.0)." => "settings-opacity-of-inactive-panels-0-0-1-0",
        "Position of the close button in a tab." => {
            "settings-position-of-the-close-button-in-a-tab"
        }
        "Right padding for centered layout." => "settings-right-padding-for-centered-layout",
        "Show banners announcing new features in the titlebar." => {
            "settings-show-banners-announcing-new-features-in-the"
        }
        "Show git status indicators on the branch icon in the titlebar." => {
            "settings-show-git-status-indicators-on-the-branch-icon"
        }
        "Show opened editors as preview tabs." => "settings-show-opened-editors-as-preview-tabs",
        "Show padding for zoomed panes." => "settings-show-padding-for-zoomed-panes",
        "Show pinned tabs in a separate row above unpinned tabs." => {
            "settings-show-pinned-tabs-in-a-separate-row-above"
        }
        "Show the Git file status on a tab item." => {
            "settings-show-the-git-file-status-on-a-tab-item"
        }
        "Show the active language button in the status bar." => {
            "settings-show-the-active-language-button-in-the-status"
        }
        "Show the active line endings button in the status bar." => {
            "settings-show-the-active-line-endings-button-in-the"
        }
        "Show the branch name button in the titlebar." => {
            "settings-show-the-branch-name-button-in-the-titlebar"
        }
        "Show the cursor position button in the status bar." => {
            "settings-show-the-cursor-position-button-in-the-status"
        }
        "Show the file icon for a tab." => "settings-show-the-file-icon-for-a-tab",
        "Show the menus in the titlebar." => "settings-show-the-menus-in-the-titlebar",
        "Show the name of the active file in the status bar." => {
            "settings-show-the-name-of-the-active-file-in-the-status"
        }
        "Show the navigation history buttons in the tab bar." => {
            "settings-show-the-navigation-history-buttons-in-the-tab"
        }
        "Show the project diagnostics button in the status bar." => {
            "settings-show-the-project-diagnostics-button-in-the"
        }
        "Show the project host and name in the titlebar." => {
            "settings-show-the-project-host-and-name-in-the-titlebar"
        }
        "Show the project panel button in the status bar." => {
            "settings-show-the-project-panel-button-in-the-status-bar"
        }
        "Show the project search button in the status bar." => {
            "settings-show-the-project-search-button-in-the-status"
        }
        "Show the tab bar buttons (New, Split Pane, Zoom)." => {
            "settings-show-the-tab-bar-buttons-new-split-pane-zoom"
        }
        "Show the tab bar in the editor." => "settings-show-the-tab-bar-in-the-editor",
        "Show the terminal button in the status bar." => {
            "settings-show-the-terminal-button-in-the-status-bar"
        }
        "Size of the border surrounding the active pane." => {
            "settings-size-of-the-border-surrounding-the-active-pane"
        }
        "What to do after closing the current tab." => {
            "settings-what-to-do-after-closing-the-current-tab"
        }
        "Whether to change focus to a pane when the mouse hovers over it." => {
            "settings-whether-to-change-focus-to-a-pane-when-the"
        }
        "Whether to keep tabs in preview mode when code navigation is used to navigate away from them. If `enable_preview_file_from_code_navigation` or `enable_preview_multibuffer_from_code_navigation` is also true, the new tab may replace the existing one." => {
            "settings-whether-to-keep-tabs-in-preview-mode-when-code"
        }
        "Whether to open tabs in preview mode when code navigation is used to open a multibuffer." => {
            "settings-whether-to-open-tabs-in-preview-mode-when-code"
        }
        "Whether to open tabs in preview mode when code navigation is used to open a single file." => {
            "settings-whether-to-open-tabs-in-preview-mode-when-code-2"
        }
        "Whether to open tabs in preview mode when opened from a multibuffer." => {
            "settings-whether-to-open-tabs-in-preview-mode-when"
        }
        "Whether to open tabs in preview mode when opened from the project panel with a single click." => {
            "settings-whether-to-open-tabs-in-preview-mode-when-2"
        }
        "Whether to open tabs in preview mode when selected from the file finder." => {
            "settings-whether-to-open-tabs-in-preview-mode-when-3"
        }
        "Which files containing diagnostic errors/warnings to mark in the tabs." => {
            "settings-which-files-containing-diagnostic-errors"
        }
        "Agent Threads Panel" => "settings-agent-threads-panel",
        "Git Panel" => "settings-git-panel",
        "Outline Panel" => "settings-outline-panel",
        "Project Panel" => "settings-project-panel",
        "Terminal Panel" => "settings-terminal-panel",
        "Agent Control" => "settings-agent-control",
        "Agent Threads Panel Dock" => "settings-agent-threads-panel-dock",
        "Auto Fold Directories" => "settings-auto-fold-directories",
        "Auto Open Files On Create" => "settings-auto-open-files-on-create",
        "Auto Open Files On Drop" => "settings-auto-open-files-on-drop",
        "Auto Open Files On Paste" => "settings-auto-open-files-on-paste",
        "Auto Reveal Entries" => "settings-auto-reveal-entries",
        "Bold Folder Labels" => "settings-bold-folder-labels",
        "Claude Initialization Command" => "settings-claude-initialization-command",
        "Codex Initialization Command" => "settings-codex-initialization-command",
        "Collapse Untracked Diff" => "settings-collapse-untracked-diff",
        "Commit Title Max Length" => "settings-commit-title-max-length",
        "Diagnostic Badges" => "settings-diagnostic-badges",
        "Diff Stats" => "settings-diff-stats",
        "Drag and Drop" => "settings-drag-and-drop",
        "Entry Spacing" => "settings-entry-spacing",
        "Fallback Branch Name" => "settings-fallback-branch-name",
        "Folder Icons" => "settings-folder-icons",
        "Git Panel Button" => "settings-git-panel-button",
        "Git Panel Default Width" => "settings-git-panel-default-width",
        "Git Panel Dock" => "settings-git-panel-dock",
        "Git Panel Group By" => "settings-git-panel-group-by",
        "Git Panel Sort By" => "settings-git-panel-sort-by",
        "Git Panel Status Style" => "settings-git-panel-status-style",
        "Git Status" => "settings-git-status",
        "Git Status Indicator" => "settings-git-status-indicator",
        "Hidden Files" => "settings-hidden-files",
        "Hide .gitignore" => "settings-hide-gitignore",
        "Hide Claude" => "settings-hide-claude",
        "Hide Codex" => "settings-hide-codex",
        "Hide Hidden" => "settings-hide-hidden",
        "Hide OpenCode" => "settings-hide-opencode",
        "Hide Pi" => "settings-hide-pi",
        "Hide Root" => "settings-hide-root",
        "Horizontal Scroll" => "settings-horizontal-scroll",
        "Indent Size" => "settings-indent-size",
        "Max Visible Threads Per Agent" => "settings-max-visible-threads-per-agent",
        "Notify When Finished" => "settings-notify-when-finished",
        "OpenCode Initialization Command" => "settings-opencode-initialization-command",
        "Outline Panel Button" => "settings-outline-panel-button",
        "Outline Panel Default Width" => "settings-outline-panel-default-width",
        "Outline Panel Dock" => "settings-outline-panel-dock",
        "Pi Initialization Command" => "settings-pi-initialization-command",
        "Project Panel Default Width" => "settings-project-panel-default-width",
        "Project Panel Dock" => "settings-project-panel-dock",
        "Reopen Sessions" => "settings-reopen-sessions",
        "Scroll Bar" => "settings-scroll-bar",
        "Show Count Badge" => "settings-show-count-badge",
        "Show Diagnostics" => "settings-show-diagnostics",
        "Show Indent Guides" => "settings-show-indent-guides",
        "Show Plan Usage" => "settings-show-plan-usage",
        "Sort Mode" => "settings-sort-mode",
        "Sort Order" => "settings-sort-order",
        "Starts Open" => "settings-starts-open",
        "Sticky Scroll" => "settings-sticky-scroll",
        "Terminal Location" => "settings-terminal-location",
        "Terminal Panel Flexible Sizing" => "settings-terminal-panel-flexible-sizing",
        "Tree View" => "settings-tree-view",
        "Amount of indentation for nested items." => {
            "settings-amount-of-indentation-for-nested-items"
        }
        "Default branch name will be when init.defaultbranch is not set in Git." => {
            "settings-default-branch-name-will-be-when-init"
        }
        "Default width of the Git panel in pixels." => {
            "settings-default-width-of-the-git-panel-in-pixels"
        }
        "Default width of the outline panel in pixels." => {
            "settings-default-width-of-the-outline-panel-in-pixels"
        }
        "Default width of the project panel in pixels." => {
            "settings-default-width-of-the-project-panel-in-pixels"
        }
        "Enable to show entries in tree view list, disable to show in flat view list." => {
            "settings-enable-to-show-entries-in-tree-view-list"
        }
        "Globs to match files that will be considered \"hidden\" and can be hidden from the project panel." => {
            "settings-globs-to-match-files-that-will-be-considered"
        }
        "Hide the Claude section from the Agent Threads panel." => {
            "settings-hide-the-claude-section-from-the-agent-threads"
        }
        "Hide the Codex section from the Agent Threads panel." => {
            "settings-hide-the-codex-section-from-the-agent-threads"
        }
        "Hide the OpenCode section from the Agent Threads panel." => {
            "settings-hide-the-opencode-section-from-the-agent"
        }
        "Hide the Pi section from the Agent Threads panel." => {
            "settings-hide-the-pi-section-from-the-agent-threads"
        }
        "How and when the scrollbar should be displayed." => {
            "settings-how-and-when-the-scrollbar-should-be-displayed"
        }
        "How changed files are grouped in the Git panel." => {
            "settings-how-changed-files-are-grouped-in-the-git-panel"
        }
        "How entries are sorted within each Git panel group." => {
            "settings-how-entries-are-sorted-within-each-git-panel"
        }
        "How entry statuses are displayed." => "settings-how-entry-statuses-are-displayed",
        "How many threads each agent section shows before a \"Show more\" control is offered." => {
            "settings-how-many-threads-each-agent-section-shows"
        }
        "Maximum length of the commit message title before a warning is shown. Set to 0 to disable." => {
            "settings-maximum-length-of-the-commit-message-title"
        }
        "Shell command to run before Claude starts. Claude starts only when the command succeeds." => {
            "settings-shell-command-to-run-before-claude-starts"
        }
        "Shell command to run before Codex starts. Codex starts only when the command succeeds." => {
            "settings-shell-command-to-run-before-codex-starts-codex"
        }
        "Shell command to run before OpenCode starts. OpenCode starts only when the command succeeds." => {
            "settings-shell-command-to-run-before-opencode-starts"
        }
        "Shell command to run before Pi starts. Pi starts only when the command succeeds." => {
            "settings-shell-command-to-run-before-pi-starts-pi-starts"
        }
        "Show a badge on the terminal panel icon with the count of open terminals." => {
            "settings-show-a-badge-on-the-terminal-panel-icon-with"
        }
        "Show a desktop notification when an agent thread finishes a turn or needs input." => {
            "settings-show-a-desktop-notification-when-an-agent"
        }
        "Show a git status indicator next to file names in the project panel." => {
            "settings-show-a-git-status-indicator-next-to-file-names"
        }
        "Show error and warning count badges next to file names in the project panel." => {
            "settings-show-error-and-warning-count-badges-next-to"
        }
        "Show file icons in the outline panel." => "settings-show-file-icons-in-the-outline-panel",
        "Show file icons in the project panel." => "settings-show-file-icons-in-the-project-panel",
        "Show file icons next to the Git status icon." => {
            "settings-show-file-icons-next-to-the-git-status-icon"
        }
        "Show five-hour and weekly plan usage beside Codex and Claude headings." => {
            "settings-show-five-hour-and-weekly-plan-usage-beside"
        }
        "Show indent guides in the project panel." => {
            "settings-show-indent-guides-in-the-project-panel"
        }
        "Show the Git panel button in the status bar." => {
            "settings-show-the-git-panel-button-in-the-status-bar"
        }
        "Show the Git status in the outline panel." => {
            "settings-show-the-git-status-in-the-outline-panel"
        }
        "Show the Git status in the project panel." => {
            "settings-show-the-git-status-in-the-project-panel"
        }
        "Show the outline panel button in the status bar." => {
            "settings-show-the-outline-panel-button-in-the-status-bar"
        }
        "Show the scrollbar in the project panel." => {
            "settings-show-the-scrollbar-in-the-project-panel"
        }
        "Sort order for entries in the project panel." => {
            "settings-sort-order-for-entries-in-the-project-panel"
        }
        "Spacing between worktree entries in the project panel." => {
            "settings-spacing-between-worktree-entries-in-the-project"
        }
        "When to reopen live resumed agent sessions from the previous app session." => {
            "settings-when-to-reopen-live-resumed-agent-sessions-from"
        }
        "When to show indent guides in the outline panel." => {
            "settings-when-to-show-indent-guides-in-the-outline-panel"
        }
        "Where new terminals open. Center opens them as a tab next to your editor; left/right/bottom docks them in a side panel." => {
            "settings-where-new-terminals-open-center-opens-them-as-a"
        }
        "Where to dock the Agent Threads panel." => {
            "settings-where-to-dock-the-agent-threads-panel"
        }
        "Where to dock the Git panel." => "settings-where-to-dock-the-git-panel",
        "Where to dock the outline panel." => "settings-where-to-dock-the-outline-panel",
        "Where to dock the project panel." => "settings-where-to-dock-the-project-panel",
        "Whether the project panel should open on startup." => {
            "settings-whether-the-project-panel-should-open-on"
        }
        "Whether the terminal panel should use flexible (proportional) sizing when docked to the left or right." => {
            "settings-whether-the-terminal-panel-should-use-flexible"
        }
        "Whether to allow horizontal scrolling in the project panel. When disabled, the view is always locked to the leftmost position and long file names are clipped." => {
            "settings-whether-to-allow-horizontal-scrolling-in-the"
        }
        "Whether to automatically open files after pasting or duplicating them." => {
            "settings-whether-to-automatically-open-files-after"
        }
        "Whether to automatically open files dropped from external sources." => {
            "settings-whether-to-automatically-open-files-dropped"
        }
        "Whether to automatically open newly created files in the editor." => {
            "settings-whether-to-automatically-open-newly-created"
        }
        "Whether to collapse untracked files in the diff panel." => {
            "settings-whether-to-collapse-untracked-files-in-the-diff"
        }
        "Whether to enable drag-and-drop operations in the project panel." => {
            "settings-whether-to-enable-drag-and-drop-operations-in"
        }
        "Whether to fold directories automatically and show compact folders when a directory has only one subdirectory inside." => {
            "settings-whether-to-fold-directories-automatically-and"
        }
        "Whether to fold directories automatically when a directory contains only one subdirectory." => {
            "settings-whether-to-fold-directories-automatically-when"
        }
        "Whether to hide the gitignore entries in the project panel." => {
            "settings-whether-to-hide-the-gitignore-entries-in-the"
        }
        "Whether to hide the hidden entries in the project panel." => {
            "settings-whether-to-hide-the-hidden-entries-in-the"
        }
        "Whether to hide the root entry when only one folder is open in the window." => {
            "settings-whether-to-hide-the-root-entry-when-only-one"
        }
        "Whether to reveal entries in the project panel automatically when a corresponding project entry becomes active." => {
            "settings-whether-to-reveal-entries-in-the-project-panel"
        }
        "Whether to reveal when a corresponding outline entry becomes active." => {
            "settings-whether-to-reveal-when-a-corresponding-outline"
        }
        "Whether to show a badge on the git panel icon with the count of uncommitted changes." => {
            "settings-whether-to-show-a-badge-on-the-git-panel-icon"
        }
        "Whether to show folder icons or chevrons for directories in the git panel." => {
            "settings-whether-to-show-folder-icons-or-chevrons-for"
        }
        "Whether to show folder icons or chevrons for directories in the outline panel." => {
            "settings-whether-to-show-folder-icons-or-chevrons-for-2"
        }
        "Whether to show folder icons or chevrons for directories in the project panel." => {
            "settings-whether-to-show-folder-icons-or-chevrons-for-3"
        }
        "Whether to show folder names with bold text in the project panel." => {
            "settings-whether-to-show-folder-names-with-bold-text-in"
        }
        "Whether to show the addition/deletion change count next to each file in the Git panel." => {
            "settings-whether-to-show-the-addition-deletion-change"
        }
        "Whether to sort file and folder names case-sensitively in the project panel." => {
            "settings-whether-to-sort-file-and-folder-names-case"
        }
        "Whether to stick parent directories at top of the project panel." => {
            "settings-whether-to-stick-parent-directories-at-top-of"
        }
        "Which files containing diagnostic errors/warnings to mark in the project panel." => {
            "settings-which-files-containing-diagnostic-errors-2"
        }
        "Auto Save" => "settings-auto-save",
        "Drag And Drop Selection" => "settings-drag-and-drop-selection",
        "Gutter" => "settings-gutter",
        "Hover Popover" => "settings-hover-popover",
        "Minimap" => "settings-minimap",
        "Multibuffer" => "settings-multibuffer",
        "Scrolling" => "settings-scrolling",
        "Signature Help" => "settings-signature-help",
        "Vim" => "settings-vim",
        "Which-key Menu" => "settings-which-key-menu",
        "Auto Save Mode" => "settings-auto-save-mode",
        "Auto Signature Help" => "settings-auto-signature-help",
        "Autoscroll On Clicks" => "settings-autoscroll-on-clicks",
        "Code Actions" => "settings-code-actions",
        "Cursor Shape - Insert Mode" => "settings-cursor-shape-insert-mode",
        "Cursor Shape - Normal Mode" => "settings-cursor-shape-normal-mode",
        "Cursor Shape - Replace Mode" => "settings-cursor-shape-replace-mode",
        "Cursor Shape - Visual Mode" => "settings-cursor-shape-visual-mode",
        "Cursors" => "settings-cursors",
        "Custom Digraphs" => "settings-custom-digraphs",
        "Default Mode" => "settings-default-mode",
        "Delay (milliseconds)" => "settings-delay-milliseconds",
        "Diff View Style" => "settings-diff-view-style",
        "Display In" => "settings-display-in",
        "Double Click In Multibuffer" => "settings-double-click-in-multibuffer",
        "Excerpt Context Lines" => "settings-excerpt-context-lines",
        "Expand Excerpt Lines" => "settings-expand-excerpt-lines",
        "Expand Outlines With Depth" => "settings-expand-outlines-with-depth",
        "Fast Scroll Sensitivity" => "settings-fast-scroll-sensitivity",
        "Git Diff" => "settings-git-diff",
        "Global Substitution Default" => "settings-global-substitution-default",
        "Hiding Delay" => "settings-hiding-delay",
        "Highlight on Yank Duration" => "settings-highlight-on-yank-duration",
        "Horizontal Scroll Margin" => "settings-horizontal-scroll-margin",
        "Horizontal Scrollbar" => "settings-horizontal-scrollbar",
        "Inline Code Actions" => "settings-inline-code-actions",
        "Max Width Columns" => "settings-max-width-columns",
        "Menu Delay" => "settings-menu-delay",
        "Min Line Number Digits" => "settings-min-line-number-digits",
        "Minimum Split Diff Width" => "settings-minimum-split-diff-width",
        "Mouse Wheel Zoom" => "settings-mouse-wheel-zoom",
        "Quick Actions" => "settings-quick-actions",
        "Regex Search" => "settings-regex-search",
        "Relative Line Numbers" => "settings-relative-line-numbers",
        "Scroll Beyond Last Line" => "settings-scroll-beyond-last-line",
        "Scroll Sensitivity" => "settings-scroll-sensitivity",
        "Search Results" => "settings-search-results",
        "Selected Symbol" => "settings-selected-symbol",
        "Selected Text" => "settings-selected-text",
        "Selections Menu" => "settings-selections-menu",
        "Show" => "settings-show",
        "Show Bookmarks" => "settings-show-bookmarks",
        "Show Breakpoints" => "settings-show-breakpoints",
        "Show Folds" => "settings-show-folds",
        "Show Line Numbers" => "settings-show-line-numbers",
        "Show Runnables" => "settings-show-runnables",
        "Show Signature Help After Edits" => "settings-show-signature-help-after-edits",
        "Show Which-key Menu" => "settings-show-which-key-menu",
        "Snippet Sort Order" => "settings-snippet-sort-order",
        "Sticky" => "settings-sticky",
        "Thumb" => "settings-thumb",
        "Thumb Border" => "settings-thumb-border",
        "Toggle Relative Line Numbers" => "settings-toggle-relative-line-numbers",
        "Use Smartcase Find" => "settings-use-smartcase-find",
        "Use System Clipboard" => "settings-use-system-clipboard",
        "Vertical Scroll Margin" => "settings-vertical-scroll-margin",
        "Vertical Scrollbar" => "settings-vertical-scrollbar",
        "Automatically show a signature help pop-up." => {
            "settings-automatically-show-a-signature-help-pop-up"
        }
        "Border style for the minimap's scrollbar thumb." => {
            "settings-border-style-for-the-minimaps-scrollbar-thumb"
        }
        "Controls line number display in the editor's gutter. \"disabled\" shows absolute line numbers, \"enabled\" shows relative line numbers for each absolute line, and \"wrapped\" shows relative line numbers for every line, absolute or wrapped." => {
            "settings-controls-line-number-display-in-the-editors"
        }
        "Controls when to use system clipboard in Vim mode." => {
            "settings-controls-when-to-use-system-clipboard-in-vim"
        }
        "Cursor shape for insert mode. Inherit uses the editor's cursor shape." => {
            "settings-cursor-shape-for-insert-mode-inherit-uses-the"
        }
        "Cursor shape for normal mode." => "settings-cursor-shape-for-normal-mode",
        "Cursor shape for replace mode." => "settings-cursor-shape-for-replace-mode",
        "Cursor shape for visual mode." => "settings-cursor-shape-for-visual-mode",
        "Custom digraph mappings for Vim mode." => "settings-custom-digraph-mappings-for-vim-mode",
        "Default depth to expand outline items in the current file." => {
            "settings-default-depth-to-expand-outline-items-in-the"
        }
        "Delay in milliseconds before drag and drop selection starts." => {
            "settings-delay-in-milliseconds-before-drag-and-drop"
        }
        "Delay in milliseconds before the which-key menu appears." => {
            "settings-delay-in-milliseconds-before-the-which-key-menu"
        }
        "Determines how snippets are sorted relative to other completion items." => {
            "settings-determines-how-snippets-are-sorted-relative-to"
        }
        "Display the which-key menu with matching bindings while a multi-stroke binding is pending." => {
            "settings-display-the-which-key-menu-with-matching"
        }
        "Duration in milliseconds to highlight yanked text in Vim mode." => {
            "settings-duration-in-milliseconds-to-highlight-yanked"
        }
        "Enable drag and drop selection." => "settings-enable-drag-and-drop-selection",
        "Enable smartcase searching in Vim mode." => {
            "settings-enable-smartcase-searching-in-vim-mode"
        }
        "Fast scroll sensitivity multiplier for both horizontal and vertical scrolling." => {
            "settings-fast-scroll-sensitivity-multiplier-for-both"
        }
        "How many lines of context to provide in multibuffer excerpts by default." => {
            "settings-how-many-lines-of-context-to-provide-in"
        }
        "How many lines to expand the multibuffer excerpts by default." => {
            "settings-how-many-lines-to-expand-the-multibuffer"
        }
        "How to display diffs in the editor." => "settings-how-to-display-diffs-in-the-editor",
        "How to highlight the current line in the minimap." => {
            "settings-how-to-highlight-the-current-line-in-the"
        }
        "Maximum number of columns to display in the minimap." => {
            "settings-maximum-number-of-columns-to-display-in-the"
        }
        "Minimum number of characters to reserve space for in the gutter." => {
            "settings-minimum-number-of-characters-to-reserve-space"
        }
        "Save after inactivity period (in milliseconds)." => {
            "settings-save-after-inactivity-period-in-milliseconds"
        }
        "Scroll sensitivity multiplier for both horizontal and vertical scrolling." => {
            "settings-scroll-sensitivity-multiplier-for-both"
        }
        "Show Git diff indicators in the scrollbar." => {
            "settings-show-git-diff-indicators-in-the-scrollbar"
        }
        "Show bookmarks in the gutter." => "settings-show-bookmarks-in-the-gutter",
        "Show breadcrumbs." => "settings-show-breadcrumbs",
        "Show breakpoints in the gutter." => "settings-show-breakpoints-in-the-gutter",
        "Show buffer search result indicators in the scrollbar." => {
            "settings-show-buffer-search-result-indicators-in-the"
        }
        "Show code action button at start of buffer line." => {
            "settings-show-code-action-button-at-start-of-buffer-line"
        }
        "Show code action buttons in the editor toolbar." => {
            "settings-show-code-action-buttons-in-the-editor-toolbar"
        }
        "Show code folding controls in the gutter." => {
            "settings-show-code-folding-controls-in-the-gutter"
        }
        "Show cursor positions in the scrollbar." => {
            "settings-show-cursor-positions-in-the-scrollbar"
        }
        "Show line numbers in the gutter." => "settings-show-line-numbers-in-the-gutter",
        "Show quick action buttons (e.g., search, selection, editor controls, etc.)." => {
            "settings-show-quick-action-buttons-e-g-search-selection"
        }
        "Show runnable buttons in the gutter." => "settings-show-runnable-buttons-in-the-gutter",
        "Show selected symbol occurrences in the scrollbar." => {
            "settings-show-selected-symbol-occurrences-in-the"
        }
        "Show selected text occurrences in the scrollbar." => {
            "settings-show-selected-text-occurrences-in-the-scrollbar"
        }
        "Show the informational hover box when moving the mouse over symbols in the editor." => {
            "settings-show-the-informational-hover-box-when-moving"
        }
        "Show the selections menu in the editor toolbar." => {
            "settings-show-the-selections-menu-in-the-editor-toolbar"
        }
        "Show the signature help pop-up after completions or bracket pairs are inserted." => {
            "settings-show-the-signature-help-pop-up-after"
        }
        "The default mode when Vim starts." => "settings-the-default-mode-when-vim-starts",
        "The minimum width (in columns) at which the split diff view is used. When the editor is narrower, the diff view automatically switches to unified mode. Set to 0 to disable." => {
            "settings-the-minimum-width-in-columns-at-which-the-split"
        }
        "The number of characters to keep on either side when scrolling with the mouse." => {
            "settings-the-number-of-characters-to-keep-on-either-side"
        }
        "The number of lines to keep above/below the cursor when auto-scrolling." => {
            "settings-the-number-of-lines-to-keep-above-below-the"
        }
        "Time to wait in milliseconds before hiding the hover popover after the mouse moves away." => {
            "settings-time-to-wait-in-milliseconds-before-hiding-the"
        }
        "Time to wait in milliseconds before showing the informational hover box." => {
            "settings-time-to-wait-in-milliseconds-before-showing-the"
        }
        "Toggle relative line numbers in Vim mode." => {
            "settings-toggle-relative-line-numbers-in-vim-mode"
        }
        "Use regex search by default in Vim search." => {
            "settings-use-regex-search-by-default-in-vim-search"
        }
        "What to do when multibuffer is double-clicked in some of its excerpts." => {
            "settings-what-to-do-when-multibuffer-is-double-clicked"
        }
        "When enabled, the :substitute command replaces all matches in a line by default. The 'g' flag then toggles this behavior." => {
            "settings-when-enabled-the-substitute-command-replaces"
        }
        "When false, forcefully disables the horizontal scrollbar." => {
            "settings-when-false-forcefully-disables-the-horizontal"
        }
        "When false, forcefully disables the vertical scrollbar." => {
            "settings-when-false-forcefully-disables-the-vertical"
        }
        "When to auto save buffer changes." => "settings-when-to-auto-save-buffer-changes",
        "When to show the minimap in the editor." => {
            "settings-when-to-show-the-minimap-in-the-editor"
        }
        "When to show the minimap thumb." => "settings-when-to-show-the-minimap-thumb",
        "When to show the scrollbar in the editor." => {
            "settings-when-to-show-the-scrollbar-in-the-editor"
        }
        "Where to show the minimap in the editor." => {
            "settings-where-to-show-the-minimap-in-the-editor"
        }
        "Whether the editor will scroll beyond the last line." => {
            "settings-whether-the-editor-will-scroll-beyond-the-last"
        }
        "Whether the hover popover sticks when the mouse moves toward it, allowing interaction with its contents." => {
            "settings-whether-the-hover-popover-sticks-when-the-mouse"
        }
        "Whether to scroll when clicking near the edge of the visible text area." => {
            "settings-whether-to-scroll-when-clicking-near-the-edge"
        }
        "Whether to stick scopes to the top of the editor" => {
            "settings-whether-to-stick-scopes-to-the-top-of-the"
        }
        "Whether to zoom the editor font size with the mouse wheel while holding the primary modifier key." => {
            "settings-whether-to-zoom-the-editor-font-size-with-the"
        }
        "Which diagnostic indicators to show in the scrollbar." => {
            "settings-which-diagnostic-indicators-to-show-in-the"
        }
        "Autoclose" => "settings-autoclose",
        "Completions" => "settings-completions",
        "Formatting" => "settings-formatting",
        "Indent Guides" => "settings-indent-guides",
        "Indentation" => "settings-indentation",
        "Inlay Hints" => "settings-inlay-hints",
        "LSP" => "settings-lsp",
        "LSP Completions" => "settings-lsp-completions",
        "Miscellaneous" => "settings-miscellaneous",
        "Prettier" => "settings-prettier",
        "Tasks" => "settings-tasks",
        "Whitespace" => "settings-whitespace",
        "Wrapping" => "settings-wrapping",
        "Active Line Width" => "settings-active-line-width",
        "Allow Rewrap" => "settings-allow-rewrap",
        "Allowed" => "settings-allowed",
        "Always Treat Brackets As Autoclosed" => "settings-always-treat-brackets-as-autoclosed",
        "Auto Indent" => "settings-auto-indent",
        "Auto Indent On Paste" => "settings-auto-indent-on-paste",
        "Auto Replace Emoji Shortcode" => "settings-auto-replace-emoji-shortcode",
        "Background Coloring" => "settings-background-coloring",
        "Code Actions On Format" => "settings-code-actions-on-format",
        "Code Lens" => "settings-code-lens",
        "Coloring" => "settings-coloring",
        "Colorize Brackets" => "settings-colorize-brackets",
        "Completion Detail Alignment" => "settings-completion-detail-alignment",
        "Completion Menu Item Kind" => "settings-completion-menu-item-kind",
        "Completion Menu Scrollbar" => "settings-completion-menu-scrollbar",
        "Debuggers" => "settings-debuggers",
        "Drop Size Target" => "settings-drop-size-target",
        "Edit Debounce Ms" => "settings-edit-debounce-ms",
        "Enable Language Server" => "settings-enable-language-server",
        "Ensure Final Newline On Save" => "settings-ensure-final-newline-on-save",
        "Extend Comment On Newline" => "settings-extend-comment-on-newline",
        "Fetch Timeout (milliseconds)" => "settings-fetch-timeout-milliseconds",
        "Format On Save" => "settings-format-on-save",
        "Formatter" => "settings-formatter",
        "Go To Definition Fallback" => "settings-go-to-definition-fallback",
        "Go To Definition Scroll Strategy" => "settings-go-to-definition-scroll-strategy",
        "Hard Tabs" => "settings-hard-tabs",
        "Image Viewer" => "settings-image-viewer",
        "Insert Mode" => "settings-insert-mode",
        "JSX Tag Auto Close" => "settings-jsx-tag-auto-close",
        "LSP Document Colors" => "settings-lsp-document-colors",
        "LSP Document Symbols" => "settings-lsp-document-symbols",
        "LSP Folding Ranges" => "settings-lsp-folding-ranges",
        "LSP Results Location" => "settings-lsp-results-location",
        "Language Servers" => "settings-language-servers",
        "Line Ending" => "settings-line-ending",
        "Line Width" => "settings-line-width",
        "Linked Edits" => "settings-linked-edits",
        "Middle Click Paste" => "settings-middle-click-paste",
        "Options" => "settings-options",
        "Parser" => "settings-parser",
        "Plugins" => "settings-plugins",
        "Prefer LSP" => "settings-prefer-lsp",
        "Preferred Line Length" => "settings-preferred-line-length",
        "Proxy" => "settings-proxy",
        "Remove Trailing Whitespace On Save" => "settings-remove-trailing-whitespace-on-save",
        "Scroll Debounce Ms" => "settings-scroll-debounce-ms",
        "Semantic Tokens" => "settings-semantic-tokens",
        "Show Background" => "settings-show-background",
        "Show Completion Documentation" => "settings-show-completion-documentation",
        "Show Completions On Input" => "settings-show-completions-on-input",
        "Show Other Hints" => "settings-show-other-hints",
        "Show Parameter Hints" => "settings-show-parameter-hints",
        "Show Type Hints" => "settings-show-type-hints",
        "Show Value Hints" => "settings-show-value-hints",
        "Show Whitespaces" => "settings-show-whitespaces",
        "Soft Wrap" => "settings-soft-wrap",
        "Space Whitespace Indicator" => "settings-space-whitespace-indicator",
        "Tab Size" => "settings-tab-size",
        "Tab Whitespace Indicator" => "settings-tab-whitespace-indicator",
        "Toggle On Modifiers Press" => "settings-toggle-on-modifiers-press",
        "Use Auto Surround" => "settings-use-auto-surround",
        "Use Autoclose" => "settings-use-autoclose",
        "Use On Type Format" => "settings-use-on-type-format",
        "Variables" => "settings-variables",
        "Vim/Emacs Modeline Support" => "settings-vim-emacs-modeline-support",
        "Word Diff Enabled" => "settings-word-diff-enabled",
        "Words" => "settings-words",
        "Words Min Length" => "settings-words-min-length",
        "Additional code actions to run when formatting." => {
            "settings-additional-code-actions-to-run-when-formatting"
        }
        "Character counts at which to show wrap guides in the editor." => {
            "settings-character-counts-at-which-to-show-wrap-guides-2"
        }
        "Controls automatic indentation behavior when typing." => {
            "settings-controls-automatic-indentation-behavior-when"
        }
        "Controls how LSP completions are inserted." => {
            "settings-controls-how-lsp-completions-are-inserted"
        }
        "Controls how words are completed." => "settings-controls-how-words-are-completed",
        "Controls where the `editor::rewrap` action is allowed for this language." => {
            "settings-controls-where-the-editor-rewrap-action-is"
        }
        "Controls whether the closing characters are always skipped over and auto-removed no matter how they were inserted." => {
            "settings-controls-whether-the-closing-characters-are"
        }
        "Default Prettier options, in the format as in package.json section for Prettier." => {
            "settings-default-prettier-options-in-the-format-as-in"
        }
        "Determines how indent guide backgrounds are colored." => {
            "settings-determines-how-indent-guide-backgrounds-are"
        }
        "Determines how indent guides are colored." => {
            "settings-determines-how-indent-guides-are-colored"
        }
        "Display indent guides in the editor." => "settings-display-indent-guides-in-the-editor",
        "Enable middle-click paste on Linux." => "settings-enable-middle-click-paste-on-linux",
        "Enables or disables formatting with Prettier for a given language." => {
            "settings-enables-or-disables-formatting-with-prettier"
        }
        "Extra task variables to set for a particular language." => {
            "settings-extra-task-variables-to-set-for-a-particular"
        }
        "Forces Prettier integration to use a specific parser name when formatting files with the language." => {
            "settings-forces-prettier-integration-to-use-a-specific"
        }
        "Forces Prettier integration to use specific plugins when formatting files with the language." => {
            "settings-forces-prettier-integration-to-use-specific"
        }
        "Global switch to toggle hints on and off." => {
            "settings-global-switch-to-toggle-hints-on-and-off"
        }
        "Global switch to toggle inline values on and off when debugging." => {
            "settings-global-switch-to-toggle-inline-values-on-and"
        }
        "How line endings should be handled for new files and during format and save operations." => {
            "settings-how-line-endings-should-be-handled-for-new"
        }
        "How many characters has to be in the completions query to automatically show the words-based completions." => {
            "settings-how-many-characters-has-to-be-in-the"
        }
        "How many columns a tab should occupy." => "settings-how-many-columns-a-tab-should-occupy",
        "How to display the LSP item kind (function, method, variable, etc.) of each entry in the completions menu." => {
            "settings-how-to-display-the-lsp-item-kind-function"
        }
        "How to perform a buffer format." => "settings-how-to-perform-a-buffer-format",
        "How to render LSP color previews in the editor." => {
            "settings-how-to-render-lsp-color-previews-in-the-editor"
        }
        "How to scroll the target into view when navigating to a definition or reference." => {
            "settings-how-to-scroll-the-target-into-view-when"
        }
        "How to soft-wrap long lines of text." => "settings-how-to-soft-wrap-long-lines-of-text",
        "Number of lines to search for modelines (set to 0 to disable)." => {
            "settings-number-of-lines-to-search-for-modelines-set-to"
        }
        "Preferred debuggers for this language." => {
            "settings-preferred-debuggers-for-this-language"
        }
        "Relative size of the drop target in the editor that will open dropped file as a split pane." => {
            "settings-relative-size-of-the-drop-target-in-the-editor"
        }
        "Show a background for inlay hints." => "settings-show-a-background-for-inlay-hints",
        "Show wrap guides in the editor." => "settings-show-wrap-guides-in-the-editor",
        "The column at which to soft-wrap lines, for buffers where soft-wrap is enabled." => {
            "settings-the-column-at-which-to-soft-wrap-lines-for"
        }
        "The list of language servers to use (or disable) for this language." => {
            "settings-the-list-of-language-servers-to-use-or-disable"
        }
        "The proxy to use for network requests." => {
            "settings-the-proxy-to-use-for-network-requests"
        }
        "The unit for image file sizes." => "settings-the-unit-for-image-file-sizes",
        "The width of the active indent guide in pixels, between 1 and 10." => {
            "settings-the-width-of-the-active-indent-guide-in-pixels"
        }
        "The width of the indent guides in pixels, between 1 and 10." => {
            "settings-the-width-of-the-indent-guides-in-pixels"
        }
        "Toggles inlay hints (hides or shows) when the user presses the modifiers specified." => {
            "settings-toggles-inlay-hints-hides-or-shows-when-the"
        }
        "Use LSP tasks over Flint language extension tasks." => {
            "settings-use-lsp-tasks-over-flint-language-extension"
        }
        "Visible character used to render space characters when show_whitespaces is enabled (default: \"•\")" => {
            "settings-visible-character-used-to-render-space"
        }
        "Visible character used to render tab characters when show_whitespaces is enabled (default: \"→\")" => {
            "settings-visible-character-used-to-render-tab-characters"
        }
        "When enabled, use folding ranges from the language server instead of indent-based folding." => {
            "settings-when-enabled-use-folding-ranges-from-the"
        }
        "When enabled, use the language server's document symbols for outlines and breadcrumbs instead of tree-sitter." => {
            "settings-when-enabled-use-the-language-servers-document"
        }
        "When fetching LSP completions, determines how long to wait for a response of a particular server (set to 0 to wait indefinitely)." => {
            "settings-when-fetching-lsp-completions-determines-how"
        }
        "When to show the scrollbar in the completion menu." => {
            "settings-when-to-show-the-scrollbar-in-the-completion"
        }
        "Where to show LSP results that can contain multiple locations (Go to Definition, Go to Implementation, Find All References)." => {
            "settings-where-to-show-lsp-results-that-can-contain"
        }
        "Whether and how to display code lenses from language servers." => {
            "settings-whether-and-how-to-display-code-lenses-from"
        }
        "Whether indentation of pasted content should be adjusted based on the context." => {
            "settings-whether-indentation-of-pasted-content-should-be"
        }
        "Whether or not to debounce inlay hints updates after buffer edits (set to 0 to disable debouncing)." => {
            "settings-whether-or-not-to-debounce-inlay-hints-updates"
        }
        "Whether or not to debounce inlay hints updates after buffer scrolls (set to 0 to disable debouncing)." => {
            "settings-whether-or-not-to-debounce-inlay-hints-updates-2"
        }
        "Whether or not to ensure there's a single newline at the end of a buffer when saving it." => {
            "settings-whether-or-not-to-ensure-theres-a-single"
        }
        "Whether or not to perform a buffer format before saving." => {
            "settings-whether-or-not-to-perform-a-buffer-format"
        }
        "Whether or not to remove any trailing whitespace from lines of a buffer before saving it." => {
            "settings-whether-or-not-to-remove-any-trailing"
        }
        "Whether other hints should be shown." => "settings-whether-other-hints-should-be-shown",
        "Whether parameter hints should be shown." => {
            "settings-whether-parameter-hints-should-be-shown"
        }
        "Whether tasks are enabled for this language." => {
            "settings-whether-tasks-are-enabled-for-this-language"
        }
        "Whether to align detail text in code completions context menus left or right." => {
            "settings-whether-to-align-detail-text-in-code"
        }
        "Whether to automatically close JSX tags." => {
            "settings-whether-to-automatically-close-jsx-tags"
        }
        "Whether to automatically replace emoji shortcodes with emoji characters." => {
            "settings-whether-to-automatically-replace-emoji"
        }
        "Whether to automatically surround text with characters for you. For example, when you select text and type '(', Flint will automatically surround text with ()." => {
            "settings-whether-to-automatically-surround-text-with"
        }
        "Whether to automatically type closing characters for you. For example, when you type '(', Flint will automatically add a closing ')' at the correct position." => {
            "settings-whether-to-automatically-type-closing"
        }
        "Whether to colorize brackets in the editor." => {
            "settings-whether-to-colorize-brackets-in-the-editor"
        }
        "Whether to display inline and alongside documentation for items in the completions menu." => {
            "settings-whether-to-display-inline-and-alongside"
        }
        "Whether to enable word diff highlighting in the editor. When enabled, changed words within modified lines are highlighted to show exactly what changed." => {
            "settings-whether-to-enable-word-diff-highlighting-in-the"
        }
        "Whether to fetch LSP completions or not." => {
            "settings-whether-to-fetch-lsp-completions-or-not"
        }
        "Whether to follow-up empty Go to definition responses from the language server." => {
            "settings-whether-to-follow-up-empty-go-to-definition"
        }
        "Whether to indent lines using tab characters, as opposed to multiple spaces." => {
            "settings-whether-to-indent-lines-using-tab-characters-as"
        }
        "Whether to perform linked edits of associated ranges, if the LS supports it. For example, when editing opening <html> tag, the contents of the closing </html> tag will be edited as well." => {
            "settings-whether-to-perform-linked-edits-of-associated"
        }
        "Whether to pop the completions menu while typing in an editor without explicitly requesting it." => {
            "settings-whether-to-pop-the-completions-menu-while"
        }
        "Whether to show tabs and spaces in the editor." => {
            "settings-whether-to-show-tabs-and-spaces-in-the-editor"
        }
        "Whether to start a new line with a comment when a previous line is a comment as well." => {
            "settings-whether-to-start-a-new-line-with-a-comment-when"
        }
        "Whether to use additional LSP queries to format (and amend) the code after every \"trigger\" symbol input, defined by LSP server capabilities" => {
            "settings-whether-to-use-additional-lsp-queries-to-format"
        }
        "Whether to use language servers to provide code intelligence." => {
            "settings-whether-to-use-language-servers-to-provide-code"
        }
        "Whether type hints should be shown." => "settings-whether-type-hints-should-be-shown",
        "Whether or not to automatically check for updates." => {
            "settings-general-auto-update-description"
        }
        "Active Editor" => "settings-dd-active-editor",
        "ActiveEditor" => "settings-dd-activeeditor",
        "Add To Existing Window" => "settings-dd-add-to-existing-window",
        "Add to Existing Window" => "settings-dd-add-to-existing-window-2",
        "All" => "settings-dd-all",
        "All Editors" => "settings-dd-all-editors",
        "AllEditors" => "settings-dd-alleditors",
        "Alt" => "settings-dd-alt",
        "Always" => "settings-dd-always",
        "Anywhere" => "settings-dd-anywhere",
        "Auto" => "settings-dd-auto",
        "Background" => "settings-dd-background",
        "Bar" => "settings-dd-bar",
        "Binary" => "settings-dd-binary",
        "Block" => "settings-dd-block",
        "Bold" => "settings-dd-bold",
        "Border" => "settings-dd-border",
        "Bottom" => "settings-dd-bottom",
        "Boundary" => "settings-dd-boundary",
        "Bounded" => "settings-dd-bounded",
        "Center" => "settings-dd-center",
        "Claude" => "settings-dd-claude",
        "Client" => "settings-dd-client",
        "Close Window" => "settings-dd-close-window",
        "CloseWindow" => "settings-dd-closewindow",
        "Cmd Or Ctrl" => "settings-dd-cmd-or-ctrl",
        "CmdOrCtrl" => "settings-dd-cmdorctrl",
        "Codex" => "settings-dd-codex",
        "Combined" => "settings-dd-combined",
        "Comfortable" => "settings-dd-comfortable",
        "Contained" => "settings-dd-contained",
        "Dark" => "settings-dd-dark",
        "Decimal" => "settings-dd-decimal",
        "Default" => "settings-dd-default",
        "Detect" => "settings-dd-detect",
        "Directories First" => "settings-dd-directories-first",
        "DirectoriesFirst" => "settings-dd-directoriesfirst",
        "Disabled" => "settings-dd-disabled",
        "Down" => "settings-dd-down",
        "Editor Width" => "settings-dd-editor-width",
        "EditorWidth" => "settings-dd-editorwidth",
        "Empty Tab" => "settings-dd-empty-tab",
        "EmptyTab" => "settings-dd-emptytab",
        "Enforce CRLF" => "settings-dd-enforce-crlf",
        "Enforce Crlf" => "settings-dd-enforce-crlf-2",
        "Enforce LF" => "settings-dd-enforce-lf",
        "Enforce Lf" => "settings-dd-enforce-lf-2",
        "Error" => "settings-dd-error",
        "Errors" => "settings-dd-errors",
        "Expanded" => "settings-dd-expanded",
        "Fallback" => "settings-dd-fallback",
        "File Name First" => "settings-dd-file-name-first",
        "File Path First" => "settings-dd-file-path-first",
        "FileNameFirst" => "settings-dd-filenamefirst",
        "FilePathFirst" => "settings-dd-filepathfirst",
        "Files First" => "settings-dd-files-first",
        "FilesFirst" => "settings-dd-filesfirst",
        "Find All References" => "settings-dd-find-all-references",
        "FindAllReferences" => "settings-dd-findallreferences",
        "Fixed" => "settings-dd-fixed",
        "Full" => "settings-dd-full",
        "Grayscale" => "settings-dd-grayscale",
        "Hidden" => "settings-dd-hidden",
        "Hide" => "settings-dd-hide",
        "Hint" => "settings-dd-hint",
        "History" => "settings-dd-history",
        "Hollow" => "settings-dd-hollow",
        "Hover" => "settings-dd-hover",
        "Icon" => "settings-dd-icon",
        "In Comments" => "settings-dd-in-comments",
        "In Selections" => "settings-dd-in-selections",
        "InComments" => "settings-dd-incomments",
        "InSelections" => "settings-dd-inselections",
        "Indent Aware" => "settings-dd-indent-aware",
        "IndentAware" => "settings-dd-indentaware",
        "Indexed" => "settings-dd-indexed",
        "Info" => "settings-dd-info",
        "Information" => "settings-dd-information",
        "Inherit" => "settings-dd-inherit",
        "Inlay" => "settings-dd-inlay",
        "Inline" => "settings-dd-inline",
        "Insert" => "settings-dd-insert",
        "Italic" => "settings-dd-italic",
        "Keep Window Open" => "settings-dd-keep-window-open",
        "KeepWindowOpen" => "settings-dd-keepwindowopen",
        "Label Color" => "settings-dd-label-color",
        "LabelColor" => "settings-dd-labelcolor",
        "Large" => "settings-dd-large",
        "Last Session" => "settings-dd-last-session",
        "Last Workspace" => "settings-dd-last-workspace",
        "LastSession" => "settings-dd-lastsession",
        "LastWorkspace" => "settings-dd-lastworkspace",
        "Launchpad" => "settings-dd-launchpad",
        "Left" => "settings-dd-left",
        "Left Aligned" => "settings-dd-left-aligned",
        "Left Neighbour" => "settings-dd-left-neighbour",
        "Left Only" => "settings-dd-left-only",
        "Left Open" => "settings-dd-left-open",
        "LeftAligned" => "settings-dd-leftaligned",
        "LeftNeighbour" => "settings-dd-leftneighbour",
        "LeftOnly" => "settings-dd-leftonly",
        "LeftOpen" => "settings-dd-leftopen",
        "Light" => "settings-dd-light",
        "Line" => "settings-dd-line",
        "Lower" => "settings-dd-lower",
        "Matching Workspace" => "settings-dd-matching-workspace",
        "MatchingWorkspace" => "settings-dd-matchingworkspace",
        "Medium" => "settings-dd-medium",
        "Menu" => "settings-dd-menu",
        "Minimum" => "settings-dd-minimum",
        "Mixed" => "settings-dd-mixed",
        "Multi Buffer" => "settings-dd-multi-buffer",
        "MultiBuffer" => "settings-dd-multibuffer",
        "Name" => "settings-dd-name",
        "Neighbour" => "settings-dd-neighbour",
        "Never" => "settings-dd-never",
        "Non Utf8" => "settings-dd-non-utf8",
        "NonUtf8" => "settings-dd-nonutf8",
        "None" => "settings-dd-none",
        "Normal" => "settings-dd-normal",
        "Off" => "settings-dd-off",
        "On" => "settings-dd-on",
        "On Typing" => "settings-dd-on-typing",
        "On Typing And Action" => "settings-dd-on-typing-and-action",
        "On Yank" => "settings-dd-on-yank",
        "OnTyping" => "settings-dd-ontyping",
        "OnTypingAndAction" => "settings-dd-ontypingandaction",
        "OnYank" => "settings-dd-onyank",
        "One Page" => "settings-dd-one-page",
        "OnePage" => "settings-dd-onepage",
        "Open" => "settings-dd-open",
        "Open A New Window" => "settings-dd-open-a-new-window",
        "Open a New Window" => "settings-dd-open-a-new-window-2",
        "Path" => "settings-dd-path",
        "Pi" => "settings-dd-pi",
        "Picker" => "settings-dd-picker",
        "Platform Default" => "settings-dd-platform-default",
        "PlatformDefault" => "settings-dd-platformdefault",
        "Prefer CRLF" => "settings-dd-prefer-crlf",
        "Prefer Crlf" => "settings-dd-prefer-crlf-2",
        "Prefer LF" => "settings-dd-prefer-lf",
        "Prefer Lf" => "settings-dd-prefer-lf-2",
        "Prefer Line" => "settings-dd-prefer-line",
        "PreferLine" => "settings-dd-preferline",
        "Preserve" => "settings-dd-preserve",
        "Preserve Indent" => "settings-dd-preserve-indent",
        "PreserveIndent" => "settings-dd-preserveindent",
        "Quit App" => "settings-dd-quit-app",
        "QuitApp" => "settings-dd-quitapp",
        "Replace" => "settings-dd-replace",
        "Replace Subsequence" => "settings-dd-replace-subsequence",
        "Replace Suffix" => "settings-dd-replace-suffix",
        "ReplaceSubsequence" => "settings-dd-replacesubsequence",
        "ReplaceSuffix" => "settings-dd-replacesuffix",
        "Right" => "settings-dd-right",
        "Right Aligned" => "settings-dd-right-aligned",
        "Right Open" => "settings-dd-right-open",
        "RightAligned" => "settings-dd-rightaligned",
        "RightOpen" => "settings-dd-rightopen",
        "Select" => "settings-dd-select",
        "Selection" => "settings-dd-selection",
        "Server" => "settings-dd-server",
        "Small" => "settings-dd-small",
        "Smart" => "settings-dd-smart",
        "Split" => "settings-dd-split",
        "Staged Hollow" => "settings-dd-staged-hollow",
        "StagedHollow" => "settings-dd-stagedhollow",
        "Staging" => "settings-dd-staging",
        "Standard" => "settings-dd-standard",
        "Status" => "settings-dd-status",
        "Subpixel" => "settings-dd-subpixel",
        "Symbol" => "settings-dd-symbol",
        "Syntax Aware" => "settings-dd-syntax-aware",
        "SyntaxAware" => "settings-dd-syntaxaware",
        "System" => "settings-dd-system",
        "Terminal Controlled" => "settings-dd-terminal-controlled",
        "TerminalControlled" => "settings-dd-terminalcontrolled",
        "Top" => "settings-dd-top",
        "Tracked Files" => "settings-dd-tracked-files",
        "TrackedFiles" => "settings-dd-trackedfiles",
        "Trailing" => "settings-dd-trailing",
        "Underline" => "settings-dd-underline",
        "Unicode" => "settings-dd-unicode",
        "Unified" => "settings-dd-unified",
        "Unstaged Hollow" => "settings-dd-unstaged-hollow",
        "UnstagedHollow" => "settings-dd-unstagedhollow",
        "Up" => "settings-dd-up",
        "Upper" => "settings-dd-upper",
        "VerticalScrollMargin" => "settings-dd-verticalscrollmargin",
        "Warning" => "settings-dd-warning",
        "Wrapped" => "settings-dd-wrapped",
        "X Large" => "settings-dd-x-large",
        "XLarge" => "settings-dd-xlarge",
        _ => return None,
    })
}

struct DynamicItem {
    discriminant: SettingItem,
    pick_discriminant: fn(&SettingsContent) -> Option<usize>,
    fields: Vec<Vec<SettingItem>>,
}

impl PartialEq for DynamicItem {
    fn eq(&self, other: &Self) -> bool {
        self.discriminant == other.discriminant && self.fields == other.fields
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
struct FileMask(u8);

impl std::fmt::Debug for FileMask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FileMask(")?;
        let mut items = vec![];

        if self.contains(USER) {
            items.push("USER");
        }
        if self.contains(PROJECT) {
            items.push("LOCAL");
        }
        if self.contains(SERVER) {
            items.push("SERVER");
        }

        write!(f, "{})", items.join(" | "))
    }
}

const USER: FileMask = FileMask(1 << 0);
const PROJECT: FileMask = FileMask(1 << 2);
const SERVER: FileMask = FileMask(1 << 3);

impl std::ops::BitAnd for FileMask {
    type Output = Self;

    fn bitand(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

impl std::ops::BitOr for FileMask {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl FileMask {
    fn contains(&self, other: FileMask) -> bool {
        self.0 & other.0 != 0
    }
}

impl PartialEq for SettingItem {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title
            && self.description == other.description
            && (match (&self.metadata, &other.metadata) {
                (None, None) => true,
                (Some(m1), Some(m2)) => m1.placeholder == m2.placeholder,
                _ => false,
            })
    }
}

#[derive(Clone, PartialEq, Default)]
enum SubPageType {
    Language,
    #[default]
    Other,
}

#[derive(Clone)]
struct SubPageLink {
    title: SharedString,
    r#type: SubPageType,
    description: Option<SharedString>,
    /// See [`SettingField.json_path`]
    json_path: Option<&'static str>,
    /// Whether or not the settings in this sub page are configurable in settings.json
    /// Removes the "Edit in settings.json" button from the page.
    in_json: bool,
    files: FileMask,
    render:
        fn(&SettingsWindow, &ScrollHandle, &mut Window, &mut Context<SettingsWindow>) -> AnyElement,
}

impl PartialEq for SubPageLink {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title
    }
}

#[derive(Clone)]
struct ActionLink {
    title: SharedString,
    description: Option<SharedString>,
    button_text: SharedString,
    on_click: Arc<dyn Fn(&mut SettingsWindow, &mut Window, &mut App) + Send + Sync>,
    files: FileMask,
}

impl PartialEq for ActionLink {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title
    }
}

fn all_language_names(cx: &App) -> Vec<SharedString> {
    let state = workspace::AppState::global(cx);
    state
        .languages
        .language_names()
        .into_iter()
        .filter(|name| name.as_ref() != "Flint Keybind Context")
        .map(Into::into)
        .collect()
}

#[allow(unused)]
#[derive(Clone, PartialEq, Debug)]
enum SettingsUiFile {
    User,                                // Uses all settings.
    Project((WorktreeId, Arc<RelPath>)), // Has a special name, and special set of settings
    Server(&'static str),                // Uses a special name, and the user settings
}

impl SettingsUiFile {
    fn setting_type(&self) -> &'static str {
        match self {
            SettingsUiFile::User => "User",
            SettingsUiFile::Project(_) => "Project",
            SettingsUiFile::Server(_) => "Server",
        }
    }

    fn is_server(&self) -> bool {
        matches!(self, SettingsUiFile::Server(_))
    }

    fn worktree_id(&self) -> Option<WorktreeId> {
        match self {
            SettingsUiFile::User => None,
            SettingsUiFile::Project((worktree_id, _)) => Some(*worktree_id),
            SettingsUiFile::Server(_) => None,
        }
    }

    fn from_settings(file: settings::SettingsFile) -> Option<Self> {
        Some(match file {
            settings::SettingsFile::User => SettingsUiFile::User,
            settings::SettingsFile::Project(location) => SettingsUiFile::Project(location),
            settings::SettingsFile::Server => SettingsUiFile::Server("todo: server name"),
            settings::SettingsFile::Default => return None,
            settings::SettingsFile::Global => return None,
        })
    }

    fn to_settings(&self) -> settings::SettingsFile {
        match self {
            SettingsUiFile::User => settings::SettingsFile::User,
            SettingsUiFile::Project(location) => settings::SettingsFile::Project(location.clone()),
            SettingsUiFile::Server(_) => settings::SettingsFile::Server,
        }
    }

    fn mask(&self) -> FileMask {
        match self {
            SettingsUiFile::User => USER,
            SettingsUiFile::Project(_) => PROJECT,
            SettingsUiFile::Server(_) => SERVER,
        }
    }
}

impl SettingsWindow {
    fn new(
        original_window: Option<WindowHandle<MultiWorkspace>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let font_family_cache = theme::FontFamilyCache::global(cx);

        cx.spawn(async move |this, cx| {
            font_family_cache.prefetch(cx).await;
            this.update(cx, |_, cx| {
                cx.notify();
            })
        })
        .detach();

        let current_file = SettingsUiFile::User;
        let search_bar = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            let placeholder = localization::text(cx, "settings-search-placeholder");
            editor.set_placeholder_text(&placeholder, window, cx);
            editor
        });
        cx.subscribe(&search_bar, |this, _, event: &EditorEvent, cx| {
            let EditorEvent::Edited { transaction_id: _ } = event else {
                return;
            };

            if this.opening_link {
                this.opening_link = false;
                return;
            }
            this.update_matches(cx);
        })
        .detach();

        let mut ui_font_size = ThemeSettings::get_global(cx).ui_font_size(cx);
        let mut ui_language = SettingsStore::global(cx).ui_language();
        cx.observe_global_in::<SettingsStore>(window, move |this, window, cx| {
            this.fetch_files(window, cx);

            let new_ui_language = SettingsStore::global(cx).ui_language();
            if new_ui_language != ui_language {
                ui_language = new_ui_language;
                this.rebuild_pages(window, cx);
            }

            // Whenever settings are changed, it's possible that the changed
            // settings affects the rendering of the `SettingsWindow`, like is
            // the case with `ui_font_size`. When that happens, we need to
            // instruct the `ListState` to re-measure the list items, as the
            // list item heights may have changed depending on the new font
            // size.
            let new_ui_font_size = ThemeSettings::get_global(cx).ui_font_size(cx);
            if new_ui_font_size != ui_font_size {
                this.list_state.remeasure();
                ui_font_size = new_ui_font_size;
            }

            cx.notify();
        })
        .detach();

        use feature_flags::FeatureFlagAppExt as _;
        let mut last_is_staff = cx.is_staff();
        cx.observe_global_in::<feature_flags::FeatureFlagStore>(window, move |this, window, cx| {
            let is_staff = cx.is_staff();
            if is_staff != last_is_staff {
                last_is_staff = is_staff;
                this.rebuild_pages(window, cx);
            }
        })
        .detach();

        cx.on_window_closed(|cx, _window_id| {
            if let Some(existing_window) = cx
                .windows()
                .into_iter()
                .find_map(|window| window.downcast::<SettingsWindow>())
                && cx.windows().len() == 1
            {
                cx.update_window(*existing_window, |_, window, _| {
                    window.remove_window();
                })
                .ok();
            }
        })
        .detach();

        let app_state = AppState::global(cx);
        let workspaces: Vec<Entity<Workspace>> = app_state
            .workspace_store
            .read(cx)
            .workspaces()
            .filter_map(|weak| weak.upgrade())
            .collect();

        for workspace in workspaces {
            let project = workspace.read(cx).project().clone();
            cx.observe_release_in(&project, window, |this, _, window, cx| {
                this.fetch_files(window, cx)
            })
            .detach();
            cx.subscribe_in(&project, window, Self::handle_project_event)
                .detach();
            cx.observe_release_in(&workspace, window, |this, _, window, cx| {
                this.fetch_files(window, cx)
            })
            .detach();
        }

        let this_weak = cx.weak_entity();
        cx.observe_new::<Project>({
            let this_weak = this_weak.clone();

            move |_, window, cx| {
                let project = cx.entity();
                let Some(window) = window else {
                    return;
                };

                this_weak
                    .update(cx, |_, cx| {
                        cx.defer_in(window, |settings_window, window, cx| {
                            settings_window.fetch_files(window, cx)
                        });
                        cx.observe_release_in(&project, window, |_, _, window, cx| {
                            cx.defer_in(window, |this, window, cx| this.fetch_files(window, cx));
                        })
                        .detach();

                        cx.subscribe_in(&project, window, Self::handle_project_event)
                            .detach();
                    })
                    .ok();
            }
        })
        .detach();

        let handle = window.window_handle();
        cx.observe_new::<Workspace>(move |workspace, _, cx| {
            let project = workspace.project().clone();
            let this_weak = this_weak.clone();

            // We defer on the settings window (via `handle`) rather than using
            // the workspace's window from observe_new. When window.defer() runs
            // its callback, it calls handle.update() which temporarily removes
            // that window from cx.windows. If we deferred on the workspace's
            // window, then when fetch_files() tries to read ALL workspaces from
            // the store (including the newly created one), it would fail with
            // "window not found" because that workspace's window would be
            // temporarily removed from cx.windows for the duration of our callback.
            handle
                .update(cx, move |_, window, cx| {
                    window.defer(cx, move |window, cx| {
                        this_weak
                            .update(cx, |this, cx| {
                                this.fetch_files(window, cx);
                                cx.observe_release_in(&project, window, |this, _, window, cx| {
                                    this.fetch_files(window, cx)
                                })
                                .detach();
                            })
                            .ok();
                    });
                })
                .ok();
        })
        .detach();

        let title_bar = if !cfg!(target_os = "macos") {
            Some(cx.new(|cx| PlatformTitleBar::new("settings-title-bar", cx)))
        } else {
            None
        };

        let list_state = gpui::ListState::new(0, gpui::ListAlignment::Top, px(0.0)).measure_all();
        list_state.set_scroll_handler(|_, _, _| {});

        let mut this = Self {
            title_bar,
            original_window,

            worktree_root_dirs: HashMap::default(),
            files: vec![],

            current_file: current_file,
            project_setting_file_buffers: HashMap::default(),
            pages: vec![],
            sub_page_stack: vec![],
            opening_link: false,
            navbar_entries: vec![],
            navbar_entry: 0,
            navbar_scroll_handle: UniformListScrollHandle::default(),
            search_bar,
            search_task: None,
            filter_table: vec![],
            has_query: false,
            content_handles: vec![],
            focus_handle: cx.focus_handle(),
            navbar_focus_handle: NonFocusableHandle::new(
                NAVBAR_CONTAINER_TAB_INDEX,
                false,
                window,
                cx,
            ),
            navbar_focus_subscriptions: vec![],
            content_focus_handle: NonFocusableHandle::new(
                CONTENT_CONTAINER_TAB_INDEX,
                false,
                window,
                cx,
            ),
            files_focus_handle: cx
                .focus_handle()
                .tab_index(HEADER_CONTAINER_TAB_INDEX)
                .tab_stop(false),
            search_index: None,
            regex_validation_error: None,
            list_state,
            last_copied_link_path: None,
        };

        this.fetch_files(window, cx);
        this.build_ui(window, cx);
        this.build_search_index(cx);

        this.search_bar.update(cx, |editor, cx| {
            editor.focus_handle(cx).focus(window, cx);
        });

        this
    }

    fn handle_project_event(
        &mut self,
        _: &Entity<Project>,
        event: &project::Event,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) {
        match event {
            project::Event::WorktreeRemoved(_) | project::Event::WorktreeAdded(_) => {
                cx.defer_in(window, |this, window, cx| {
                    this.fetch_files(window, cx);
                });
            }
            _ => {}
        }
    }

    fn toggle_navbar_entry(&mut self, nav_entry_index: usize) {
        // We can only toggle root entries
        if !self.navbar_entries[nav_entry_index].is_root {
            return;
        }

        let expanded = &mut self.navbar_entries[nav_entry_index].expanded;
        *expanded = !*expanded;
        self.navbar_entry = nav_entry_index;
        self.reset_list_state();
    }

    fn toggle_and_focus_navbar_entry(
        &mut self,
        nav_entry_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_navbar_entry(nav_entry_index);
        window.focus(&self.navbar_entries[nav_entry_index].focus_handle, cx);
        cx.notify();
    }

    fn toggle_navbar_entry_on_double_click(
        &mut self,
        nav_entry_index: usize,
        event: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(entry) = self.navbar_entries.get(nav_entry_index) else {
            return false;
        };
        if !entry.is_root || event.click_count() != 2 {
            return false;
        }

        self.toggle_and_focus_navbar_entry(nav_entry_index, window, cx);
        true
    }

    fn build_navbar(&mut self, cx: &App) {
        let mut navbar_entries = Vec::new();

        for (page_index, page) in self.pages.iter().enumerate() {
            navbar_entries.push(NavBarEntry {
                title: page.title,
                is_root: true,
                expanded: false,
                page_index,
                item_index: None,
                focus_handle: cx.focus_handle().tab_index(0).tab_stop(true),
            });

            for (item_index, item) in page.items.iter().enumerate() {
                let SettingsPageItem::SectionHeader(title) = item else {
                    continue;
                };
                navbar_entries.push(NavBarEntry {
                    title,
                    is_root: false,
                    expanded: false,
                    page_index,
                    item_index: Some(item_index),
                    focus_handle: cx.focus_handle().tab_index(0).tab_stop(true),
                });
            }
        }

        self.navbar_entries = navbar_entries;
    }

    fn setup_navbar_focus_subscriptions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) {
        let mut focus_subscriptions = Vec::new();

        for entry_index in 0..self.navbar_entries.len() {
            let focus_handle = self.navbar_entries[entry_index].focus_handle.clone();

            let subscription = cx.on_focus(
                &focus_handle,
                window,
                move |this: &mut SettingsWindow,
                      window: &mut Window,
                      cx: &mut Context<SettingsWindow>| {
                    this.open_and_scroll_to_navbar_entry(entry_index, None, false, window, cx);
                },
            );
            focus_subscriptions.push(subscription);
        }
        self.navbar_focus_subscriptions = focus_subscriptions;
    }

    fn visible_navbar_entries(&self) -> impl Iterator<Item = (usize, &NavBarEntry)> {
        let mut index = 0;
        let entries = &self.navbar_entries;
        let search_matches = &self.filter_table;
        let has_query = self.has_query;
        std::iter::from_fn(move || {
            while index < entries.len() {
                let entry = &entries[index];
                let included_in_search = if let Some(item_index) = entry.item_index {
                    search_matches[entry.page_index][item_index]
                } else {
                    search_matches[entry.page_index].iter().any(|b| *b)
                        || search_matches[entry.page_index].is_empty()
                };
                if included_in_search {
                    break;
                }
                index += 1;
            }
            if index >= self.navbar_entries.len() {
                return None;
            }
            let entry = &entries[index];
            let entry_index = index;

            index += 1;
            if entry.is_root && !entry.expanded && !has_query {
                while index < entries.len() {
                    if entries[index].is_root {
                        break;
                    }
                    index += 1;
                }
            }

            return Some((entry_index, entry));
        })
    }

    fn filter_matches_to_file(&mut self) {
        let current_file = self.current_file.mask();
        for (page, page_filter) in std::iter::zip(&self.pages, &mut self.filter_table) {
            let mut header_index = 0;
            let mut any_found_since_last_header = true;

            for (index, item) in page.items.iter().enumerate() {
                match item {
                    SettingsPageItem::SectionHeader(_) => {
                        if !any_found_since_last_header {
                            page_filter[header_index] = false;
                        }
                        header_index = index;
                        any_found_since_last_header = false;
                    }
                    SettingsPageItem::SettingItem(SettingItem { files, .. })
                    | SettingsPageItem::SubPageLink(SubPageLink { files, .. })
                    | SettingsPageItem::DynamicItem(DynamicItem {
                        discriminant: SettingItem { files, .. },
                        ..
                    }) => {
                        if !files.contains(current_file) {
                            page_filter[index] = false;
                        } else {
                            any_found_since_last_header = true;
                        }
                    }
                    SettingsPageItem::UserLanguageSetting(_) => {
                        if !USER.contains(current_file) {
                            page_filter[index] = false;
                        } else {
                            any_found_since_last_header = true;
                        }
                    }
                    SettingsPageItem::ActionLink(ActionLink { files, .. }) => {
                        if !files.contains(current_file) {
                            page_filter[index] = false;
                        } else {
                            any_found_since_last_header = true;
                        }
                    }
                }
            }
            if let Some(last_header) = page_filter.get_mut(header_index)
                && !any_found_since_last_header
            {
                *last_header = false;
            }
        }
    }

    fn filter_by_json_path(&self, query: &str) -> Vec<usize> {
        let Some(path) = query.strip_prefix('#') else {
            return vec![];
        };
        let Some(search_index) = self.search_index.as_ref() else {
            return vec![];
        };
        let mut indices = vec![];
        for (index, SearchKeyLUTEntry { json_path, .. }) in search_index.key_lut.iter().enumerate()
        {
            let Some(json_path) = json_path else {
                continue;
            };

            if let Some(post) = json_path.strip_prefix(path)
                && (post.is_empty() || post.starts_with('.'))
            {
                indices.push(index);
            }
        }
        indices
    }

    fn apply_match_indices(&mut self, match_indices: impl Iterator<Item = usize>, query: &str) {
        let Some(search_index) = self.search_index.as_ref() else {
            return;
        };

        for page in &mut self.filter_table {
            page.fill(false);
        }

        for match_index in match_indices {
            let SearchKeyLUTEntry {
                page_index,
                header_index,
                item_index,
                ..
            } = search_index.key_lut[match_index];
            let page = &mut self.filter_table[page_index];
            page[header_index] = true;
            page[item_index] = true;
        }
        self.has_query = true;
        self.filter_matches_to_file();
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();
        self.open_best_matching_nav_page(&query_words);
        self.reset_list_state();
        self.scroll_content_to_best_match(&query_words);
    }

    fn update_matches(&mut self, cx: &mut Context<SettingsWindow>) {
        self.search_task.take();
        let query = self.search_bar.read(cx).text(cx);
        if query.is_empty() || self.search_index.is_none() {
            for page in &mut self.filter_table {
                page.fill(true);
            }
            self.has_query = false;
            self.filter_matches_to_file();
            self.reset_list_state();
            cx.notify();
            return;
        }

        let is_json_link_query = query.starts_with("#");
        if is_json_link_query {
            let indices = self.filter_by_json_path(&query);
            if !indices.is_empty() {
                self.apply_match_indices(indices.into_iter(), &query);
                cx.notify();
                return;
            }
        }

        let search_index = self.search_index.as_ref().unwrap().clone();

        self.search_task = Some(cx.spawn(async move |this, cx| {
            let exact_match_task = cx.background_spawn({
                let search_index = search_index.clone();
                let query = query.clone();
                async move {
                    let query_lower = query.to_lowercase();
                    let query_words: Vec<&str> = query_lower.split_whitespace().collect();
                    if query_words.is_empty() {
                        return Vec::new();
                    }
                    search_index
                        .documents
                        .iter()
                        .filter(|doc| {
                            query_words.iter().all(|query_word| {
                                doc.words
                                    .iter()
                                    .any(|doc_word| doc_word.starts_with(query_word))
                            })
                        })
                        .map(|doc| doc.id)
                        .collect::<Vec<usize>>()
                }
            });
            let cancel_flag = std::sync::atomic::AtomicBool::new(false);
            let fuzzy_search_task = fuzzy::match_strings(
                search_index.fuzzy_match_candidates.as_slice(),
                &query,
                false,
                true,
                search_index.fuzzy_match_candidates.len(),
                &cancel_flag,
                cx.background_executor().clone(),
            );

            let fuzzy_matches = fuzzy_search_task.await;
            let exact_matches = exact_match_task.await;

            this.update(cx, |this, cx| {
                let exact_indices = exact_matches.into_iter();
                let fuzzy_indices = fuzzy_matches
                    .into_iter()
                    .take_while(|fuzzy_match| fuzzy_match.score >= 0.5)
                    .map(|fuzzy_match| fuzzy_match.candidate_id);
                let merged_indices = exact_indices.chain(fuzzy_indices);

                this.apply_match_indices(merged_indices, &query);
                cx.notify();
            })
            .ok();

            cx.background_executor().timer(Duration::from_secs(1)).await;
        }));
    }

    fn build_filter_table(&mut self) {
        self.filter_table = self
            .pages
            .iter()
            .map(|page| vec![true; page.items.len()])
            .collect::<Vec<_>>();
    }

    fn build_search_index(&mut self, cx: &App) {
        fn split_into_words(parts: &[&str]) -> Vec<String> {
            parts
                .iter()
                .flat_map(|s| {
                    s.split(|c: char| !c.is_alphanumeric())
                        .filter(|w| !w.is_empty())
                        .map(|w| w.to_lowercase())
                })
                .collect()
        }

        let mut key_lut: Vec<SearchKeyLUTEntry> = vec![];
        let mut documents: Vec<SearchDocument> = Vec::default();
        let mut fuzzy_match_candidates = Vec::default();

        fn push_candidates(
            fuzzy_match_candidates: &mut Vec<StringMatchCandidate>,
            key_index: usize,
            input: &str,
        ) {
            for word in input.split_ascii_whitespace() {
                fuzzy_match_candidates.push(StringMatchCandidate::new(key_index, word));
            }
        }

        // PERF: We are currently searching all items even in project files
        // where many settings are filtered out, using the logic in filter_matches_to_file
        // we could only search relevant items based on the current file
        for (page_index, page) in self.pages.iter().enumerate() {
            let localized_page_title = settings_source_text(cx, page.title);
            let mut header_index = 0;
            let mut header_str = "";
            for (item_index, item) in page.items.iter().enumerate() {
                let key_index = key_lut.len();
                let mut json_path = None;
                let localized_header = settings_source_text(cx, header_str);
                match item {
                    SettingsPageItem::DynamicItem(DynamicItem {
                        discriminant: item, ..
                    })
                    | SettingsPageItem::SettingItem(item) => {
                        let localized_title = settings_source_text(cx, item.title);
                        let localized_description = settings_source_text(cx, item.description);
                        json_path = item
                            .field
                            .json_path()
                            .map(|path| path.trim_end_matches('$'));
                        documents.push(SearchDocument {
                            id: key_index,
                            words: split_into_words(&[
                                page.title,
                                header_str,
                                item.title,
                                item.description,
                                localized_page_title.as_ref(),
                                localized_header.as_ref(),
                                localized_title.as_ref(),
                                localized_description.as_ref(),
                            ]),
                        });
                        push_candidates(&mut fuzzy_match_candidates, key_index, item.title);
                        push_candidates(&mut fuzzy_match_candidates, key_index, item.description);
                        push_candidates(
                            &mut fuzzy_match_candidates,
                            key_index,
                            localized_title.as_ref(),
                        );
                        push_candidates(
                            &mut fuzzy_match_candidates,
                            key_index,
                            localized_description.as_ref(),
                        );
                    }
                    SettingsPageItem::UserLanguageSetting(setting_item) => {
                        json_path = Some("ui_language");
                        documents.push(SearchDocument {
                            id: key_index,
                            words: split_into_words(&[
                                page.title,
                                header_str,
                                setting_item.title.as_ref(),
                                setting_item.description.as_ref(),
                                "Language",
                                "Select the language that Flint uses for its interface.",
                            ]),
                        });
                        push_candidates(
                            &mut fuzzy_match_candidates,
                            key_index,
                            setting_item.title.as_ref(),
                        );
                        push_candidates(
                            &mut fuzzy_match_candidates,
                            key_index,
                            setting_item.description.as_ref(),
                        );
                        push_candidates(&mut fuzzy_match_candidates, key_index, "Language");
                    }
                    SettingsPageItem::SectionHeader(header) => {
                        let localized_header = settings_source_text(cx, header);
                        documents.push(SearchDocument {
                            id: key_index,
                            words: split_into_words(&[header, localized_header.as_ref()]),
                        });
                        push_candidates(&mut fuzzy_match_candidates, key_index, header);
                        push_candidates(
                            &mut fuzzy_match_candidates,
                            key_index,
                            localized_header.as_ref(),
                        );
                        header_index = item_index;
                        header_str = *header;
                    }
                    SettingsPageItem::SubPageLink(sub_page_link) => {
                        json_path = sub_page_link.json_path;
                        documents.push(SearchDocument {
                            id: key_index,
                            words: split_into_words(&[
                                page.title,
                                header_str,
                                sub_page_link.title.as_ref(),
                            ]),
                        });
                        push_candidates(
                            &mut fuzzy_match_candidates,
                            key_index,
                            sub_page_link.title.as_ref(),
                        );
                    }
                    SettingsPageItem::ActionLink(action_link) => {
                        documents.push(SearchDocument {
                            id: key_index,
                            words: split_into_words(&[
                                page.title,
                                header_str,
                                action_link.title.as_ref(),
                            ]),
                        });
                        push_candidates(
                            &mut fuzzy_match_candidates,
                            key_index,
                            action_link.title.as_ref(),
                        );
                    }
                }
                let current_localized_header = settings_source_text(cx, header_str);
                push_candidates(&mut fuzzy_match_candidates, key_index, page.title);
                push_candidates(&mut fuzzy_match_candidates, key_index, header_str);
                push_candidates(
                    &mut fuzzy_match_candidates,
                    key_index,
                    localized_page_title.as_ref(),
                );
                push_candidates(
                    &mut fuzzy_match_candidates,
                    key_index,
                    current_localized_header.as_ref(),
                );

                key_lut.push(SearchKeyLUTEntry {
                    page_index,
                    header_index,
                    item_index,
                    json_path,
                });
            }
        }
        self.search_index = Some(Arc::new(SearchIndex {
            documents,
            key_lut,
            fuzzy_match_candidates,
        }));
    }

    fn build_content_handles(&mut self, window: &mut Window, cx: &mut Context<SettingsWindow>) {
        self.content_handles = self
            .pages
            .iter()
            .map(|page| {
                std::iter::repeat_with(|| NonFocusableHandle::new(0, false, window, cx))
                    .take(page.items.len())
                    .collect()
            })
            .collect::<Vec<_>>();
    }

    fn reset_list_state(&mut self) {
        let mut visible_items_count = self.visible_page_items().count();

        if visible_items_count > 0 {
            // show page title if page is non empty
            visible_items_count += 1;
        }

        self.list_state.reset(visible_items_count);
    }

    fn build_ui(&mut self, window: &mut Window, cx: &mut Context<SettingsWindow>) {
        if self.pages.is_empty() {
            self.pages = page_data::settings_data(cx);
            self.build_navbar(cx);
            self.setup_navbar_focus_subscriptions(window, cx);
            self.build_content_handles(window, cx);
        }
        self.sub_page_stack.clear();
        // PERF: doesn't have to be rebuilt, can just be filled with true. pages is constant once it is built
        self.build_filter_table();
        self.reset_list_state();
        self.update_matches(cx);

        cx.notify();
    }

    fn rebuild_pages(&mut self, window: &mut Window, cx: &mut Context<SettingsWindow>) {
        self.pages.clear();
        self.navbar_entries.clear();
        self.navbar_focus_subscriptions.clear();
        self.content_handles.clear();
        self.build_ui(window, cx);
        self.build_search_index(cx);
    }

    #[track_caller]
    fn fetch_files(&mut self, window: &mut Window, cx: &mut Context<SettingsWindow>) {
        self.worktree_root_dirs.clear();
        let prev_files = self.files.clone();
        let settings_store = cx.global::<SettingsStore>();
        let mut ui_files = vec![];
        let mut all_files = settings_store.get_all_files();
        if !all_files.contains(&settings::SettingsFile::User) {
            all_files.push(settings::SettingsFile::User);
        }
        for file in all_files {
            let Some(settings_ui_file) = SettingsUiFile::from_settings(file) else {
                continue;
            };
            if settings_ui_file.is_server() {
                continue;
            }

            if let Some(worktree_id) = settings_ui_file.worktree_id() {
                let directory_name = all_projects(self.original_window.as_ref(), cx)
                    .find_map(|project| project.read(cx).worktree_for_id(worktree_id, cx))
                    .map(|worktree| worktree.read(cx).root_name());

                let Some(directory_name) = directory_name else {
                    log::error!(
                        "No directory name found for settings file at worktree ID: {}",
                        worktree_id
                    );
                    continue;
                };

                self.worktree_root_dirs
                    .insert(worktree_id, directory_name.as_unix_str().to_string());
            }

            let focus_handle = prev_files
                .iter()
                .find_map(|(prev_file, handle)| {
                    (prev_file == &settings_ui_file).then(|| handle.clone())
                })
                .unwrap_or_else(|| cx.focus_handle().tab_index(0).tab_stop(true));
            ui_files.push((settings_ui_file, focus_handle));
        }

        ui_files.reverse();

        if self.original_window.is_some() {
            let mut missing_worktrees = Vec::new();

            for worktree in all_projects(self.original_window.as_ref(), cx)
                .flat_map(|project| project.read(cx).visible_worktrees(cx))
                .filter(|tree| !self.worktree_root_dirs.contains_key(&tree.read(cx).id()))
            {
                let worktree = worktree.read(cx);
                let worktree_id = worktree.id();
                let Some(directory_name) = worktree.root_dir().and_then(|file| {
                    file.file_name()
                        .map(|os_string| os_string.to_string_lossy().to_string())
                }) else {
                    continue;
                };

                missing_worktrees.push((worktree_id, directory_name.clone()));
                let path = RelPath::empty().to_owned().into_arc();

                let settings_ui_file = SettingsUiFile::Project((worktree_id, path));

                let focus_handle = prev_files
                    .iter()
                    .find_map(|(prev_file, handle)| {
                        (prev_file == &settings_ui_file).then(|| handle.clone())
                    })
                    .unwrap_or_else(|| cx.focus_handle().tab_index(0).tab_stop(true));

                ui_files.push((settings_ui_file, focus_handle));
            }

            self.worktree_root_dirs.extend(missing_worktrees);
        }

        self.files = ui_files;
        let current_file_still_exists = self
            .files
            .iter()
            .any(|(file, _)| file == &self.current_file);
        if !current_file_still_exists {
            self.change_file(0, window, cx);
        }
    }

    fn open_navbar_entry_page(&mut self, navbar_entry: usize) {
        // Navigating to another page dismisses the transient "copied share
        // link" checkmark shown on a Skills page row.

        if !self.is_nav_entry_visible(navbar_entry) {
            self.open_first_nav_page();
        }

        let is_new_page = self.navbar_entries[self.navbar_entry].page_index
            != self.navbar_entries[navbar_entry].page_index;
        self.navbar_entry = navbar_entry;

        // We only need to reset visible items when updating matches
        // and selecting a new page
        if is_new_page {
            self.reset_list_state();
        }

        self.sub_page_stack.clear();
    }

    fn open_best_matching_nav_page(&mut self, query_words: &[&str]) {
        let mut entries = self.visible_navbar_entries().peekable();
        let first_entry = entries.peek().map(|(index, _)| (0, *index));
        let best_match = entries
            .enumerate()
            .filter(|(_, (_, entry))| !entry.is_root)
            .map(|(logical_index, (index, entry))| {
                let title_lower = entry.title.to_lowercase();
                let matching_words = query_words
                    .iter()
                    .filter(|query_word| {
                        title_lower
                            .split_whitespace()
                            .any(|title_word| title_word.starts_with(*query_word))
                    })
                    .count();
                (logical_index, index, matching_words)
            })
            .filter(|(_, _, count)| *count > 0)
            .max_by_key(|(_, _, count)| *count)
            .map(|(logical_index, index, _)| (logical_index, index));
        if let Some((logical_index, navbar_entry_index)) = best_match.or(first_entry) {
            self.open_navbar_entry_page(navbar_entry_index);
            self.navbar_scroll_handle
                .scroll_to_item(logical_index + 1, gpui::ScrollStrategy::Top);
        }
    }

    fn scroll_content_to_best_match(&self, query_words: &[&str]) {
        let position = self
            .visible_page_items()
            .enumerate()
            .find(|(_, (_, item))| match item {
                SettingsPageItem::SectionHeader(title) => {
                    let title_lower = title.to_lowercase();
                    query_words.iter().all(|query_word| {
                        title_lower
                            .split_whitespace()
                            .any(|title_word| title_word.starts_with(query_word))
                    })
                }
                _ => false,
            })
            .map(|(position, _)| position);
        if let Some(position) = position {
            self.list_state.scroll_to(gpui::ListOffset {
                item_ix: position + 1,
                offset_in_item: px(0.),
            });
        }
    }

    fn open_first_nav_page(&mut self) {
        let Some(first_navbar_entry_index) = self.visible_navbar_entries().next().map(|e| e.0)
        else {
            return;
        };
        self.open_navbar_entry_page(first_navbar_entry_index);
    }

    fn change_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<SettingsWindow>) {
        if ix >= self.files.len() {
            self.current_file = SettingsUiFile::User;
            self.build_ui(window, cx);
            return;
        }

        if self.files[ix].0 == self.current_file {
            return;
        }
        self.current_file = self.files[ix].0.clone();

        if let SettingsUiFile::Project((_, _)) = &self.current_file {}

        self.build_ui(window, cx);

        if self
            .visible_navbar_entries()
            .any(|(index, _)| index == self.navbar_entry)
        {
            self.open_and_scroll_to_navbar_entry(self.navbar_entry, None, true, window, cx);
        } else {
            self.open_first_nav_page();
        };
    }

    /// Changes the current settings file like [`Self::change_file`], but keeps
    /// the currently open sub-page stack when every sub-page in it is
    /// available in the new file's scope (e.g. switching a Skills sub-page
    /// between the user scope and a project scope).
    fn change_file_in_sub_page(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) {
        if ix >= self.files.len() || self.files[ix].0 == self.current_file {
            return;
        }
        self.current_file = self.files[ix].0.clone();

        if let SettingsUiFile::Project((_, _)) = &self.current_file {}

        let sub_page_stack = std::mem::take(&mut self.sub_page_stack);
        self.build_ui(window, cx);

        let file_mask = self.current_file.mask();
        if let Some(first_sub_page) = sub_page_stack.first()
            && sub_page_stack
                .iter()
                .all(|sub_page| sub_page.link.files.contains(file_mask))
        {
            if !self.is_nav_entry_visible(self.navbar_entry) {
                // The previously selected page may be filtered out in the new
                // scope (e.g. after deep-linking into a sub-page). Re-anchor
                // the navbar to the page containing the open sub-page, which
                // is visible because its sub-page link supports this scope.
                let anchor_entry = self
                    .pages
                    .iter()
                    .position(|page| {
                        page.items.iter().any(|item| {
                            matches!(item, SettingsPageItem::SubPageLink(link) if link == &first_sub_page.link)
                        })
                    })
                    .and_then(|page_index| {
                        self.navbar_entries
                            .iter()
                            .position(|entry| entry.is_root && entry.page_index == page_index)
                    });
                if let Some(anchor_entry) = anchor_entry
                    && self.is_nav_entry_visible(anchor_entry)
                {
                    self.open_navbar_entry_page(anchor_entry);
                }
            }
            if self.is_nav_entry_visible(self.navbar_entry) {
                self.sub_page_stack = sub_page_stack;
                cx.notify();
                return;
            }
        }

        if self.is_nav_entry_visible(self.navbar_entry) {
            self.open_and_scroll_to_navbar_entry(self.navbar_entry, None, true, window, cx);
        } else {
            self.open_first_nav_page();
        }
    }

    fn render_files_header(
        &self,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> impl IntoElement {
        static OVERFLOW_LIMIT: usize = 1;

        let file_button =
            |ix, file: &SettingsUiFile, focus_handle, cx: &mut Context<SettingsWindow>| {
                Button::new(
                    ix,
                    self.display_name(&file)
                        .expect("Files should always have a name"),
                )
                .toggle_state(file == &self.current_file)
                .selected_style(ButtonStyle::Tinted(ui::TintColor::Accent))
                .track_focus(focus_handle)
                .on_click(cx.listener({
                    let focus_handle = focus_handle.clone();
                    move |this, _: &gpui::ClickEvent, window, cx| {
                        this.change_file(ix, window, cx);
                        focus_handle.focus(window, cx);
                    }
                }))
            };

        let this = cx.entity();

        let selected_file_ix = self
            .files
            .iter()
            .enumerate()
            .skip(OVERFLOW_LIMIT)
            .find_map(|(ix, (file, _))| {
                if file == &self.current_file {
                    Some(ix)
                } else {
                    None
                }
            })
            .unwrap_or(OVERFLOW_LIMIT);
        let edit_in_json_id = SharedString::new(format!("edit-in-json-{}", selected_file_ix));

        h_flex()
            .w_full()
            .gap_1()
            .justify_between()
            .track_focus(&self.files_focus_handle)
            .tab_group()
            .tab_index(HEADER_GROUP_TAB_INDEX)
            .child(
                h_flex()
                    .gap_1()
                    .children(
                        self.files.iter().enumerate().take(OVERFLOW_LIMIT).map(
                            |(ix, (file, focus_handle))| file_button(ix, file, focus_handle, cx),
                        ),
                    )
                    .when(self.files.len() > OVERFLOW_LIMIT, |div| {
                        let (file, focus_handle) = &self.files[selected_file_ix];

                        div.child(file_button(selected_file_ix, file, focus_handle, cx))
                            .when(self.files.len() > OVERFLOW_LIMIT + 1, |div| {
                                div.child(
                                    DropdownMenu::new(
                                        "more-files",
                                        format!("+{}", self.files.len() - (OVERFLOW_LIMIT + 1)),
                                        ContextMenu::build(window, cx, move |mut menu, _, _| {
                                            for (mut ix, (file, focus_handle)) in self
                                                .files
                                                .iter()
                                                .enumerate()
                                                .skip(OVERFLOW_LIMIT + 1)
                                            {
                                                let (display_name, focus_handle) =
                                                    if selected_file_ix == ix {
                                                        ix = OVERFLOW_LIMIT;
                                                        (
                                                            self.display_name(&self.files[ix].0),
                                                            self.files[ix].1.clone(),
                                                        )
                                                    } else {
                                                        (
                                                            self.display_name(&file),
                                                            focus_handle.clone(),
                                                        )
                                                    };

                                                menu = menu.entry(
                                                    display_name
                                                        .expect("Files should always have a name"),
                                                    None,
                                                    {
                                                        let this = this.clone();
                                                        move |window, cx| {
                                                            this.update(cx, |this, cx| {
                                                                this.change_file(ix, window, cx);
                                                            });
                                                            focus_handle.focus(window, cx);
                                                        }
                                                    },
                                                );
                                            }

                                            menu
                                        }),
                                    )
                                    .style(DropdownStyle::Subtle)
                                    .trigger_tooltip(Tooltip::text(localization::text(
                                        cx,
                                        "settings-view-other-projects",
                                    )))
                                    .trigger_icon(IconName::ChevronDown)
                                    .attach(gpui::Anchor::BottomLeft)
                                    .offset(gpui::Point {
                                        x: px(0.0),
                                        y: px(2.0),
                                    })
                                    .tab_index(0),
                                )
                            })
                    }),
            )
            .child(
                Button::new(
                    edit_in_json_id,
                    localization::text(cx, "settings-edit-json"),
                )
                .tab_index(0_isize)
                .style(ButtonStyle::OutlinedGhost)
                .tooltip(Tooltip::for_action_title_in(
                    localization::text(cx, "settings-edit-json"),
                    &OpenCurrentFile,
                    &self.focus_handle,
                ))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_current_settings_file(window, cx);
                })),
            )
    }

    pub(crate) fn display_name(&self, file: &SettingsUiFile) -> Option<String> {
        match file {
            SettingsUiFile::User => Some("User".to_string()),
            SettingsUiFile::Project((worktree_id, path)) => self
                .worktree_root_dirs
                .get(&worktree_id)
                .map(|directory_name| {
                    let path_style = PathStyle::local();
                    if path.is_empty() {
                        directory_name.clone()
                    } else {
                        format!(
                            "{}{}{}",
                            directory_name,
                            path_style.primary_separator(),
                            path.display(path_style)
                        )
                    }
                }),
            SettingsUiFile::Server(file) => Some(file.to_string()),
        }
    }

    // TODO:
    //  Reconsider this after preview launch
    // fn file_location_str(&self) -> String {
    //     match &self.current_file {
    //         SettingsUiFile::User => "settings.json".to_string(),
    //         SettingsUiFile::Project((worktree_id, path)) => self
    //             .worktree_root_dirs
    //             .get(&worktree_id)
    //             .map(|directory_name| {
    //                 let path_style = PathStyle::local();
    //                 let file_path = path.join(paths::local_settings_file_relative_path());
    //                 format!(
    //                     "{}{}{}",
    //                     directory_name,
    //                     path_style.separator(),
    //                     file_path.display(path_style)
    //                 )
    //             })
    //             .expect("Current file should always be present in root dir map"),
    //         SettingsUiFile::Server(file) => file.to_string(),
    //     }
    // }

    fn render_search(&self, _window: &mut Window, cx: &mut App) -> Div {
        h_flex()
            .py_1()
            .px_1p5()
            .mb_3()
            .gap_1p5()
            .rounded_sm()
            .bg(cx.theme().colors().editor_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .child(Icon::new(IconName::MagnifyingGlass).color(Color::Muted))
            .child(self.search_bar.clone())
    }

    fn render_nav(
        &self,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> impl IntoElement {
        let visible_count = self.visible_navbar_entries().count();

        let focus_keybind_label = if self
            .navbar_focus_handle
            .read(cx)
            .handle
            .contains_focused(window, cx)
            || self
                .visible_navbar_entries()
                .any(|(_, entry)| entry.focus_handle.is_focused(window))
        {
            localization::text(cx, "settings-focus-content")
        } else {
            localization::text(cx, "settings-focus-navbar")
        };

        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("NavigationMenu");
        key_context.add("menu");
        if self.search_bar.focus_handle(cx).is_focused(window) {
            key_context.add("search");
        }

        v_flex()
            .key_context(key_context)
            .on_action(cx.listener(|this, _: &CollapseNavEntry, window, cx| {
                let Some(focused_entry) = this.focused_nav_entry(window, cx) else {
                    return;
                };
                let focused_entry_parent = this.root_entry_containing(focused_entry);
                if this.navbar_entries[focused_entry_parent].expanded {
                    this.toggle_navbar_entry(focused_entry_parent);
                    window.focus(&this.navbar_entries[focused_entry_parent].focus_handle, cx);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ExpandNavEntry, window, cx| {
                let Some(focused_entry) = this.focused_nav_entry(window, cx) else {
                    return;
                };
                if !this.navbar_entries[focused_entry].is_root {
                    return;
                }
                if !this.navbar_entries[focused_entry].expanded {
                    this.toggle_navbar_entry(focused_entry);
                }
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &FocusPreviousRootNavEntry, window, cx| {
                    let entry_index = this
                        .focused_nav_entry(window, cx)
                        .unwrap_or(this.navbar_entry);
                    let mut root_index = None;
                    for (index, entry) in this.visible_navbar_entries() {
                        if index >= entry_index {
                            break;
                        }
                        if entry.is_root {
                            root_index = Some(index);
                        }
                    }
                    let Some(previous_root_index) = root_index else {
                        return;
                    };
                    this.focus_and_scroll_to_nav_entry(previous_root_index, window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &FocusNextRootNavEntry, window, cx| {
                let entry_index = this
                    .focused_nav_entry(window, cx)
                    .unwrap_or(this.navbar_entry);
                let mut root_index = None;
                for (index, entry) in this.visible_navbar_entries() {
                    if index <= entry_index {
                        continue;
                    }
                    if entry.is_root {
                        root_index = Some(index);
                        break;
                    }
                }
                let Some(next_root_index) = root_index else {
                    return;
                };
                this.focus_and_scroll_to_nav_entry(next_root_index, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusFirstNavEntry, window, cx| {
                if let Some((first_entry_index, _)) = this.visible_navbar_entries().next() {
                    this.focus_and_scroll_to_nav_entry(first_entry_index, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusLastNavEntry, window, cx| {
                if let Some((last_entry_index, _)) = this.visible_navbar_entries().last() {
                    this.focus_and_scroll_to_nav_entry(last_entry_index, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusNextNavEntry, window, cx| {
                let entry_index = this
                    .focused_nav_entry(window, cx)
                    .unwrap_or(this.navbar_entry);
                let mut next_index = None;
                for (index, _) in this.visible_navbar_entries() {
                    if index > entry_index {
                        next_index = Some(index);
                        break;
                    }
                }
                let Some(next_entry_index) = next_index else {
                    return;
                };
                this.open_and_scroll_to_navbar_entry(
                    next_entry_index,
                    Some(gpui::ScrollStrategy::Bottom),
                    false,
                    window,
                    cx,
                );
            }))
            .on_action(cx.listener(|this, _: &FocusPreviousNavEntry, window, cx| {
                let entry_index = this
                    .focused_nav_entry(window, cx)
                    .unwrap_or(this.navbar_entry);
                let mut prev_index = None;
                for (index, _) in this.visible_navbar_entries() {
                    if index >= entry_index {
                        break;
                    }
                    prev_index = Some(index);
                }
                let Some(prev_entry_index) = prev_index else {
                    return;
                };
                this.open_and_scroll_to_navbar_entry(
                    prev_entry_index,
                    Some(gpui::ScrollStrategy::Top),
                    false,
                    window,
                    cx,
                );
            }))
            .w_56()
            .h_full()
            .p_2p5()
            .when(cfg!(target_os = "macos"), |this| this.pt_10())
            .flex_none()
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().panel_background)
            .child(self.render_search(window, cx))
            .child(
                v_flex()
                    .flex_1()
                    .overflow_hidden()
                    .track_focus(&self.navbar_focus_handle.focus_handle(cx))
                    .tab_group()
                    .tab_index(NAVBAR_GROUP_TAB_INDEX)
                    .child(
                        uniform_list(
                            "settings-ui-nav-bar",
                            visible_count + 1,
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                this.visible_navbar_entries()
                                    .skip(range.start.saturating_sub(1))
                                    .take(range.len())
                                    .map(|(entry_index, entry)| {
                                        TreeViewItem::new(
                                            ("settings-ui-navbar-entry", entry_index),
                                            settings_source_text(cx, entry.title),
                                        )
                                        .track_focus(&entry.focus_handle)
                                        .root_item(entry.is_root)
                                        .toggle_state(this.is_navbar_entry_selected(entry_index))
                                        .when(entry.is_root, |item| {
                                            item.expanded(entry.expanded || this.has_query)
                                                .on_toggle(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.toggle_and_focus_navbar_entry(
                                                            entry_index,
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                ))
                                        })
                                        .on_click({
                                            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                                                if this.toggle_navbar_entry_on_double_click(
                                                        entry_index,
                                                        event,
                                                        window,
                                                        cx,
                                                    )
                                                {
                                                    return;
                                                }


                                                this.open_and_scroll_to_navbar_entry(
                                                    entry_index,
                                                    None,
                                                    true,
                                                    window,
                                                    cx,
                                                );
                                            })
                                        })
                                    })
                                    .collect()
                            }),
                        )
                        .size_full()
                        .track_scroll(&self.navbar_scroll_handle),
                    )
                    .vertical_scrollbar_for(&self.navbar_scroll_handle, window, cx),
            )
            .child(
                h_flex()
                    .w_full()
                    .h_8()
                    .p_2()
                    .pb_0p5()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        KeybindingHint::new(
                            KeyBinding::for_action_in(
                                &ToggleFocusNav,
                                &self.navbar_focus_handle.focus_handle(cx),
                                cx,
                            ),
                            cx.theme().colors().surface_background.opacity(0.5),
                        )
                        .suffix(focus_keybind_label),
                    ),
            )
    }

    fn open_and_scroll_to_navbar_entry(
        &mut self,
        navbar_entry_index: usize,
        scroll_strategy: Option<gpui::ScrollStrategy>,
        focus_content: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_navbar_entry_page(navbar_entry_index);
        cx.notify();

        let mut handle_to_focus = None;

        if self.navbar_entries[navbar_entry_index].is_root
            || !self.is_nav_entry_visible(navbar_entry_index)
        {
            if let Some(scroll_handle) = self.current_sub_page_scroll_handle() {
                scroll_handle.set_offset(point(px(0.), px(0.)));
            }

            if focus_content {
                let Some(first_item_index) =
                    self.visible_page_items().next().map(|(index, _)| index)
                else {
                    return;
                };
                handle_to_focus = Some(self.focus_handle_for_content_element(first_item_index, cx));
            } else if !self.is_nav_entry_visible(navbar_entry_index) {
                let Some(first_visible_nav_entry_index) =
                    self.visible_navbar_entries().next().map(|(index, _)| index)
                else {
                    return;
                };
                self.focus_and_scroll_to_nav_entry(first_visible_nav_entry_index, window, cx);
            } else {
                handle_to_focus =
                    Some(self.navbar_entries[navbar_entry_index].focus_handle.clone());
            }
        } else {
            let entry_item_index = self.navbar_entries[navbar_entry_index]
                .item_index
                .expect("Non-root items should have an item index");
            self.scroll_to_content_item(entry_item_index, window, cx);
            if focus_content {
                handle_to_focus = Some(self.focus_handle_for_content_element(entry_item_index, cx));
            } else {
                handle_to_focus =
                    Some(self.navbar_entries[navbar_entry_index].focus_handle.clone());
            }
        }

        if let Some(scroll_strategy) = scroll_strategy
            && let Some(logical_entry_index) = self
                .visible_navbar_entries()
                .into_iter()
                .position(|(index, _)| index == navbar_entry_index)
        {
            self.navbar_scroll_handle
                .scroll_to_item(logical_entry_index + 1, scroll_strategy);
        }

        // Page scroll handle updates the active item index
        // in it's next paint call after using scroll_handle.scroll_to_top_of_item
        // The call after that updates the offset of the scroll handle. So to
        // ensure the scroll handle doesn't lag behind we need to render three frames
        // back to back.
        cx.on_next_frame(window, move |_, window, cx| {
            if let Some(handle) = handle_to_focus.as_ref() {
                window.focus(handle, cx);
            }

            cx.on_next_frame(window, |_, _, cx| {
                cx.notify();
            });
            cx.notify();
        });
        cx.notify();
    }

    fn scroll_to_content_item(
        &self,
        content_item_index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self
            .visible_page_items()
            .position(|(index, _)| index == content_item_index)
            .unwrap_or(0);
        if index == 0 {
            if let Some(scroll_handle) = self.current_sub_page_scroll_handle() {
                scroll_handle.set_offset(point(px(0.), px(0.)));
            }

            self.list_state.scroll_to(gpui::ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            return;
        }
        self.list_state.scroll_to(gpui::ListOffset {
            item_ix: index + 1,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    fn is_nav_entry_visible(&self, nav_entry_index: usize) -> bool {
        self.visible_navbar_entries()
            .any(|(index, _)| index == nav_entry_index)
    }

    fn focus_and_scroll_to_first_visible_nav_entry(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(nav_entry_index) = self.visible_navbar_entries().next().map(|(index, _)| index)
        {
            self.focus_and_scroll_to_nav_entry(nav_entry_index, window, cx);
        }
    }

    fn focus_and_scroll_to_nav_entry(
        &self,
        nav_entry_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(position) = self
            .visible_navbar_entries()
            .position(|(index, _)| index == nav_entry_index)
        else {
            return;
        };
        self.navbar_scroll_handle
            .scroll_to_item(position, gpui::ScrollStrategy::Top);
        window.focus(&self.navbar_entries[nav_entry_index].focus_handle, cx);
        cx.notify();
    }

    fn current_sub_page_scroll_handle(&self) -> Option<&ScrollHandle> {
        self.sub_page_stack.last().map(|page| &page.scroll_handle)
    }

    fn visible_page_items(&self) -> impl Iterator<Item = (usize, &SettingsPageItem)> {
        let page_idx = self.current_page_index();

        self.current_page()
            .items
            .iter()
            .enumerate()
            .filter(move |&(item_index, _)| self.filter_table[page_idx][item_index])
    }

    fn render_sub_page_breadcrumbs(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let scope_name: SharedString = self
            .display_name(&self.current_file)
            .unwrap_or_else(|| self.current_file.setting_type().to_string())
            .into();

        // Only offer scopes in which every sub-page in the stack is available.
        let allowed_mask = self
            .sub_page_stack
            .iter()
            .fold(USER | PROJECT | SERVER, |mask, sub_page| {
                mask & sub_page.link.files
            });
        let allowed_file_indices: Vec<usize> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, (file, _))| allowed_mask.contains(file.mask()))
            .map(|(ix, _)| ix)
            .collect();

        let scope_element = if allowed_file_indices.len() > 1 {
            let this = cx.entity();
            let scope_label = localization::text(cx, "settings-scope");
            DropdownMenu::new(
                "sub-page-scope-picker",
                scope_name,
                ContextMenu::build(window, cx, move |mut menu, _, _| {
                    menu = menu.header(scope_label.clone());

                    for ix in allowed_file_indices {
                        let (file, focus_handle) = &self.files[ix];
                        let display_name = self
                            .display_name(file)
                            .expect("Files should always have a name");

                        menu = menu.toggleable_entry(
                            display_name,
                            file == &self.current_file,
                            IconPosition::End,
                            None,
                            {
                                let this = this.clone();
                                let focus_handle = focus_handle.clone();
                                move |window, cx| {
                                    this.update(cx, |this, cx| {
                                        this.change_file_in_sub_page(ix, window, cx);
                                    });
                                    focus_handle.focus(window, cx);
                                }
                            },
                        );
                    }

                    menu
                }),
            )
            .style(DropdownStyle::Subtle)
            .trigger_tooltip(Tooltip::text(localization::text(
                cx,
                "settings-change-scope",
            )))
            .attach(gpui::Anchor::BottomLeft)
            .offset(gpui::Point {
                x: px(0.0),
                y: px(2.0),
            })
            .tab_index(0)
            .into_any_element()
        } else {
            Label::new(scope_name)
                .color(Color::Muted)
                .into_any_element()
        };

        h_flex()
            .min_w_0()
            .gap_1()
            .overflow_x_hidden()
            .child(scope_element)
            .child(Label::new("/").color(Color::Muted))
            .children(
                itertools::intersperse(
                    std::iter::once(self.current_page().title.into()).chain(
                        self.sub_page_stack
                            .iter()
                            .enumerate()
                            .flat_map(|(index, page)| {
                                (index == 0)
                                    .then(|| page.section_header.clone())
                                    .into_iter()
                                    .chain(std::iter::once(page.link.title.clone()))
                            }),
                    ),
                    "/".into(),
                )
                .map(|item| Label::new(item).color(Color::Muted)),
            )
    }

    fn render_no_results(&self, cx: &App) -> impl IntoElement {
        let search_query = self.search_bar.read(cx).text(cx);

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .child(Label::new(localization::text(cx, "settings-no-results")))
            .child(
                Label::new(localization::tr!(
                    cx,
                    "settings-no-results-detail",
                    query = search_query
                ))
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
    }

    fn render_current_page_items(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> impl IntoElement {
        let current_page_index = self.current_page_index();
        let mut page_content = v_flex().id("settings-ui-page").size_full();

        let has_active_search = !self.search_bar.read(cx).is_empty(cx);
        let has_no_results = self.visible_page_items().next().is_none() && has_active_search;

        if has_no_results {
            page_content = page_content.child(self.render_no_results(cx))
        } else {
            let last_non_header_index = self
                .visible_page_items()
                .filter_map(|(index, item)| {
                    (!matches!(item, SettingsPageItem::SectionHeader(_))).then_some(index)
                })
                .last();

            let root_nav_label = self
                .navbar_entries
                .iter()
                .find(|entry| entry.is_root && entry.page_index == self.current_page_index())
                .map(|entry| entry.title);

            let list_content = list(
                self.list_state.clone(),
                cx.processor(move |this, index, window, cx| {
                    if index == 0 {
                        return div()
                            .px_8()
                            .when(this.sub_page_stack.is_empty(), |this| {
                                this.when_some(root_nav_label, |this, title| {
                                    this.child(
                                        Label::new(settings_source_text(cx, title))
                                            .size(LabelSize::Large)
                                            .mt_2()
                                            .mb_3(),
                                    )
                                })
                            })
                            .into_any_element();
                    }

                    let mut visible_items = this.visible_page_items();
                    let Some((actual_item_index, item)) = visible_items.nth(index - 1) else {
                        return gpui::Empty.into_any_element();
                    };

                    let next_is_header = visible_items
                        .next()
                        .map(|(_, item)| matches!(item, SettingsPageItem::SectionHeader(_)))
                        .unwrap_or(false);

                    let is_last = Some(actual_item_index) == last_non_header_index;
                    let is_last_in_section = next_is_header || is_last;

                    let bottom_border = !is_last_in_section;
                    let extra_bottom_padding = is_last_in_section;

                    let item_focus_handle = this.content_handles[current_page_index]
                        [actual_item_index]
                        .focus_handle(cx);

                    v_flex()
                        .id(("settings-page-item", actual_item_index))
                        .track_focus(&item_focus_handle)
                        .w_full()
                        .min_w_0()
                        .child(item.render(
                            this,
                            actual_item_index,
                            bottom_border,
                            extra_bottom_padding,
                            window,
                            cx,
                        ))
                        .into_any_element()
                }),
            );

            page_content = page_content.child(list_content.size_full())
        }
        page_content
    }

    fn render_sub_page_items<'a, Items>(
        &self,
        items: Items,
        scroll_handle: &ScrollHandle,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> impl IntoElement
    where
        Items: Iterator<Item = (usize, &'a SettingsPageItem)>,
    {
        let page_content = v_flex()
            .id("settings-ui-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(scroll_handle);
        self.render_sub_page_items_in(page_content, items, false, window, cx)
    }

    fn render_sub_page_items_in<'a, Items>(
        &self,
        page_content: Stateful<Div>,
        items: Items,
        is_inline_section: bool,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> impl IntoElement
    where
        Items: Iterator<Item = (usize, &'a SettingsPageItem)>,
    {
        let items: Vec<_> = items.collect();
        let items_len = items.len();

        let has_active_search = !self.search_bar.read(cx).is_empty(cx);
        let has_no_results = items_len == 0 && has_active_search;

        if has_no_results {
            page_content.child(self.render_no_results(cx))
        } else {
            let last_non_header_index = items
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (_, item))| !matches!(item, SettingsPageItem::SectionHeader(_)))
                .map(|(index, _)| index);

            let root_nav_label = self
                .navbar_entries
                .iter()
                .find(|entry| entry.is_root && entry.page_index == self.current_page_index())
                .map(|entry| entry.title);

            page_content
                .when(self.sub_page_stack.is_empty(), |this| {
                    this.when_some(root_nav_label, |this, title| {
                        this.child(
                            Label::new(settings_source_text(cx, title))
                                .size(LabelSize::Large)
                                .mt_2()
                                .mb_3(),
                        )
                    })
                })
                .children(items.clone().into_iter().enumerate().map(
                    |(index, (actual_item_index, item))| {
                        let is_last_item = Some(index) == last_non_header_index;
                        let next_is_header = items.get(index + 1).is_some_and(|(_, next_item)| {
                            matches!(next_item, SettingsPageItem::SectionHeader(_))
                        });
                        let bottom_border = !is_inline_section && !next_is_header && !is_last_item;

                        let extra_bottom_padding =
                            !is_inline_section && (next_is_header || is_last_item);

                        v_flex()
                            .w_full()
                            .min_w_0()
                            .id(("settings-page-item", actual_item_index))
                            .child(item.render(
                                self,
                                actual_item_index,
                                bottom_border,
                                extra_bottom_padding,
                                window,
                                cx,
                            ))
                    },
                ))
        }
    }

    fn render_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> impl IntoElement {
        let page_header;
        let page_content;

        if let Some(current_sub_page) = self.sub_page_stack.last() {
            page_header = h_flex()
                .w_full()
                .min_w_0()
                .justify_between()
                .child(
                    h_flex()
                        .min_w_0()
                        .ml_neg_1p5()
                        .gap_1()
                        .child(
                            IconButton::new("back-btn", IconName::ArrowLeft)
                                .icon_size(IconSize::Small)
                                .shape(IconButtonShape::Square)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.pop_sub_page(window, cx);
                                })),
                        )
                        .child(self.render_sub_page_breadcrumbs(window, cx)),
                )
                .when(current_sub_page.link.in_json, |this| {
                    this.child(
                        div().flex_shrink_0().child(
                            Button::new(
                                "open-in-settings-file",
                                localization::text(cx, "settings-edit-json"),
                            )
                            .tab_index(0_isize)
                            .style(ButtonStyle::OutlinedGhost)
                            .tooltip(Tooltip::for_action_title_in(
                                localization::text(cx, "settings-edit-json"),
                                &OpenCurrentFile,
                                &self.focus_handle,
                            ))
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.open_current_settings_file(window, cx);
                                },
                            )),
                        ),
                    )
                })
                .into_any_element();

            let active_page_render_fn = &current_sub_page.link.render;
            page_content =
                (active_page_render_fn)(self, &current_sub_page.scroll_handle, window, cx);
        } else {
            page_header = self.render_files_header(window, cx).into_any_element();

            page_content = self
                .render_current_page_items(window, cx)
                .into_any_element();
        }

        let current_sub_page = self.sub_page_stack.last();

        let mut warning_banner = gpui::Empty.into_any_element();
        if let Some(error) =
            SettingsStore::global(cx).error_for_file(self.current_file.to_settings())
        {
            fn banner(
                label: SharedString,
                error: String,
                cx: &mut Context<SettingsWindow>,
            ) -> impl IntoElement {
                Banner::new()
                    .severity(Severity::Warning)
                    .child(
                        v_flex()
                            .my_0p5()
                            .gap_0p5()
                            .child(Label::new(label))
                            .child(Label::new(error).size(LabelSize::Small).color(Color::Muted)),
                    )
                    .action_slot(
                        div().pr_1().pb_1().child(
                            Button::new("fix-in-json", localization::text(cx, "settings-fix-json"))
                                .tab_index(0_isize)
                                .style(ButtonStyle::Tinted(ui::TintColor::Warning))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_current_settings_file(window, cx);
                                })),
                        ),
                    )
            }

            let parse_error = error.parse_error();
            let parse_failed = parse_error.is_some();

            warning_banner = v_flex()
                .gap_2()
                .when_some(parse_error, |this, err| {
                    this.child(banner(
                        localization::text(cx, "settings-load-failed"),
                        err,
                        cx,
                    ))
                })
                .map(|this| match &error.migration_status {
                    settings::MigrationStatus::Succeeded => this.child(banner(
                        localization::text(cx, "settings-outdated"),
                        match &self.current_file {
                            SettingsUiFile::User => {
                                localization::text(cx, "settings-migrate-automatic")
                            }
                            SettingsUiFile::Server(_) | SettingsUiFile::Project(_) => {
                                localization::text(cx, "settings-migrate-manual")
                            }
                        }
                        .to_string(),
                        cx,
                    )),
                    settings::MigrationStatus::Failed { error: err } if !parse_failed => this
                        .child(banner(
                            localization::text(cx, "settings-migration-failed"),
                            err.clone(),
                            cx,
                        )),
                    _ => this,
                })
                .into_any_element()
        }

        let mut restricted_banner = gpui::Empty.into_any_element();
        if let SettingsUiFile::Project((worktree_id, _)) = &self.current_file {
            let worktree_id = *worktree_id;
            let is_restricted = all_projects(self.original_window.as_ref(), cx)
                .find(|project| project.read(cx).worktree_for_id(worktree_id, cx).is_some())
                .map(|project| {
                    let worktree_store = project.read(cx).worktree_store();
                    project::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(
                        &worktree_store,
                        cx,
                    )
                })
                .unwrap_or(false);

            if is_restricted {
                let original_window = self.original_window;
                restricted_banner = Banner::new()
                    .severity(Severity::Warning)
                    .child(
                        v_flex()
                            .my_0p5()
                            .gap_0p5()
                            .child(Label::new(localization::text(
                                cx,
                                "settings-restricted-mode",
                            )))
                            .child(
                                Label::new(localization::text(cx, "settings-restricted-detail"))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .action_slot(
                        div().pr_2().pb_1().child(
                            Button::new(
                                "manage-trust",
                                localization::text(cx, "settings-manage-trust"),
                            )
                            .style(ButtonStyle::Tinted(ui::TintColor::Warning))
                            .on_click(cx.listener(
                                move |_this, _, window, cx| {
                                    if let Some(original_window) = original_window {
                                        original_window
                                            .update(cx, |multi_workspace, window, cx| {
                                                multi_workspace.workspace().update(
                                                    cx,
                                                    |workspace, cx| {
                                                        workspace
                                                            .show_worktree_trust_security_modal(
                                                                true, window, cx,
                                                            );
                                                    },
                                                );
                                            })
                                            .log_err();
                                    }
                                    // Close the settings window
                                    window.remove_window();
                                },
                            )),
                        ),
                    )
                    .into_any_element();
            }
        }

        v_flex()
            .id("settings-ui-page")
            .on_action(cx.listener(|this, _: &menu::SelectNext, window, cx| {
                if !this.sub_page_stack.is_empty() {
                    window.focus_next(cx);
                    return;
                }
                for (logical_index, (actual_index, _)) in this.visible_page_items().enumerate() {
                    let handle = this.content_handles[this.current_page_index()][actual_index]
                        .focus_handle(cx);
                    let mut offset = 1; // for page header

                    if let Some((_, next_item)) = this.visible_page_items().nth(logical_index + 1)
                        && matches!(next_item, SettingsPageItem::SectionHeader(_))
                    {
                        offset += 1;
                    }
                    if handle.contains_focused(window, cx) {
                        let next_logical_index = logical_index + offset + 1;
                        this.list_state.scroll_to_reveal_item(next_logical_index);
                        // We need to render the next item to ensure it's focus handle is in the element tree
                        cx.on_next_frame(window, |_, window, cx| {
                            cx.notify();
                            cx.on_next_frame(window, |_, window, cx| {
                                window.focus_next(cx);
                                cx.notify();
                            });
                        });
                        cx.notify();
                        return;
                    }
                }
                window.focus_next(cx);
            }))
            .on_action(cx.listener(|this, _: &menu::SelectPrevious, window, cx| {
                if !this.sub_page_stack.is_empty() {
                    window.focus_prev(cx);
                    return;
                }
                let mut prev_was_header = false;
                for (logical_index, (actual_index, item)) in this.visible_page_items().enumerate() {
                    let is_header = matches!(item, SettingsPageItem::SectionHeader(_));
                    let handle = this.content_handles[this.current_page_index()][actual_index]
                        .focus_handle(cx);
                    let mut offset = 1; // for page header

                    if prev_was_header {
                        offset -= 1;
                    }
                    if handle.contains_focused(window, cx) {
                        let next_logical_index = logical_index + offset - 1;
                        this.list_state.scroll_to_reveal_item(next_logical_index);
                        // We need to render the next item to ensure it's focus handle is in the element tree
                        cx.on_next_frame(window, |_, window, cx| {
                            cx.notify();
                            cx.on_next_frame(window, |_, window, cx| {
                                window.focus_prev(cx);
                                cx.notify();
                            });
                        });
                        cx.notify();
                        return;
                    }
                    prev_was_header = is_header;
                }
                window.focus_prev(cx);
            }))
            .when(current_sub_page.is_none(), |this| {
                this.vertical_scrollbar_for(&self.list_state, window, cx)
            })
            .when_some(current_sub_page, |this, current_sub_page| {
                this.custom_scrollbars(
                    Scrollbars::new(ui::ScrollAxes::Vertical)
                        .tracked_scroll_handle(&current_sub_page.scroll_handle)
                        .id((current_sub_page.link.title.clone(), 42)),
                    window,
                    cx,
                )
            })
            .track_focus(&self.content_focus_handle.focus_handle(cx))
            .pt_6()
            .gap_4()
            .flex_1()
            .min_w_0()
            .bg(cx.theme().colors().editor_background)
            .child(
                v_flex()
                    .px_8()
                    .gap_2()
                    .child(page_header)
                    .child(warning_banner)
                    .child(restricted_banner),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .size_full()
                    .tab_group()
                    .tab_index(CONTENT_GROUP_TAB_INDEX)
                    .child(page_content),
            )
    }

    /// This function will create a new settings file if one doesn't exist
    /// if the current file is a project settings with a valid worktree id
    /// We do this because the settings ui allows initializing project settings
    fn open_current_settings_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.current_file {
            SettingsUiFile::User => {
                let Some(original_window) = self.original_window else {
                    return;
                };
                original_window
                    .update(cx, |multi_workspace, window, cx| {
                        multi_workspace
                            .workspace()
                            .clone()
                            .update(cx, |workspace, cx| {
                                workspace
                                    .with_local_or_wsl_workspace(
                                        window,
                                        cx,
                                        open_user_settings_in_workspace,
                                    )
                                    .detach();
                            });
                    })
                    .ok();

                window.remove_window();
            }
            SettingsUiFile::Project((worktree_id, path)) => {
                let settings_path = path.join(paths::local_settings_file_relative_path());
                let app_state = workspace::AppState::global(cx);

                let Some((workspace_window, worktree, corresponding_workspace)) = app_state
                    .workspace_store
                    .read(cx)
                    .workspaces_with_windows()
                    .filter_map(|(window_handle, weak)| {
                        let workspace = weak.upgrade()?;
                        let window = window_handle.downcast::<MultiWorkspace>()?;
                        Some((window, workspace))
                    })
                    .find_map(|(window, workspace): (_, Entity<Workspace>)| {
                        workspace
                            .read(cx)
                            .project()
                            .read(cx)
                            .worktree_for_id(*worktree_id, cx)
                            .map(|worktree| (window, worktree, workspace))
                    })
                else {
                    log::error!(
                        "No corresponding workspace contains worktree id: {}",
                        worktree_id
                    );

                    return;
                };

                let create_task = if worktree.read(cx).entry_for_path(&settings_path).is_some() {
                    None
                } else {
                    Some(worktree.update(cx, |tree, cx| {
                        tree.create_entry(
                            settings_path.clone(),
                            false,
                            Some(initial_project_settings_content().as_bytes().to_vec()),
                            cx,
                        )
                    }))
                };

                let worktree_id = *worktree_id;

                // TODO: move flint::open_local_file() APIs to this crate, and
                // re-implement the "initial_contents" behavior
                let workspace_weak = corresponding_workspace.downgrade();
                workspace_window
                    .update(cx, |_, window, cx| {
                        cx.spawn_in(window, async move |_, cx| {
                            if let Some(create_task) = create_task {
                                create_task.await.ok()?;
                            };

                            workspace_weak
                                .update_in(cx, |workspace, window, cx| {
                                    workspace.open_path(
                                        (worktree_id, settings_path.clone()),
                                        None,
                                        true,
                                        window,
                                        cx,
                                    )
                                })
                                .ok()?
                                .await
                                .log_err()?;

                            workspace_weak
                                .update_in(cx, |_, window, cx| {
                                    window.activate_window();
                                    cx.notify();
                                })
                                .ok();

                            Some(())
                        })
                        .detach();
                    })
                    .ok();

                window.remove_window();
            }
            SettingsUiFile::Server(_) => {
                // Server files are not editable
                return;
            }
        };
    }

    fn current_page_index(&self) -> usize {
        if self.navbar_entries.is_empty() {
            return 0;
        }

        self.navbar_entries[self.navbar_entry].page_index
    }

    fn current_page(&self) -> &SettingsPage {
        &self.pages[self.current_page_index()]
    }

    fn is_navbar_entry_selected(&self, ix: usize) -> bool {
        ix == self.navbar_entry
    }

    fn push_sub_page(
        &mut self,
        sub_page_link: SubPageLink,
        section_header: SharedString,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) {
        self.sub_page_stack
            .push(SubPage::new(sub_page_link, section_header));
        self.content_focus_handle.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Push a dynamically-created sub-page with a custom render function.
    /// This is useful for nested sub-pages that aren't defined in the main pages list.
    pub fn push_dynamic_sub_page(
        &mut self,
        title: impl Into<SharedString>,
        section_header: impl Into<SharedString>,
        json_path: Option<&'static str>,
        render: fn(
            &SettingsWindow,
            &ScrollHandle,
            &mut Window,
            &mut Context<SettingsWindow>,
        ) -> AnyElement,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) {
        self.regex_validation_error = None;
        let sub_page_link = SubPageLink {
            title: title.into(),
            r#type: SubPageType::default(),
            description: None,
            json_path,
            in_json: true,
            files: USER,
            render,
        };
        self.push_sub_page(sub_page_link, section_header.into(), window, cx);
    }

    /// Navigate to a sub-page by its json_path.
    /// Returns true if the sub-page was found and pushed, false otherwise.
    pub fn navigate_to_sub_page(
        &mut self,
        json_path: &str,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> bool {
        for page in &self.pages {
            for (item_index, item) in page.items.iter().enumerate() {
                if let SettingsPageItem::SubPageLink(sub_page_link) = item {
                    if sub_page_link.json_path == Some(json_path) {
                        let section_header = page
                            .items
                            .iter()
                            .take(item_index)
                            .rev()
                            .find_map(|item| item.header_text().map(SharedString::new_static))
                            .unwrap_or_else(|| localization::text(cx, "settings-default-section"));

                        self.push_sub_page(sub_page_link.clone(), section_header, window, cx);
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Navigate to a setting by its json_path.
    /// Clears the sub-page stack and scrolls to the setting item.
    /// Returns true if the setting was found, false otherwise.
    pub fn navigate_to_setting(
        &mut self,
        json_path: &str,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> bool {
        self.sub_page_stack.clear();

        for (page_index, page) in self.pages.iter().enumerate() {
            for (item_index, item) in page.items.iter().enumerate() {
                let item_json_path = match item {
                    SettingsPageItem::SettingItem(setting_item) => setting_item.field.json_path(),
                    SettingsPageItem::UserLanguageSetting(_) => Some("ui_language"),
                    SettingsPageItem::DynamicItem(dynamic_item) => {
                        dynamic_item.discriminant.field.json_path()
                    }
                    _ => None,
                };
                if item_json_path == Some(json_path) {
                    if let Some(navbar_entry_index) = self
                        .navbar_entries
                        .iter()
                        .position(|e| e.page_index == page_index && e.is_root)
                    {
                        self.open_and_scroll_to_navbar_entry(
                            navbar_entry_index,
                            None,
                            false,
                            window,
                            cx,
                        );
                        self.scroll_to_content_item(item_index, window, cx);
                        return true;
                    }
                }
            }
        }
        false
    }

    fn pop_sub_page(&mut self, window: &mut Window, cx: &mut Context<SettingsWindow>) {
        self.regex_validation_error = None;
        self.sub_page_stack.pop();
        self.content_focus_handle.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn focus_file_at_index(&mut self, index: usize, window: &mut Window, cx: &mut App) {
        if let Some((_, handle)) = self.files.get(index) {
            handle.focus(window, cx);
        }
    }

    fn focused_file_index(&self, window: &Window, cx: &Context<Self>) -> usize {
        if self.files_focus_handle.contains_focused(window, cx)
            && let Some(index) = self
                .files
                .iter()
                .position(|(_, handle)| handle.is_focused(window))
        {
            return index;
        }
        if let Some(current_file_index) = self
            .files
            .iter()
            .position(|(file, _)| file == &self.current_file)
        {
            return current_file_index;
        }
        0
    }

    fn focus_handle_for_content_element(
        &self,
        actual_item_index: usize,
        cx: &Context<Self>,
    ) -> FocusHandle {
        let page_index = self.current_page_index();
        self.content_handles[page_index][actual_item_index].focus_handle(cx)
    }

    fn focused_nav_entry(&self, window: &Window, cx: &App) -> Option<usize> {
        if !self
            .navbar_focus_handle
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            return None;
        }
        for (index, entry) in self.navbar_entries.iter().enumerate() {
            if entry.focus_handle.is_focused(window) {
                return Some(index);
            }
        }
        None
    }

    fn root_entry_containing(&self, nav_entry_index: usize) -> usize {
        let mut index = Some(nav_entry_index);
        while let Some(prev_index) = index
            && !self.navbar_entries[prev_index].is_root
        {
            index = prev_index.checked_sub(1);
        }
        return index.expect("No root entry found");
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui_font = theme_settings::setup_ui_font(window, cx);

        client_side_decorations(
            v_flex()
                .text_color(cx.theme().colors().text)
                .size_full()
                .children(self.title_bar.clone())
                .child(
                    div()
                        .id("settings-window")
                        .key_context("SettingsWindow")
                        .track_focus(&self.focus_handle)
                        .on_action(cx.listener(|this, _: &OpenCurrentFile, window, cx| {
                            this.open_current_settings_file(window, cx);
                        }))
                        .on_action(|_: &Minimize, window, _cx| {
                            window.minimize_window();
                        })
                        .on_action(cx.listener(|this, _: &search::FocusSearch, window, cx| {
                            this.search_bar.focus_handle(cx).focus(window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &ToggleFocusNav, window, cx| {
                            if this
                                .navbar_focus_handle
                                .focus_handle(cx)
                                .contains_focused(window, cx)
                            {
                                this.open_and_scroll_to_navbar_entry(
                                    this.navbar_entry,
                                    None,
                                    true,
                                    window,
                                    cx,
                                );
                            } else {
                                this.focus_and_scroll_to_nav_entry(this.navbar_entry, window, cx);
                            }
                        }))
                        .on_action(cx.listener(
                            |this, FocusFile(file_index): &FocusFile, window, cx| {
                                this.focus_file_at_index(*file_index as usize, window, cx);
                            },
                        ))
                        .on_action(cx.listener(|this, _: &FocusNextFile, window, cx| {
                            let next_index = usize::min(
                                this.focused_file_index(window, cx) + 1,
                                this.files.len().saturating_sub(1),
                            );
                            this.focus_file_at_index(next_index, window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &FocusPreviousFile, window, cx| {
                            let prev_index = this.focused_file_index(window, cx).saturating_sub(1);
                            this.focus_file_at_index(prev_index, window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &menu::SelectNext, window, cx| {
                            if this
                                .search_bar
                                .focus_handle(cx)
                                .contains_focused(window, cx)
                            {
                                this.focus_and_scroll_to_first_visible_nav_entry(window, cx);
                            } else {
                                window.focus_next(cx);
                            }
                        }))
                        .on_action(|_: &menu::SelectPrevious, window, cx| {
                            window.focus_prev(cx);
                        })
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_h_0()
                        .font(ui_font)
                        .bg(cx.theme().colors().background)
                        .text_color(cx.theme().colors().text)
                        .when(!cfg!(target_os = "macos"), |this| {
                            this.border_t_1().border_color(cx.theme().colors().border)
                        })
                        .child(self.render_nav(window, cx))
                        .child(self.render_page(window, cx)),
                ),
            window,
            cx,
            Tiling::default(),
        )
    }
}

fn all_projects(
    window: Option<&WindowHandle<MultiWorkspace>>,
    cx: &App,
) -> impl Iterator<Item = Entity<Project>> {
    let mut seen_project_ids = std::collections::HashSet::new();
    let app_state = workspace::AppState::global(cx);
    app_state
        .workspace_store
        .read(cx)
        .workspaces()
        .filter_map(|weak| weak.upgrade())
        .map(|workspace: Entity<Workspace>| workspace.read(cx).project().clone())
        .chain(
            window
                .and_then(|handle| handle.read(cx).ok())
                .into_iter()
                .flat_map(|multi_workspace| {
                    multi_workspace
                        .workspaces()
                        .map(|workspace| workspace.read(cx).project().clone())
                        .collect::<Vec<_>>()
                }),
        )
        .filter(move |project| seen_project_ids.insert(project.entity_id()))
}

fn open_user_settings_in_workspace(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();

    cx.spawn_in(window, async move |workspace, cx| {
        let (config_dir, settings_file) = project.update(cx, |project, cx| {
            (
                project.try_windows_path_to_wsl(paths::config_dir().as_path(), cx),
                project.try_windows_path_to_wsl(paths::settings_file().as_path(), cx),
            )
        });
        let config_dir = config_dir.await?;
        let settings_file = settings_file.await?;
        project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(&config_dir, false, cx)
            })
            .await
            .ok();
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_paths(
                    vec![settings_file],
                    OpenOptions {
                        visible: Some(OpenVisible::None),
                        ..Default::default()
                    },
                    None,
                    window,
                    cx,
                )
            })?
            .await;

        workspace.update_in(cx, |_, window, cx| {
            window.activate_window();
            cx.notify();
        })
    })
    .detach();
}

fn update_settings_file(
    file: SettingsUiFile,
    _file_name: Option<&'static str>,
    window: &mut Window,
    cx: &mut App,
    update: impl 'static + Send + FnOnce(&mut SettingsContent, &App),
) -> Result<()> {
    match file {
        SettingsUiFile::Project((worktree_id, rel_path)) => {
            let rel_path = rel_path.join(paths::local_settings_file_relative_path());
            let Some(settings_window) = window.root::<SettingsWindow>().flatten() else {
                anyhow::bail!("No settings window found");
            };

            update_project_setting_file(worktree_id, rel_path, update, settings_window, cx)
        }
        SettingsUiFile::User => {
            // todo(settings_ui) error?
            SettingsStore::global(cx).update_settings_file(<dyn fs::Fs>::global(cx), update);
            Ok(())
        }
        SettingsUiFile::Server(_) => unimplemented!(),
    }
}

struct ProjectSettingsUpdateEntry {
    worktree_id: WorktreeId,
    rel_path: Arc<RelPath>,
    settings_window: WeakEntity<SettingsWindow>,
    project: WeakEntity<Project>,
    worktree: WeakEntity<Worktree>,
    update: Box<dyn FnOnce(&mut SettingsContent, &App)>,
}

struct ProjectSettingsUpdateQueue {
    tx: mpsc::UnboundedSender<ProjectSettingsUpdateEntry>,
    _task: Task<()>,
}

impl Global for ProjectSettingsUpdateQueue {}

impl ProjectSettingsUpdateQueue {
    fn new(cx: &mut App) -> Self {
        let (tx, mut rx) = mpsc::unbounded();
        let task = cx.spawn(async move |mut cx| {
            while let Some(entry) = rx.next().await {
                if let Err(err) = Self::process_entry(entry, &mut cx).await {
                    log::error!("Failed to update project settings: {err:?}");
                }
            }
        });
        Self { tx, _task: task }
    }

    fn enqueue(cx: &mut App, entry: ProjectSettingsUpdateEntry) {
        cx.update_global::<Self, _>(|queue, _cx| {
            if let Err(err) = queue.tx.unbounded_send(entry) {
                log::error!("Failed to enqueue project settings update: {err}");
            }
        });
    }

    async fn process_entry(entry: ProjectSettingsUpdateEntry, cx: &mut AsyncApp) -> Result<()> {
        let ProjectSettingsUpdateEntry {
            worktree_id,
            rel_path,
            settings_window,
            project,
            worktree,
            update,
        } = entry;

        let project_path = ProjectPath {
            worktree_id,
            path: rel_path.clone(),
        };

        let needs_creation = worktree.read_with(cx, |worktree, _| {
            worktree.entry_for_path(&rel_path).is_none()
        })?;

        if needs_creation {
            worktree
                .update(cx, |worktree, cx| {
                    worktree.create_entry(rel_path.clone(), false, None, cx)
                })?
                .await?;
        }

        let buffer_store = project.read_with(cx, |project, _cx| project.buffer_store().clone())?;

        let cached_buffer = settings_window
            .read_with(cx, |settings_window, _| {
                settings_window
                    .project_setting_file_buffers
                    .get(&project_path)
                    .cloned()
            })
            .unwrap_or_default();

        let buffer = if let Some(cached_buffer) = cached_buffer {
            let needs_reload = cached_buffer.read_with(cx, |buffer, _| buffer.has_conflict());
            if needs_reload {
                cached_buffer
                    .update(cx, |buffer, cx| buffer.reload(cx))
                    .await
                    .context("Failed to reload settings file")?;
            }
            cached_buffer
        } else {
            let buffer = buffer_store
                .update(cx, |store, cx| store.open_buffer(project_path.clone(), cx))
                .await
                .context("Failed to open settings file")?;

            let _ = settings_window.update(cx, |this, _cx| {
                this.project_setting_file_buffers
                    .insert(project_path, buffer.clone());
            });

            buffer
        };

        buffer.update(cx, |buffer, cx| {
            let current_text = buffer.text();
            if let Some(new_text) = cx
                .global::<SettingsStore>()
                .new_text_for_update(current_text, |settings| update(settings, cx))
                .log_err()
            {
                buffer.edit([(0..buffer.len(), new_text)], None, cx);
            }
        });

        buffer_store
            .update(cx, |store, cx| store.save_buffer(buffer, cx))
            .await
            .context("Failed to save settings file")?;

        Ok(())
    }
}

fn update_project_setting_file(
    worktree_id: WorktreeId,
    rel_path: Arc<RelPath>,
    update: impl 'static + FnOnce(&mut SettingsContent, &App),
    settings_window: Entity<SettingsWindow>,
    cx: &mut App,
) -> Result<()> {
    let Some((worktree, project)) =
        all_projects(settings_window.read(cx).original_window.as_ref(), cx).find_map(|project| {
            project
                .read(cx)
                .worktree_for_id(worktree_id, cx)
                .zip(Some(project))
        })
    else {
        anyhow::bail!("Could not find project with worktree id: {}", worktree_id);
    };

    let entry = ProjectSettingsUpdateEntry {
        worktree_id,
        rel_path,
        settings_window: settings_window.downgrade(),
        project: project.downgrade(),
        worktree: worktree.downgrade(),
        update: Box::new(update),
    };

    ProjectSettingsUpdateQueue::enqueue(cx, entry);

    Ok(())
}

struct CurrentSettingsValue<'a, T> {
    value: &'a T,
    disabled: bool,
}

fn get_current_value<'a, T>(
    settings_store: &'a SettingsStore,
    file: &SettingsUiFile,
    field: &'a SettingField<T>,
    _cx: &'a App,
) -> Option<CurrentSettingsValue<'a, T>> {
    let (_file, value) = settings_store.get_value_from_file(file.to_settings(), field.pick);
    let value = value?;

    Some(CurrentSettingsValue {
        disabled: false,
        value,
    })
}

fn render_text_field<T: From<String> + Into<String> + AsRef<str> + Clone>(
    field: SettingField<T>,
    file: SettingsUiFile,
    metadata: Option<&SettingsFieldMetadata>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let (_, initial_text) =
        SettingsStore::global(cx).get_value_from_file(file.to_settings(), field.pick);
    let initial_text = initial_text.filter(|s| !s.as_ref().is_empty());

    SettingsInputField::new()
        .tab_index(0)
        .when_some(initial_text, |editor, text| {
            editor.with_initial_text(text.as_ref().to_string())
        })
        .when_some(
            metadata.and_then(|metadata| metadata.placeholder),
            |editor, placeholder| editor.with_placeholder(placeholder),
        )
        .on_confirm({
            move |new_text, window, cx| {
                update_settings_file(
                    file.clone(),
                    field.json_path,
                    window,
                    cx,
                    move |settings, app| {
                        (field.write)(settings, new_text.map(Into::into), app);
                    },
                )
                .log_err(); // todo(settings_ui) don't log err
            }
        })
        .into_any_element()
}

fn render_toggle_button<B: Into<bool> + From<bool> + Copy>(
    field: SettingField<B>,
    file: SettingsUiFile,
    _metadata: Option<&SettingsFieldMetadata>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let value = get_current_value(&SettingsStore::global(cx), &file, &field, cx);
    let (value, disabled) = value
        .map(|current_value| (*current_value.value, current_value.disabled))
        .unwrap_or((false.into(), false));

    let toggle_state = if value.into() {
        ToggleState::Selected
    } else {
        ToggleState::Unselected
    };

    Switch::new("toggle_button", toggle_state)
        .tab_index(0_isize)
        .disabled(disabled)
        .on_click({
            move |state, window, cx| {
                let state = *state == ui::ToggleState::Selected;
                update_settings_file(
                    file.clone(),
                    field.json_path,
                    window,
                    cx,
                    move |settings, app| {
                        (field.write)(settings, Some(state.into()), app);
                    },
                )
                .log_err(); // todo(settings_ui) don't log err
            }
        })
        .into_any_element()
}

fn render_editable_number_field<T: NumberFieldType + Send + Sync>(
    field: SettingField<T>,
    file: SettingsUiFile,
    _metadata: Option<&SettingsFieldMetadata>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let (_, value) = SettingsStore::global(cx).get_value_from_file(file.to_settings(), field.pick);
    let value = value.copied().unwrap_or_else(T::min_value);

    let id = field
        .json_path
        .map(|p| format!("numeric_stepper_{}", p))
        .unwrap_or_else(|| "numeric_stepper".to_string());

    NumberField::new(id, value, window, cx)
        .mode(NumberFieldMode::Edit, cx)
        .tab_index(0_isize)
        .on_change({
            move |value, window, cx| {
                let value = *value;
                update_settings_file(
                    file.clone(),
                    field.json_path,
                    window,
                    cx,
                    move |settings, app| {
                        (field.write)(settings, Some(value), app);
                    },
                )
                .log_err(); // todo(settings_ui) don't log err
            }
        })
        .into_any_element()
}

fn render_dropdown<T>(
    field: SettingField<T>,
    file: SettingsUiFile,
    metadata: Option<&SettingsFieldMetadata>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement
where
    T: strum::VariantArray + strum::VariantNames + Copy + PartialEq + Send + Sync + 'static,
{
    let variants = || -> &'static [T] { <T as strum::VariantArray>::VARIANTS };
    let labels = || -> &'static [&'static str] { <T as strum::VariantNames>::VARIANTS };
    let should_do_titlecase = metadata
        .and_then(|metadata| metadata.should_do_titlecase)
        .unwrap_or(true);

    let current_value = get_current_value(&SettingsStore::global(cx), &file, &field, cx);
    let (current_value, disabled) = current_value
        .map(|current_value| (*current_value.value, current_value.disabled))
        .unwrap_or((variants()[0], false));

    EnumVariantDropdown::new("dropdown", current_value, variants(), labels(), {
        move |value, window, cx| {
            if value == current_value {
                return;
            }
            update_settings_file(
                file.clone(),
                field.json_path,
                window,
                cx,
                move |settings, app| {
                    (field.write)(settings, Some(value), app);
                },
            )
            .log_err(); // todo(settings_ui) don't log err
        }
    })
    .disabled(disabled)
    .tab_index(0)
    .title_case(should_do_titlecase)
    .into_any_element()
}

fn render_picker_trigger_button(id: SharedString, label: SharedString) -> Button {
    Button::new(id, label)
        .tab_index(0_isize)
        .style(ButtonStyle::Outlined)
        .size(ButtonSize::Medium)
        .end_icon(
            Icon::new(IconName::ChevronUpDown)
                .size(IconSize::Small)
                .color(Color::Muted),
        )
}

fn render_font_picker(
    field: SettingField<settings::FontFamilyName>,
    file: SettingsUiFile,
    _metadata: Option<&SettingsFieldMetadata>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let current_value = SettingsStore::global(cx)
        .get_value_from_file(file.to_settings(), field.pick)
        .1
        .cloned()
        .map_or_else(|| SharedString::default(), |value| value.into_gpui());

    PopoverMenu::new("font-picker")
        .trigger(render_picker_trigger_button(
            "font_family_picker_trigger".into(),
            current_value.clone(),
        ))
        .menu(move |window, cx| {
            let file = file.clone();
            let current_value = current_value.clone();

            Some(cx.new(move |cx| {
                font_picker(
                    current_value,
                    move |font_name, window, cx| {
                        update_settings_file(
                            file.clone(),
                            field.json_path,
                            window,
                            cx,
                            move |settings, app| {
                                (field.write)(settings, Some(font_name.to_string().into()), app);
                            },
                        )
                        .log_err(); // todo(settings_ui) don't log err
                    },
                    window,
                    cx,
                )
            }))
        })
        .anchor(gpui::Anchor::TopLeft)
        .offset(gpui::Point {
            x: px(0.0),
            y: px(2.0),
        })
        .with_handle(ui::PopoverMenuHandle::default())
        .into_any_element()
}

fn render_theme_picker(
    field: SettingField<settings::ThemeName>,
    file: SettingsUiFile,
    _metadata: Option<&SettingsFieldMetadata>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let (_, value) = SettingsStore::global(cx).get_value_from_file(file.to_settings(), field.pick);
    let current_value = value
        .cloned()
        .map(|theme_name| theme_name.0.into())
        .unwrap_or_else(|| cx.theme().name.clone());

    PopoverMenu::new("theme-picker")
        .trigger(render_picker_trigger_button(
            "theme_picker_trigger".into(),
            current_value.clone(),
        ))
        .menu(move |window, cx| {
            Some(cx.new(|cx| {
                let file = file.clone();
                let current_value = current_value.clone();
                theme_picker(
                    current_value,
                    move |theme_name, window, cx| {
                        update_settings_file(
                            file.clone(),
                            field.json_path,
                            window,
                            cx,
                            move |settings, app| {
                                (field.write)(
                                    settings,
                                    Some(settings::ThemeName(theme_name.into())),
                                    app,
                                );
                            },
                        )
                        .log_err(); // todo(settings_ui) don't log err
                    },
                    window,
                    cx,
                )
            }))
        })
        .anchor(gpui::Anchor::TopLeft)
        .offset(gpui::Point {
            x: px(0.0),
            y: px(2.0),
        })
        .with_handle(ui::PopoverMenuHandle::default())
        .into_any_element()
}

fn render_icon_theme_picker(
    field: SettingField<settings::IconThemeName>,
    file: SettingsUiFile,
    _metadata: Option<&SettingsFieldMetadata>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let (_, value) = SettingsStore::global(cx).get_value_from_file(file.to_settings(), field.pick);
    let current_value = value
        .cloned()
        .map(|theme_name| theme_name.0.into())
        .unwrap_or_else(|| cx.theme().name.clone());

    PopoverMenu::new("icon-theme-picker")
        .trigger(render_picker_trigger_button(
            "icon_theme_picker_trigger".into(),
            current_value.clone(),
        ))
        .menu(move |window, cx| {
            Some(cx.new(|cx| {
                let file = file.clone();
                let current_value = current_value.clone();
                icon_theme_picker(
                    current_value,
                    move |theme_name, window, cx| {
                        update_settings_file(
                            file.clone(),
                            field.json_path,
                            window,
                            cx,
                            move |settings, app| {
                                (field.write)(
                                    settings,
                                    Some(settings::IconThemeName(theme_name.into())),
                                    app,
                                );
                            },
                        )
                        .log_err(); // todo(settings_ui) don't log err
                    },
                    window,
                    cx,
                )
            }))
        })
        .anchor(gpui::Anchor::TopLeft)
        .offset(gpui::Point {
            x: px(0.0),
            y: px(2.0),
        })
        .with_handle(ui::PopoverMenuHandle::default())
        .into_any_element()
}

#[cfg(test)]
pub mod test {

    use super::*;

    impl SettingsWindow {
        fn navbar_entry(&self) -> usize {
            self.navbar_entry
        }

        #[cfg(any(test, feature = "test-support"))]
        pub fn test(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let search_bar = cx.new(|cx| Editor::single_line(window, cx));
            let dummy_page = SettingsPage {
                title: "Test",
                items: Box::new([]),
            };
            Self {
                title_bar: None,
                original_window: None,
                worktree_root_dirs: HashMap::default(),
                files: Vec::default(),
                current_file: SettingsUiFile::User,
                project_setting_file_buffers: HashMap::default(),
                pages: vec![dummy_page],
                search_bar,
                navbar_entry: 0,
                navbar_entries: Vec::default(),
                navbar_scroll_handle: UniformListScrollHandle::default(),
                navbar_focus_subscriptions: Vec::default(),
                filter_table: Vec::default(),
                has_query: false,
                content_handles: Vec::default(),
                search_task: None,
                sub_page_stack: Vec::default(),
                opening_link: false,
                focus_handle: cx.focus_handle(),
                navbar_focus_handle: NonFocusableHandle::new(
                    NAVBAR_CONTAINER_TAB_INDEX,
                    false,
                    window,
                    cx,
                ),
                content_focus_handle: NonFocusableHandle::new(
                    CONTENT_CONTAINER_TAB_INDEX,
                    false,
                    window,
                    cx,
                ),
                files_focus_handle: cx.focus_handle(),
                search_index: None,
                list_state: ListState::new(0, gpui::ListAlignment::Top, px(0.0)),
                regex_validation_error: None,
                last_copied_link_path: None,
            }
        }
    }

    impl PartialEq for NavBarEntry {
        fn eq(&self, other: &Self) -> bool {
            self.title == other.title
                && self.is_root == other.is_root
                && self.expanded == other.expanded
                && self.page_index == other.page_index
                && self.item_index == other.item_index
            // ignoring focus_handle
        }
    }

    pub fn register_settings(cx: &mut App) {
        localization::init(localization::UiLanguage::English, cx)
            .expect("test localization must load");
        settings::init(cx);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        editor::init(cx);
        menu::init();
    }

    fn parse(input: &'static str, window: &mut Window, cx: &mut App) -> SettingsWindow {
        struct PageBuilder {
            title: &'static str,
            items: Vec<SettingsPageItem>,
        }
        let mut page_builders: Vec<PageBuilder> = Vec::new();
        let mut expanded_pages = Vec::new();
        let mut selected_idx = None;
        let mut index = 0;
        let mut in_expanded_section = false;

        for mut line in input
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
        {
            if let Some(pre) = line.strip_suffix('*') {
                assert!(selected_idx.is_none(), "Only one selected entry allowed");
                selected_idx = Some(index);
                line = pre;
            }
            let (kind, title) = line.split_once(" ").unwrap();
            assert_eq!(kind.len(), 1);
            let kind = kind.chars().next().unwrap();
            if kind == 'v' {
                let page_idx = page_builders.len();
                expanded_pages.push(page_idx);
                page_builders.push(PageBuilder {
                    title,
                    items: vec![],
                });
                index += 1;
                in_expanded_section = true;
            } else if kind == '>' {
                page_builders.push(PageBuilder {
                    title,
                    items: vec![],
                });
                index += 1;
                in_expanded_section = false;
            } else if kind == '-' {
                page_builders
                    .last_mut()
                    .unwrap()
                    .items
                    .push(SettingsPageItem::SectionHeader(title));
                if selected_idx == Some(index) && !in_expanded_section {
                    panic!("Items in unexpanded sections cannot be selected");
                }
                index += 1;
            } else {
                panic!(
                    "Entries must start with one of 'v', '>', or '-'\n line: {}",
                    line
                );
            }
        }

        let pages: Vec<SettingsPage> = page_builders
            .into_iter()
            .map(|builder| SettingsPage {
                title: builder.title,
                items: builder.items.into_boxed_slice(),
            })
            .collect();

        let mut settings_window = SettingsWindow {
            title_bar: None,
            original_window: None,
            worktree_root_dirs: HashMap::default(),
            files: Vec::default(),
            current_file: crate::SettingsUiFile::User,
            project_setting_file_buffers: HashMap::default(),
            pages,
            search_bar: cx.new(|cx| Editor::single_line(window, cx)),
            navbar_entry: selected_idx.expect("Must have a selected navbar entry"),
            navbar_entries: Vec::default(),
            navbar_scroll_handle: UniformListScrollHandle::default(),
            navbar_focus_subscriptions: vec![],
            filter_table: vec![],
            sub_page_stack: vec![],
            opening_link: false,
            has_query: false,
            content_handles: vec![],
            search_task: None,
            focus_handle: cx.focus_handle(),
            navbar_focus_handle: NonFocusableHandle::new(
                NAVBAR_CONTAINER_TAB_INDEX,
                false,
                window,
                cx,
            ),
            content_focus_handle: NonFocusableHandle::new(
                CONTENT_CONTAINER_TAB_INDEX,
                false,
                window,
                cx,
            ),
            files_focus_handle: cx.focus_handle(),
            search_index: None,
            list_state: ListState::new(0, gpui::ListAlignment::Top, px(0.0)),
            regex_validation_error: None,
            last_copied_link_path: None,
        };

        settings_window.build_filter_table();
        settings_window.build_navbar(cx);
        for expanded_page_index in expanded_pages {
            for entry in &mut settings_window.navbar_entries {
                if entry.page_index == expanded_page_index && entry.is_root {
                    entry.expanded = true;
                }
            }
        }
        settings_window
    }

    #[track_caller]
    fn check_navbar_toggle(
        before: &'static str,
        toggle_page: &'static str,
        after: &'static str,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut settings_window = parse(before, window, cx);
        let toggle_page_idx = settings_window
            .pages
            .iter()
            .position(|page| page.title == toggle_page)
            .expect("page not found");
        let toggle_idx = settings_window
            .navbar_entries
            .iter()
            .position(|entry| entry.page_index == toggle_page_idx)
            .expect("page not found");
        settings_window.toggle_navbar_entry(toggle_idx);

        let expected_settings_window = parse(after, window, cx);

        pretty_assertions::assert_eq!(
            settings_window
                .visible_navbar_entries()
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>(),
            expected_settings_window
                .visible_navbar_entries()
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>(),
        );
        pretty_assertions::assert_eq!(
            settings_window.navbar_entries[settings_window.navbar_entry()],
            expected_settings_window.navbar_entries[expected_settings_window.navbar_entry()],
        );
    }

    macro_rules! check_navbar_toggle {
        ($name:ident, before: $before:expr, toggle_page: $toggle_page:expr, after: $after:expr) => {
            #[gpui::test]
            fn $name(cx: &mut gpui::TestAppContext) {
                let window = cx.add_empty_window();
                window.update(|window, cx| {
                    register_settings(cx);
                    check_navbar_toggle($before, $toggle_page, $after, window, cx);
                });
            }
        };
    }

    check_navbar_toggle!(
        navbar_basic_open,
        before: r"
        v General
        - General
        - Privacy*
        v Project
        - Project Settings
        ",
        toggle_page: "General",
        after: r"
        > General*
        v Project
        - Project Settings
        "
    );

    check_navbar_toggle!(
        navbar_basic_close,
        before: r"
        > General*
        - General
        - Privacy
        v Project
        - Project Settings
        ",
        toggle_page: "General",
        after: r"
        v General*
        - General
        - Privacy
        v Project
        - Project Settings
        "
    );

    check_navbar_toggle!(
        navbar_basic_second_root_entry_close,
        before: r"
        > General
        - General
        - Privacy
        v Project
        - Project Settings*
        ",
        toggle_page: "Project",
        after: r"
        > General
        > Project*
        "
    );

    check_navbar_toggle!(
        navbar_toggle_subroot,
        before: r"
        v General Page
        - General
        - Privacy
        v Project
        - Worktree Settings Content*
        v AI
        - General
        > Appearance & Behavior
        ",
        toggle_page: "Project",
        after: r"
        v General Page
        - General
        - Privacy
        > Project*
        v AI
        - General
        > Appearance & Behavior
        "
    );

    check_navbar_toggle!(
        navbar_toggle_close_propagates_selected_index,
        before: r"
        v General Page
        - General
        - Privacy
        v Project
        - Worktree Settings Content
        v AI
        - General*
        > Appearance & Behavior
        ",
        toggle_page: "General Page",
        after: r"
        > General Page*
        v Project
        - Worktree Settings Content
        v AI
        - General
        > Appearance & Behavior
        "
    );

    check_navbar_toggle!(
        navbar_toggle_expand_propagates_selected_index,
        before: r"
        > General Page
        - General
        - Privacy
        v Project
        - Worktree Settings Content
        v AI
        - General*
        > Appearance & Behavior
        ",
        toggle_page: "General Page",
        after: r"
        v General Page*
        - General
        - Privacy
        v Project
        - Worktree Settings Content
        v AI
        - General
        > Appearance & Behavior
        "
    );

    #[gpui::test]
    fn navbar_double_click_toggle(cx: &mut gpui::TestAppContext) {
        let (settings_window, cx) = cx.add_window_view(|window, cx| {
            register_settings(cx);
            let mut settings_window = parse(
                r"
                > General*
                - General
                - Privacy
                v Project
                - Project Settings
                ",
                window,
                cx,
            );
            settings_window.build_content_handles(window, cx);
            settings_window
        });

        settings_window.update_in(cx, |settings_window, window, cx| {
            let general_idx = settings_window
                .navbar_entries
                .iter()
                .position(|entry| entry.title == "General" && entry.is_root)
                .expect("General root entry should exist");
            let privacy_idx = settings_window
                .navbar_entries
                .iter()
                .position(|entry| entry.title == "Privacy" && !entry.is_root)
                .expect("Privacy nested entry should exist");

            let click_event = |click_count| {
                gpui::ClickEvent::Mouse(gpui::MouseClickEvent {
                    down: gpui::MouseDownEvent {
                        button: gpui::MouseButton::Left,
                        click_count,
                        ..Default::default()
                    },
                    up: gpui::MouseUpEvent {
                        button: gpui::MouseButton::Left,
                        click_count,
                        ..Default::default()
                    },
                })
            };

            assert!(
                !settings_window.toggle_navbar_entry_on_double_click(
                    general_idx,
                    &click_event(1),
                    window,
                    cx,
                ),
                "single-clicks should use the normal navigation path"
            );
            assert!(!settings_window.navbar_entries[general_idx].expanded);

            assert!(settings_window.toggle_navbar_entry_on_double_click(
                general_idx,
                &click_event(2),
                window,
                cx,
            ));
            assert!(settings_window.navbar_entries[general_idx].expanded);

            assert!(
                !settings_window.toggle_navbar_entry_on_double_click(
                    general_idx,
                    &click_event(3),
                    window,
                    cx,
                ),
                "triple-clicks should not toggle the entry again"
            );
            assert!(settings_window.navbar_entries[general_idx].expanded);

            assert!(!settings_window.toggle_navbar_entry_on_double_click(
                privacy_idx,
                &click_event(2),
                window,
                cx,
            ));
        });
    }

    #[gpui::test]
    async fn test_settings_window_shows_worktrees_from_multiple_workspaces(
        cx: &mut gpui::TestAppContext,
    ) {
        use project::Project;
        use serde_json::json;

        cx.update(|cx| {
            register_settings(cx);
        });

        let app_state = cx.update(|cx| {
            let app_state = AppState::test(cx);
            AppState::set_global(app_state.clone(), cx);
            app_state
        });

        let fake_fs = app_state.fs.as_fake();

        fake_fs
            .insert_tree(
                "/workspace1",
                json!({
                    "worktree_a": {
                        "file1.rs": "fn main() {}"
                    },
                    "worktree_b": {
                        "file2.rs": "fn test() {}"
                    }
                }),
            )
            .await;

        fake_fs
            .insert_tree(
                "/workspace2",
                json!({
                    "worktree_c": {
                        "file3.rs": "fn foo() {}"
                    }
                }),
            )
            .await;

        let project1 = cx.update(|cx| {
            Project::local(
                app_state.http_client.clone(),
                app_state.node_runtime.clone(),
                app_state.languages.clone(),
                app_state.fs.clone(),
                None,
                project::LocalProjectFlags::default(),
                cx,
            )
        });

        project1
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/workspace1/worktree_a", true, cx)
            })
            .await
            .expect("Failed to create worktree_a");
        project1
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/workspace1/worktree_b", true, cx)
            })
            .await
            .expect("Failed to create worktree_b");

        let project2 = cx.update(|cx| {
            Project::local(
                app_state.http_client.clone(),
                app_state.node_runtime.clone(),
                app_state.languages.clone(),
                app_state.fs.clone(),
                None,
                project::LocalProjectFlags::default(),
                cx,
            )
        });

        project2
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/workspace2/worktree_c", true, cx)
            })
            .await
            .expect("Failed to create worktree_c");

        let (_multi_workspace1, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| {
                Workspace::new(
                    Default::default(),
                    project1.clone(),
                    app_state.clone(),
                    window,
                    cx,
                )
            });
            MultiWorkspace::new(workspace, window, cx)
        });

        let (_multi_workspace2, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| {
                Workspace::new(
                    Default::default(),
                    project2.clone(),
                    app_state.clone(),
                    window,
                    cx,
                )
            });
            MultiWorkspace::new(workspace, window, cx)
        });

        let workspace2_handle = cx.window_handle().downcast::<MultiWorkspace>().unwrap();

        cx.run_until_parked();

        let (settings_window, cx) = cx
            .add_window_view(|window, cx| SettingsWindow::new(Some(workspace2_handle), window, cx));

        cx.run_until_parked();

        settings_window.read_with(cx, |settings_window, _| {
            let worktree_names: Vec<_> = settings_window
                .worktree_root_dirs
                .values()
                .cloned()
                .collect();

            assert!(
                worktree_names.iter().any(|name| name == "worktree_a"),
                "Should contain worktree_a from workspace1, but found: {:?}",
                worktree_names
            );
            assert!(
                worktree_names.iter().any(|name| name == "worktree_b"),
                "Should contain worktree_b from workspace1, but found: {:?}",
                worktree_names
            );
            assert!(
                worktree_names.iter().any(|name| name == "worktree_c"),
                "Should contain worktree_c from workspace2, but found: {:?}",
                worktree_names
            );

            assert_eq!(
                worktree_names.len(),
                3,
                "Should have exactly 3 worktrees from both workspaces, but found: {:?}",
                worktree_names
            );

            let project_files: Vec<_> = settings_window
                .files
                .iter()
                .filter_map(|(f, _)| match f {
                    SettingsUiFile::Project((worktree_id, _)) => Some(*worktree_id),
                    _ => None,
                })
                .collect();

            let unique_project_files: std::collections::HashSet<_> = project_files.iter().collect();
            assert_eq!(
                project_files.len(),
                unique_project_files.len(),
                "Should have no duplicate project files, but found duplicates. All files: {:?}",
                project_files
            );
        });
    }

    #[gpui::test]
    async fn test_settings_window_updates_when_new_workspace_created(
        cx: &mut gpui::TestAppContext,
    ) {
        use project::Project;
        use serde_json::json;

        cx.update(|cx| {
            register_settings(cx);
        });

        let app_state = cx.update(|cx| {
            let app_state = AppState::test(cx);
            AppState::set_global(app_state.clone(), cx);
            app_state
        });

        let fake_fs = app_state.fs.as_fake();

        fake_fs
            .insert_tree(
                "/workspace1",
                json!({
                    "worktree_a": {
                        "file1.rs": "fn main() {}"
                    }
                }),
            )
            .await;

        fake_fs
            .insert_tree(
                "/workspace2",
                json!({
                    "worktree_b": {
                        "file2.rs": "fn test() {}"
                    }
                }),
            )
            .await;

        let project1 = cx.update(|cx| {
            Project::local(
                app_state.http_client.clone(),
                app_state.node_runtime.clone(),
                app_state.languages.clone(),
                app_state.fs.clone(),
                None,
                project::LocalProjectFlags::default(),
                cx,
            )
        });

        project1
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/workspace1/worktree_a", true, cx)
            })
            .await
            .expect("Failed to create worktree_a");

        let (_multi_workspace1, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| {
                Workspace::new(
                    Default::default(),
                    project1.clone(),
                    app_state.clone(),
                    window,
                    cx,
                )
            });
            MultiWorkspace::new(workspace, window, cx)
        });

        let workspace1_handle = cx.window_handle().downcast::<MultiWorkspace>().unwrap();

        cx.run_until_parked();

        let (settings_window, cx) = cx
            .add_window_view(|window, cx| SettingsWindow::new(Some(workspace1_handle), window, cx));

        cx.run_until_parked();

        settings_window.read_with(cx, |settings_window, _| {
            assert_eq!(
                settings_window.worktree_root_dirs.len(),
                1,
                "Should have 1 worktree initially"
            );
        });

        let project2 = cx.update(|_, cx| {
            Project::local(
                app_state.http_client.clone(),
                app_state.node_runtime.clone(),
                app_state.languages.clone(),
                app_state.fs.clone(),
                None,
                project::LocalProjectFlags::default(),
                cx,
            )
        });

        project2
            .update(&mut cx.cx, |project, cx| {
                project.find_or_create_worktree("/workspace2/worktree_b", true, cx)
            })
            .await
            .expect("Failed to create worktree_b");

        let (_multi_workspace2, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| {
                Workspace::new(
                    Default::default(),
                    project2.clone(),
                    app_state.clone(),
                    window,
                    cx,
                )
            });
            MultiWorkspace::new(workspace, window, cx)
        });

        cx.run_until_parked();

        settings_window.read_with(cx, |settings_window, _| {
            let worktree_names: Vec<_> = settings_window
                .worktree_root_dirs
                .values()
                .cloned()
                .collect();

            assert!(
                worktree_names.iter().any(|name| name == "worktree_a"),
                "Should contain worktree_a, but found: {:?}",
                worktree_names
            );
            assert!(
                worktree_names.iter().any(|name| name == "worktree_b"),
                "Should contain worktree_b from newly created workspace, but found: {:?}",
                worktree_names
            );

            assert_eq!(
                worktree_names.len(),
                2,
                "Should have 2 worktrees after new workspace created, but found: {:?}",
                worktree_names
            );

            let project_files: Vec<_> = settings_window
                .files
                .iter()
                .filter_map(|(f, _)| match f {
                    SettingsUiFile::Project((worktree_id, _)) => Some(*worktree_id),
                    _ => None,
                })
                .collect();

            let unique_project_files: std::collections::HashSet<_> = project_files.iter().collect();
            assert_eq!(
                project_files.len(),
                unique_project_files.len(),
                "Should have no duplicate project files, but found duplicates. All files: {:?}",
                project_files
            );
        });
    }

    #[gpui::test]
    fn settings_search_index_contains_english_and_chinese_terms(cx: &mut gpui::TestAppContext) {
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            register_settings(cx);
            localization::set_language(localization::UiLanguage::SimplifiedChinese, cx);
            let mut settings_window = parse("> General*", window, cx);
            settings_window.pages[0].items = vec![SettingsPageItem::UserLanguageSetting(
                user_language_setting(cx),
            )]
            .into_boxed_slice();
            settings_window.build_filter_table();
            settings_window.build_search_index(cx);

            let words = &settings_window
                .search_index
                .as_ref()
                .expect("settings search index should be built")
                .documents[0]
                .words;
            assert!(words.iter().any(|word| word == "language"));
            assert!(words.iter().any(|word| word == "语言"));
        });
    }
}

#[cfg(test)]
mod project_settings_update_tests {
    use super::*;
    use fs::{FakeFs, Fs as _};
    use gpui::TestAppContext;
    use project::Project;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestSetup {
        fs: Arc<FakeFs>,
        project: Entity<Project>,
        worktree_id: WorktreeId,
        worktree: WeakEntity<Worktree>,
        rel_path: Arc<RelPath>,
        project_path: ProjectPath,
    }

    async fn init_test(cx: &mut TestAppContext, initial_settings: Option<&str>) -> TestSetup {
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            localization::init(localization::UiLanguage::English, cx)
                .expect("test localization must load");
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            menu::init();
            let queue = ProjectSettingsUpdateQueue::new(cx);
            cx.set_global(queue);
        });

        let fs = FakeFs::new(cx.executor());
        let tree = if let Some(settings_content) = initial_settings {
            json!({
                ".flint": {
                    "settings.json": settings_content
                },
                "src": { "main.rs": "" }
            })
        } else {
            json!({ "src": { "main.rs": "" } })
        };
        fs.insert_tree("/project", tree).await;

        let project = Project::test(fs.clone(), ["/project".as_ref()], cx).await;

        let (worktree_id, worktree) = project.read_with(cx, |project, cx| {
            let worktree = project.worktrees(cx).next().unwrap();
            (worktree.read(cx).id(), worktree.downgrade())
        });

        let rel_path: Arc<RelPath> = RelPath::unix(".flint/settings.json")
            .expect("valid path")
            .into_arc();
        let project_path = ProjectPath {
            worktree_id,
            path: rel_path.clone(),
        };

        TestSetup {
            fs,
            project,
            worktree_id,
            worktree,
            rel_path,
            project_path,
        }
    }

    #[gpui::test]
    async fn test_creates_settings_file_if_missing(cx: &mut TestAppContext) {
        let setup = init_test(cx, None).await;

        let entry = ProjectSettingsUpdateEntry {
            worktree_id: setup.worktree_id,
            rel_path: setup.rel_path.clone(),
            settings_window: WeakEntity::new_invalid(),
            project: setup.project.downgrade(),
            worktree: setup.worktree,
            update: Box::new(|content, _cx| {
                content.project.all_languages.defaults.tab_size = Some(NonZeroU32::new(4).unwrap());
            }),
        };

        cx.update(|cx| ProjectSettingsUpdateQueue::enqueue(cx, entry));
        cx.executor().run_until_parked();

        let buffer_store = setup
            .project
            .read_with(cx, |project, _| project.buffer_store().clone());
        let buffer = buffer_store
            .update(cx, |store, cx| store.open_buffer(setup.project_path, cx))
            .await
            .expect("buffer should exist");

        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert!(
            text.contains("\"tab_size\": 4"),
            "Expected tab_size setting in: {}",
            text
        );
    }

    #[gpui::test]
    async fn test_updates_existing_settings_file(cx: &mut TestAppContext) {
        let setup = init_test(cx, Some(r#"{ "tab_size": 2 }"#)).await;

        let entry = ProjectSettingsUpdateEntry {
            worktree_id: setup.worktree_id,
            rel_path: setup.rel_path.clone(),
            settings_window: WeakEntity::new_invalid(),
            project: setup.project.downgrade(),
            worktree: setup.worktree,
            update: Box::new(|content, _cx| {
                content.project.all_languages.defaults.tab_size = Some(NonZeroU32::new(8).unwrap());
            }),
        };

        cx.update(|cx| ProjectSettingsUpdateQueue::enqueue(cx, entry));
        cx.executor().run_until_parked();

        let buffer_store = setup
            .project
            .read_with(cx, |project, _| project.buffer_store().clone());
        let buffer = buffer_store
            .update(cx, |store, cx| store.open_buffer(setup.project_path, cx))
            .await
            .expect("buffer should exist");

        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert!(
            text.contains("\"tab_size\": 8"),
            "Expected updated tab_size in: {}",
            text
        );
    }

    #[gpui::test]
    async fn test_updates_are_serialized(cx: &mut TestAppContext) {
        let setup = init_test(cx, Some("{}")).await;

        let update_order = Arc::new(std::sync::Mutex::new(Vec::new()));

        for i in 1..=3 {
            let update_order = update_order.clone();
            let entry = ProjectSettingsUpdateEntry {
                worktree_id: setup.worktree_id,
                rel_path: setup.rel_path.clone(),
                settings_window: WeakEntity::new_invalid(),
                project: setup.project.downgrade(),
                worktree: setup.worktree.clone(),
                update: Box::new(move |content, _cx| {
                    update_order.lock().unwrap().push(i);
                    content.project.all_languages.defaults.tab_size =
                        Some(NonZeroU32::new(i).unwrap());
                }),
            };
            cx.update(|cx| ProjectSettingsUpdateQueue::enqueue(cx, entry));
        }

        cx.executor().run_until_parked();

        let order = update_order.lock().unwrap().clone();
        assert_eq!(order, vec![1, 2, 3], "Updates should be processed in order");

        let buffer_store = setup
            .project
            .read_with(cx, |project, _| project.buffer_store().clone());
        let buffer = buffer_store
            .update(cx, |store, cx| store.open_buffer(setup.project_path, cx))
            .await
            .expect("buffer should exist");

        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert!(
            text.contains("\"tab_size\": 3"),
            "Final tab_size should be 3: {}",
            text
        );
    }

    #[gpui::test]
    async fn test_queue_continues_after_failure(cx: &mut TestAppContext) {
        let setup = init_test(cx, Some("{}")).await;

        let successful_updates = Arc::new(AtomicUsize::new(0));

        {
            let successful_updates = successful_updates.clone();
            let entry = ProjectSettingsUpdateEntry {
                worktree_id: setup.worktree_id,
                rel_path: setup.rel_path.clone(),
                settings_window: WeakEntity::new_invalid(),
                project: setup.project.downgrade(),
                worktree: setup.worktree.clone(),
                update: Box::new(move |content, _cx| {
                    successful_updates.fetch_add(1, Ordering::SeqCst);
                    content.project.all_languages.defaults.tab_size =
                        Some(NonZeroU32::new(2).unwrap());
                }),
            };
            cx.update(|cx| ProjectSettingsUpdateQueue::enqueue(cx, entry));
        }

        {
            let entry = ProjectSettingsUpdateEntry {
                worktree_id: setup.worktree_id,
                rel_path: setup.rel_path.clone(),
                settings_window: WeakEntity::new_invalid(),
                project: WeakEntity::new_invalid(),
                worktree: setup.worktree.clone(),
                update: Box::new(|content, _cx| {
                    content.project.all_languages.defaults.tab_size =
                        Some(NonZeroU32::new(99).unwrap());
                }),
            };
            cx.update(|cx| ProjectSettingsUpdateQueue::enqueue(cx, entry));
        }

        {
            let successful_updates = successful_updates.clone();
            let entry = ProjectSettingsUpdateEntry {
                worktree_id: setup.worktree_id,
                rel_path: setup.rel_path.clone(),
                settings_window: WeakEntity::new_invalid(),
                project: setup.project.downgrade(),
                worktree: setup.worktree.clone(),
                update: Box::new(move |content, _cx| {
                    successful_updates.fetch_add(1, Ordering::SeqCst);
                    content.project.all_languages.defaults.tab_size =
                        Some(NonZeroU32::new(4).unwrap());
                }),
            };
            cx.update(|cx| ProjectSettingsUpdateQueue::enqueue(cx, entry));
        }

        cx.executor().run_until_parked();

        assert_eq!(
            successful_updates.load(Ordering::SeqCst),
            2,
            "Two updates should have succeeded despite middle failure"
        );

        let buffer_store = setup
            .project
            .read_with(cx, |project, _| project.buffer_store().clone());
        let buffer = buffer_store
            .update(cx, |store, cx| store.open_buffer(setup.project_path, cx))
            .await
            .expect("buffer should exist");

        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert!(
            text.contains("\"tab_size\": 4"),
            "Final tab_size should be 4 (third update): {}",
            text
        );
    }

    #[gpui::test]
    async fn test_handles_dropped_worktree(cx: &mut TestAppContext) {
        let setup = init_test(cx, Some("{}")).await;

        let entry = ProjectSettingsUpdateEntry {
            worktree_id: setup.worktree_id,
            rel_path: setup.rel_path.clone(),
            settings_window: WeakEntity::new_invalid(),
            project: setup.project.downgrade(),
            worktree: WeakEntity::new_invalid(),
            update: Box::new(|content, _cx| {
                content.project.all_languages.defaults.tab_size =
                    Some(NonZeroU32::new(99).unwrap());
            }),
        };

        cx.update(|cx| ProjectSettingsUpdateQueue::enqueue(cx, entry));
        cx.executor().run_until_parked();

        let file_content = setup
            .fs
            .load("/project/.flint/settings.json".as_ref())
            .await
            .unwrap();
        assert_eq!(
            file_content, "{}",
            "File should be unchanged when worktree is dropped"
        );
    }

    #[gpui::test]
    async fn test_reloads_conflicted_buffer(cx: &mut TestAppContext) {
        let setup = init_test(cx, Some(r#"{ "tab_size": 2 }"#)).await;

        let buffer_store = setup
            .project
            .read_with(cx, |project, _| project.buffer_store().clone());
        let buffer = buffer_store
            .update(cx, |store, cx| {
                store.open_buffer(setup.project_path.clone(), cx)
            })
            .await
            .expect("buffer should exist");

        buffer.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "// comment\n")], None, cx);
        });

        let has_unsaved_edits = buffer.read_with(cx, |buffer, _| buffer.has_unsaved_edits());
        assert!(has_unsaved_edits, "Buffer should have unsaved edits");

        setup
            .fs
            .save(
                "/project/.flint/settings.json".as_ref(),
                &r#"{ "tab_size": 99 }"#.into(),
                Default::default(),
            )
            .await
            .expect("save should succeed");

        cx.executor().run_until_parked();

        let has_conflict = buffer.read_with(cx, |buffer, _| buffer.has_conflict());
        assert!(
            has_conflict,
            "Buffer should have conflict after external modification"
        );

        let (settings_window, _) = cx.add_window_view(|window, cx| {
            let mut sw = SettingsWindow::test(window, cx);
            sw.project_setting_file_buffers
                .insert(setup.project_path.clone(), buffer.clone());
            sw
        });

        let entry = ProjectSettingsUpdateEntry {
            worktree_id: setup.worktree_id,
            rel_path: setup.rel_path.clone(),
            settings_window: settings_window.downgrade(),
            project: setup.project.downgrade(),
            worktree: setup.worktree.clone(),
            update: Box::new(|content, _cx| {
                content.project.all_languages.defaults.tab_size = Some(NonZeroU32::new(4).unwrap());
            }),
        };

        cx.update(|cx| ProjectSettingsUpdateQueue::enqueue(cx, entry));
        cx.executor().run_until_parked();

        let text = buffer.read_with(cx, |buffer, _| buffer.text());
        assert!(
            text.contains("\"tab_size\": 4"),
            "Buffer should have the new tab_size after reload and update: {}",
            text
        );
        assert!(
            !text.contains("// comment"),
            "Buffer should not contain the unsaved edit after reload: {}",
            text
        );
        assert!(
            !text.contains("99"),
            "Buffer should not contain the external modification value: {}",
            text
        );
    }
}
