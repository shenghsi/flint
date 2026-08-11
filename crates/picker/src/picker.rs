mod head;
pub mod highlighted_match_with_paths;
mod persistence;
pub mod popover_menu;
mod preview;

use preview::Preview;
pub use preview::{Layout as PreviewLayout, Update as PreviewUpdate};

use anyhow::Result;

use flint_actions::editor::{MoveDown, MoveUp};
use gpui::{
    Action, AnyElement, App, Bounds, ClickEvent, Context, CursorStyle, DismissEvent, DragMoveEvent,
    Entity, EventEmitter, FocusHandle, Focusable, Length, ListSizingBehavior, ListState,
    MouseButton, MouseUpEvent, Pixels, Render, ScrollStrategy, Task, UniformListScrollHandle,
    WeakEntity, Window, actions, canvas, div, list, prelude::*, uniform_list,
};
use gpui_util::ResultExt;
use head::Head;
use project::Project;
use schemars::JsonSchema;
use serde::Deserialize;
use std::{
    cell::Cell, cell::RefCell, collections::HashMap, ops::Range, rc::Rc, sync::Arc, time::Duration,
};
use theme_settings::ThemeSettings;
use ui::{
    Button, Color, Divider, DocumentationAside, DocumentationSide, IconButton, IconName, Label,
    ListItem, ListItemSpacing, ScrollAxes, Scrollbars, Tooltip, WithScrollbar, prelude::*,
    utils::WithRemSize, v_flex,
};
use ui_input::{ErasedEditor, ErasedEditorEvent};
use workspace::{ModalView, item::Settings};

enum ElementContainer {
    List(ListState),
    UniformList(UniformListScrollHandle),
}

#[derive(Clone, Copy)]
enum DividerDrag {
    Right {
        start_x: Pixels,
        start_width: Pixels,
    },
    Below {
        start_y: Pixels,
        start_height: Pixels,
    },
}

struct DividerDragView;

impl Render for DividerDragView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

const MIN_PREVIEW_PX: Pixels = px(240.);

pub enum Direction {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollBehavior {
    RevealSelected,
    PreserveOffset,
}

actions!(
    picker,
    [
        /// Confirms the selected completion in the picker.
        ConfirmCompletion,
        /// Toggles multi-select mode for pickers that support it.
        ToggleMultiSelect,
        /// Toggles the focused item and advances to the next selectable item.
        MultiSelectNext,
        /// Toggles the preview between hidden and visible.
        TogglePreview,
        /// Shows the preview to the right of the results.
        SetPreviewRight,
        /// Shows the preview below the results.
        SetPreviewBelow,
        /// Hides the preview.
        SetPreviewHidden
    ]
);

/// ConfirmInput is an alternative editor action which - instead of selecting active picker entry - treats pickers editor input literally,
/// performing some kind of action on it.
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Default, Action)]
#[action(namespace = picker)]
#[serde(deny_unknown_fields)]
pub struct ConfirmInput {
    pub secondary: bool,
}

struct PendingUpdateMatches {
    delegate_update_matches: Option<Task<()>>,
    _task: Task<Result<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PickerItemId(SharedString);

impl PickerItemId {
    pub fn new(value: impl Into<SharedString>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }

    pub fn into_shared_string(self) -> SharedString {
        self.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickerRestorationState {
    pub query: String,
    pub multi_select_enabled: bool,
    pub selected_item_ids: Vec<PickerItemId>,
}

pub struct Picker<D: PickerDelegate> {
    pub delegate: D,
    element_container: ElementContainer,
    head: Head,
    pending_update_matches: Option<PendingUpdateMatches>,
    confirm_on_update: Option<bool>,
    width: Option<Length>,
    widest_item: Option<usize>,
    max_height: Option<Length>,
    /// An external control to display a scrollbar in the `Picker`.
    show_scrollbar: bool,
    /// Whether the `Picker` is rendered as a self-contained modal.
    ///
    /// Set this to `false` when rendering the `Picker` as part of a larger modal.
    is_modal: bool,
    /// Bounds tracking for the picker container (for aside positioning)
    picker_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Bounds tracking for items (for aside positioning) - maps item index to bounds
    item_bounds: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    preview: Option<Preview>,
    preview_size: Option<Pixels>,
    multi_select_enabled: bool,
    selected_item_ids: Vec<PickerItemId>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum PickerEditorPosition {
    #[default]
    /// Render the editor at the start of the picker. Usually the top
    Start,
    /// Render the editor at the end of the picker. Usually the bottom
    End,
}

pub trait PickerDelegate: Sized + 'static {
    type ListItem: IntoElement;

    fn match_count(&self) -> usize;
    fn selected_index(&self) -> usize;
    fn separators_after_indices(&self) -> Vec<usize> {
        Vec::new()
    }
    fn set_selected_index(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    );

    /// Called before the picker handles `SelectPrevious` or `SelectNext`. Return `Some(query)` to
    /// set a new query and prevent the default selection behavior.
    fn select_history(
        &mut self,
        _direction: Direction,
        _query: &str,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        None
    }
    fn can_select(
        &self,
        _ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> bool {
        true
    }
    fn select_on_hover(&self) -> bool {
        true
    }

    fn supports_multi_select(&self) -> bool {
        false
    }

    fn item_id(&self, _index: usize) -> Option<PickerItemId> {
        None
    }

    fn item_id_is_valid(&self, _id: &PickerItemId, _cx: &App) -> bool {
        false
    }

    fn confirm_multi(
        &mut self,
        _ids: Vec<PickerItemId>,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
    }

    fn workspace(&self, _cx: &App) -> Option<WeakEntity<workspace::Workspace>> {
        None
    }

    fn reopen_request(
        &self,
        _state: &PickerRestorationState,
        _cx: &App,
    ) -> Option<Arc<dyn workspace::ReopenablePickerRequest>> {
        None
    }

    // Allows binding some optional effect to when the selection changes.
    fn selected_index_changed(
        &self,
        _ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Box<dyn Fn(&mut Window, &mut App) + 'static>> {
        None
    }
    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str>;
    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some(localization::text(_cx, "picker-no-matches"))
    }
    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()>;

    // Delegates that support this method (e.g. the CommandPalette) can chose to block on any background
    // work for up to `duration` to try and get a result synchronously.
    // This avoids a flash of an empty command-palette on cmd-shift-p, and lets workspace::SendKeystrokes
    // mostly work when dismissing a palette.
    fn finalize_update_matches(
        &mut self,
        _query: String,
        _duration: Duration,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> bool {
        false
    }

    /// Override if you want to have <enter> update the query instead of confirming.
    fn confirm_update_query(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<String> {
        None
    }
    fn confirm(&mut self, secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>);
    /// Instead of interacting with currently selected entry, treats editor input literally,
    /// performing some kind of action on it.
    fn confirm_input(
        &mut self,
        _secondary: bool,
        _window: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) {
    }
    fn dismissed(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>);
    fn should_dismiss(&self) -> bool {
        true
    }
    fn confirm_completion(
        &mut self,
        _query: String,
        _window: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<String> {
        None
    }

    fn editor_position(&self) -> PickerEditorPosition {
        PickerEditorPosition::default()
    }

    fn render_editor(
        &self,
        editor: &Arc<dyn ErasedEditor>,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Div {
        v_flex()
            .when(
                self.editor_position() == PickerEditorPosition::End,
                |this| this.child(Divider::horizontal()),
            )
            .child(
                h_flex()
                    .overflow_hidden()
                    .flex_none()
                    .h_9()
                    .px_2p5()
                    .child(editor.render(window, cx)),
            )
            .when(
                self.editor_position() == PickerEditorPosition::Start,
                |this| this.child(Divider::horizontal()),
            )
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem>;

    fn render_match_with_state(
        &self,
        ix: usize,
        selected: bool,
        _multi_selected: bool,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        self.render_match(ix, selected, window, cx)
    }

    fn render_header(
        &self,
        _window: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<AnyElement> {
        None
    }

    fn render_footer(
        &self,
        _window: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<AnyElement> {
        None
    }

    fn documentation_aside(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<DocumentationAside> {
        None
    }

    /// Returns the index of the item whose documentation aside should be shown.
    /// This is used to position the aside relative to that item.
    /// Typically this is the hovered item, not necessarily the selected item.
    fn documentation_aside_index(&self) -> Option<usize> {
        None
    }

    /// A stable, human-readable identifier for this picker, used as the
    /// persistence key for the preview layout. Delegates that support a preview
    /// should override this so each picker remembers its own layout.
    fn name() -> &'static str {
        "picker"
    }

    /// Returns the data the preview should show for the currently selected
    /// match, or `None` if there is nothing to preview. Called whenever the
    /// selection or the matches change.
    fn try_get_preview_data_for_match(&self, _cx: &App) -> Option<PreviewUpdate> {
        None
    }

    /// Called after the preview layout changes so delegates can react (a
    /// horizontal preview may want different sizing than a vertical one).
    fn preview_layout_changed(&mut self, _layout_is_horizontal: bool) {}
}

impl<D: PickerDelegate> Focusable for Picker<D> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.head {
            Head::Editor(editor) => editor.focus_handle(cx),
            Head::Empty(head) => head.focus_handle(cx),
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
enum ContainerKind {
    List,
    UniformList,
}

impl<D: PickerDelegate> Picker<D> {
    /// A picker, which displays its matches using `gpui::uniform_list`, all matches should have the same height.
    /// The picker allows the user to perform search items by text.
    /// If `PickerDelegate::render_match` can return items with different heights, use `Picker::list`.
    pub fn uniform_list(delegate: D, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let head = Head::editor(
            delegate.placeholder_text(window, cx),
            Self::on_input_editor_event,
            window,
            cx,
        );

        Self::new(delegate, ContainerKind::UniformList, head, None, window, cx)
    }

    /// A picker similar to [`uniform_list()`](Self::uniform_list) however this picker has a
    /// preview window where it shows extra information about the selected match.
    pub fn uniform_list_with_preview(
        delegate: D,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let head = Head::editor(
            delegate.placeholder_text(window, cx),
            Self::on_input_editor_event,
            window,
            cx,
        );

        let preview = Preview::new_editor(project, window, cx);
        Self::new(
            delegate,
            ContainerKind::UniformList,
            head,
            Some(preview),
            window,
            cx,
        )
    }

    /// A picker, which displays its matches using `gpui::uniform_list`, all matches should have the same height.
    /// If `PickerDelegate::render_match` can return items with different heights, use `Picker::list`.
    pub fn nonsearchable_uniform_list(
        delegate: D,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let head = Head::empty(Self::on_empty_head_blur, window, cx);

        Self::new(delegate, ContainerKind::UniformList, head, None, window, cx)
    }

    /// A picker, which displays its matches using `gpui::list`, matches can have different heights.
    /// The picker allows the user to perform search items by text.
    /// If `PickerDelegate::render_match` only returns items with the same height, use `Picker::uniform_list` as its implementation is optimized for that.
    pub fn nonsearchable_list(delegate: D, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let head = Head::empty(Self::on_empty_head_blur, window, cx);

        Self::new(delegate, ContainerKind::List, head, None, window, cx)
    }

    /// A picker similar to [`list()`](Self::list) (variable-height rows) but with
    /// a preview window. Use this instead of [`uniform_list_with_preview()`](Self::uniform_list_with_preview)
    /// when [`PickerDelegate::render_match`] can return rows of different heights.
    pub fn list_with_preview(
        delegate: D,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let head = Head::editor(
            delegate.placeholder_text(window, cx),
            Self::on_input_editor_event,
            window,
            cx,
        );

        let preview = Preview::new_editor(project, window, cx);
        Self::new(
            delegate,
            ContainerKind::List,
            head,
            Some(preview),
            window,
            cx,
        )
    }

    /// A picker, which displays its matches using `gpui::list`, matches can have different heights.
    /// The picker allows the user to perform search items by text.
    /// If `PickerDelegate::render_match` only returns items with the same height, use `Picker::uniform_list` as its implementation is optimized for that.
    pub fn list(delegate: D, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let head = Head::editor(
            delegate.placeholder_text(window, cx),
            Self::on_input_editor_event,
            window,
            cx,
        );

        Self::new(delegate, ContainerKind::List, head, None, window, cx)
    }

    fn new(
        delegate: D,
        container: ContainerKind,
        head: Head,
        mut preview: Option<Preview>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let element_container = Self::create_element_container(container);
        if let Some(preview) = &mut preview {
            preview.layout = persistence::load_layout(D::name(), cx)
                .log_err()
                .flatten()
                .unwrap_or_default();
        };
        let mut this = Self {
            delegate,
            head,
            element_container,
            pending_update_matches: None,
            confirm_on_update: None,
            width: None,
            widest_item: None,
            max_height: Some(rems(24.).into()),
            show_scrollbar: false,
            is_modal: true,
            picker_bounds: Rc::new(Cell::new(None)),
            item_bounds: Rc::new(RefCell::new(HashMap::default())),
            preview,
            preview_size: None,
            multi_select_enabled: false,
            selected_item_ids: Vec::new(),
        };
        this.update_matches("".to_string(), window, cx);
        // give the delegate 4ms to render the first set of suggestions.
        this.delegate
            .finalize_update_matches("".to_string(), Duration::from_millis(4), window, cx);
        this
    }

    fn create_element_container(container: ContainerKind) -> ElementContainer {
        match container {
            ContainerKind::UniformList => {
                ElementContainer::UniformList(UniformListScrollHandle::new())
            }
            ContainerKind::List => {
                ElementContainer::List(ListState::new(0, gpui::ListAlignment::Top, px(1000.)))
            }
        }
    }

    pub fn width(mut self, width: impl Into<gpui::Length>) -> Self {
        self.width = Some(width.into());
        self
    }

    pub fn widest_item(mut self, ix: Option<usize>) -> Self {
        self.widest_item = ix;
        self
    }

    pub fn max_height(mut self, max_height: Option<gpui::Length>) -> Self {
        self.max_height = max_height;
        self
    }

    pub fn show_scrollbar(mut self, show_scrollbar: bool) -> Self {
        self.show_scrollbar = show_scrollbar;
        self
    }

    pub fn modal(mut self, modal: bool) -> Self {
        self.is_modal = modal;
        self
    }

    pub fn list_measure_all(mut self) -> Self {
        match self.element_container {
            ElementContainer::List(state) => {
                self.element_container = ElementContainer::List(state.measure_all());
            }
            _ => {}
        }
        self
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus_handle(cx).focus(window, cx);
    }

    /// Handles the selecting an index, and passing the change to the delegate.
    /// If `fallback_direction` is set to `None`, the index will not be selected
    /// if the element at that index cannot be selected.
    /// If `fallback_direction` is set to
    /// `Some(..)`, the next selectable element will be selected in the
    /// specified direction (Down or Up), cycling through all elements until
    /// finding one that can be selected or returning if there are no selectable elements.
    /// If `scroll_to_index` is true, the new selected index will be scrolled into
    /// view.
    ///
    /// If some effect is bound to `selected_index_changed`, it will be executed.
    pub fn set_selected_index(
        &mut self,
        mut ix: usize,
        fallback_direction: Option<Direction>,
        scroll_to_index: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delegate = &self.delegate;
        self.selected_item_ids
            .retain(|item_id| delegate.item_id_is_valid(item_id, cx));
        let match_count = self.delegate.match_count();
        if match_count == 0 {
            return;
        }

        if let Some(bias) = fallback_direction {
            let mut curr_ix = ix;
            while !self.delegate.can_select(curr_ix, window, cx) {
                curr_ix = match bias {
                    Direction::Down => {
                        if curr_ix == match_count - 1 {
                            0
                        } else {
                            curr_ix + 1
                        }
                    }
                    Direction::Up => {
                        if curr_ix == 0 {
                            match_count - 1
                        } else {
                            curr_ix - 1
                        }
                    }
                };
                // There is no item that can be selected
                if ix == curr_ix {
                    return;
                }
            }
            ix = curr_ix;
        } else if !self.delegate.can_select(ix, window, cx) {
            return;
        }

        let previous_index = self.delegate.selected_index();
        self.delegate.set_selected_index(ix, window, cx);
        let current_index = self.delegate.selected_index();

        if previous_index != current_index {
            if let Some(action) = self.delegate.selected_index_changed(ix, window, cx) {
                action(window, cx);
            }
            self.update_preview(window, cx);
            if scroll_to_index {
                self.scroll_to_item_index(ix);
            }
        }
    }

    pub fn select_next(
        &mut self,
        _: &menu::SelectNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = self.query(cx);
        if let Some(query) = self
            .delegate
            .select_history(Direction::Down, &query, window, cx)
        {
            self.set_query(&query, window, cx);
            return;
        }
        let count = self.delegate.match_count();
        if count > 0 {
            let index = self.delegate.selected_index();
            let ix = if index == count - 1 { 0 } else { index + 1 };
            self.set_selected_index(ix, Some(Direction::Down), true, window, cx);
            cx.notify();
        }
    }

    pub fn editor_move_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        self.select_previous(&Default::default(), window, cx);
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = self.query(cx);
        if let Some(query) = self
            .delegate
            .select_history(Direction::Up, &query, window, cx)
        {
            self.set_query(&query, window, cx);
            return;
        }
        let count = self.delegate.match_count();
        if count > 0 {
            let index = self.delegate.selected_index();
            let ix = if index == 0 { count - 1 } else { index - 1 };
            self.set_selected_index(ix, Some(Direction::Up), true, window, cx);
            cx.notify();
        }
    }

    pub fn editor_move_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        self.select_next(&Default::default(), window, cx);
    }

    pub fn select_first(
        &mut self,
        _: &menu::SelectFirst,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.delegate.match_count();
        if count > 0 {
            self.set_selected_index(0, Some(Direction::Down), true, window, cx);
            cx.notify();
        }
    }

    fn select_last(&mut self, _: &menu::SelectLast, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        if count > 0 {
            self.set_selected_index(count - 1, Some(Direction::Up), true, window, cx);
            cx.notify();
        }
    }

    pub fn cycle_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        let index = self.delegate.selected_index();
        let new_index = if index + 1 == count { 0 } else { index + 1 };
        self.set_selected_index(new_index, Some(Direction::Down), true, window, cx);
        cx.notify();
    }

    pub fn set_multi_select_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if !self.delegate.supports_multi_select() {
            return;
        }
        self.multi_select_enabled = enabled;
        if !enabled {
            self.selected_item_ids.clear();
        }
        cx.notify();
    }

    pub fn multi_select_enabled(&self) -> bool {
        self.multi_select_enabled
    }

    pub fn selected_item_ids(&self) -> &[PickerItemId] {
        &self.selected_item_ids
    }

    pub fn is_item_selected(&self, index: usize) -> bool {
        self.delegate
            .item_id(index)
            .is_some_and(|item_id| self.selected_item_ids.contains(&item_id))
    }

    pub fn toggle_item_selection(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.multi_select_enabled
            || !self.delegate.supports_multi_select()
            || !self.delegate.can_select(index, window, cx)
        {
            return;
        }
        let Some(item_id) = self.delegate.item_id(index) else {
            return;
        };
        if !self.delegate.item_id_is_valid(&item_id, cx) {
            return;
        }

        if let Some(selected_index) = self
            .selected_item_ids
            .iter()
            .position(|selected| selected == &item_id)
        {
            self.selected_item_ids.remove(selected_index);
        } else {
            self.selected_item_ids.push(item_id);
        }
        cx.notify();
    }

    pub fn reconcile_multi_selection(&mut self, cx: &mut Context<Self>) {
        self.selected_item_ids
            .retain(|item_id| self.delegate.item_id_is_valid(item_id, cx));
        cx.notify();
    }

    fn ordered_selected_item_ids(&self, cx: &App) -> Vec<PickerItemId> {
        let mut ordered = Vec::with_capacity(self.selected_item_ids.len());
        for index in 0..self.delegate.match_count() {
            let Some(item_id) = self.delegate.item_id(index) else {
                continue;
            };
            if self.selected_item_ids.contains(&item_id) && !ordered.contains(&item_id) {
                ordered.push(item_id);
            }
        }
        for item_id in &self.selected_item_ids {
            if !ordered.contains(item_id) && self.delegate.item_id_is_valid(item_id, cx) {
                ordered.push(item_id.clone());
            }
        }
        ordered
    }

    pub fn restoration_state(&self, cx: &App) -> PickerRestorationState {
        PickerRestorationState {
            query: self.query(cx),
            multi_select_enabled: self.multi_select_enabled,
            selected_item_ids: self.selected_item_ids.clone(),
        }
    }

    pub fn restore_state(
        &mut self,
        state: PickerRestorationState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.multi_select_enabled =
            state.multi_select_enabled && self.delegate.supports_multi_select();
        self.selected_item_ids = state.selected_item_ids;
        if self.query(cx) != state.query {
            self.set_query(&state.query, window, cx);
        }
        cx.notify();
    }

    fn toggle_multi_select(
        &mut self,
        _: &ToggleMultiSelect,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_multi_select_enabled(!self.multi_select_enabled, cx);
    }

    fn multi_select_next(
        &mut self,
        _: &MultiSelectNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.multi_select_enabled {
            self.set_multi_select_enabled(true, cx);
        }
        self.toggle_item_selection(self.delegate.selected_index(), window, cx);
        self.select_next(&menu::SelectNext, window, cx);
    }

    pub fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.delegate.should_dismiss() {
            self.record_reopen_request(cx);
            self.delegate.dismissed(window, cx);
            cx.emit(DismissEvent);
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_update_matches.is_some()
            && !self.delegate.finalize_update_matches(
                self.query(cx),
                Duration::from_millis(16),
                window,
                cx,
            )
        {
            self.confirm_on_update = Some(false)
        } else {
            self.pending_update_matches.take();
            self.do_confirm(false, window, cx);
        }
    }

    fn secondary_confirm(
        &mut self,
        _: &menu::SecondaryConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_update_matches.is_some()
            && !self.delegate.finalize_update_matches(
                self.query(cx),
                Duration::from_millis(16),
                window,
                cx,
            )
        {
            self.confirm_on_update = Some(true)
        } else {
            self.do_confirm(true, window, cx);
        }
    }

    fn confirm_input(&mut self, input: &ConfirmInput, window: &mut Window, cx: &mut Context<Self>) {
        self.delegate.confirm_input(input.secondary, window, cx);
    }

    fn confirm_completion(
        &mut self,
        _: &ConfirmCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(new_query) = self.delegate.confirm_completion(self.query(cx), window, cx) {
            self.set_query(&new_query, window, cx);
        } else {
            cx.propagate()
        }
    }

    fn handle_click(
        &mut self,
        ix: usize,
        secondary: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.prevent_default();
        if !self.delegate.can_select(ix, window, cx) {
            return;
        }
        self.set_selected_index(ix, None, false, window, cx);
        if secondary && self.delegate.supports_multi_select() {
            if !self.multi_select_enabled {
                self.set_multi_select_enabled(true, cx);
            }
            self.toggle_item_selection(ix, window, cx);
        } else {
            self.do_confirm(secondary, window, cx)
        }
    }

    fn do_confirm(&mut self, secondary: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(update_query) = self.delegate.confirm_update_query(window, cx) {
            self.set_query(&update_query, window, cx);
            self.set_selected_index(0, Some(Direction::Down), false, window, cx);
        } else if self.multi_select_enabled && !self.selected_item_ids.is_empty() {
            self.record_reopen_request(cx);
            let item_ids = self.ordered_selected_item_ids(cx);
            if item_ids.is_empty() {
                return;
            }
            self.delegate.confirm_multi(item_ids, window, cx);
        } else {
            self.record_reopen_request(cx);
            self.delegate.confirm(secondary, window, cx)
        }
    }

    fn record_reopen_request(&self, cx: &mut Context<Self>) {
        let state = self.restoration_state(cx);
        let Some(request) = self.delegate.reopen_request(&state, cx) else {
            return;
        };
        let Some(workspace) = self.delegate.workspace(cx) else {
            return;
        };
        workspace
            .update(cx, |workspace, _| {
                workspace.record_picker_request(
                    request,
                    workspace::StoredPickerState {
                        query: state.query,
                        multi_select_enabled: state.multi_select_enabled,
                        selected_item_ids: state
                            .selected_item_ids
                            .into_iter()
                            .map(PickerItemId::into_shared_string)
                            .collect(),
                    },
                );
            })
            .log_err();
    }

    fn on_input_editor_event(
        &mut self,
        event: &ErasedEditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Head::Editor(editor) = &self.head else {
            panic!("unexpected call");
        };
        match event {
            ErasedEditorEvent::BufferEdited => {
                let query = editor.text(cx);
                self.update_matches(query, window, cx);
            }
            ErasedEditorEvent::Blurred => {
                if self.is_modal && window.is_window_active() {
                    self.cancel(&menu::Cancel, window, cx);
                }
            }
        }
    }

    fn on_empty_head_blur(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Head::Empty(_) = &self.head else {
            panic!("unexpected call");
        };
        if window.is_window_active() {
            self.cancel(&menu::Cancel, window, cx);
        }
    }

    pub fn refresh_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.head {
            Head::Editor(editor) => {
                let placeholder = self.delegate.placeholder_text(window, cx);

                editor.set_placeholder_text(placeholder.as_ref(), window, cx);
                cx.notify();
            }
            Head::Empty(_) => {}
        }
    }

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.update_matches(query, window, cx);
    }

    pub fn update_matches(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.update_matches_with_options(query, ScrollBehavior::RevealSelected, window, cx);
    }

    pub fn update_matches_with_options(
        &mut self,
        query: String,
        scroll_behavior: ScrollBehavior,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delegate_pending_update_matches = self.delegate.update_matches(query, window, cx);

        self.matches_updated(scroll_behavior, window, cx);
        // This struct ensures that we can synchronously drop the task returned by the
        // delegate's `update_matches` method and the task that the picker is spawning.
        // If we simply capture the delegate's task into the picker's task, when the picker's
        // task gets synchronously dropped, the delegate's task would keep running until
        // the picker's task has a chance of being scheduled, because dropping a task happens
        // asynchronously.
        self.pending_update_matches = Some(PendingUpdateMatches {
            delegate_update_matches: Some(delegate_pending_update_matches),
            _task: cx.spawn_in(window, async move |this, cx| {
                let delegate_pending_update_matches = this.update(cx, |this, _| {
                    this.pending_update_matches
                        .as_mut()
                        .unwrap()
                        .delegate_update_matches
                        .take()
                        .unwrap()
                })?;
                delegate_pending_update_matches.await;
                this.update_in(cx, |this, window, cx| {
                    this.matches_updated(scroll_behavior, window, cx);
                })
            }),
        });
    }

    fn matches_updated(
        &mut self,
        scroll_behavior: ScrollBehavior,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let match_count = self.delegate.match_count();
        match &mut self.element_container {
            ElementContainer::List(state) => match scroll_behavior {
                ScrollBehavior::RevealSelected => {
                    state.reset(match_count);
                    let index = self.delegate.selected_index();
                    self.scroll_to_item_index(index);
                }
                ScrollBehavior::PreserveOffset => {
                    let offset = state.logical_scroll_top();
                    state.reset(match_count);
                    state.scroll_to(offset);
                }
            },
            ElementContainer::UniformList(_) => match scroll_behavior {
                ScrollBehavior::RevealSelected => {
                    let index = self.delegate.selected_index();
                    self.scroll_to_item_index(index);
                }
                ScrollBehavior::PreserveOffset => {}
            },
        }
        self.pending_update_matches = None;
        self.update_preview(window, cx);
        if let Some(secondary) = self.confirm_on_update.take() {
            self.do_confirm(secondary, window, cx);
        }
        cx.notify();
    }

    pub fn query(&self, cx: &App) -> String {
        match &self.head {
            Head::Editor(editor) => editor.text(cx),
            Head::Empty(_) => "".to_string(),
        }
    }

    pub fn set_query(&self, query: &str, window: &mut Window, cx: &mut App) {
        if let Head::Editor(editor) = &self.head {
            editor.set_text(query, window, cx);
            editor.move_selection_to_end(window, cx);
        }
    }

    pub fn select_query(&self, window: &mut Window, cx: &mut App) {
        if let Head::Editor(editor) = &self.head {
            editor.select_all(window, cx);
        }
    }

    fn scroll_to_item_index(&mut self, ix: usize) {
        match &mut self.element_container {
            ElementContainer::List(state) => state.scroll_to_reveal_item(ix),
            ElementContainer::UniformList(scroll_handle) => {
                scroll_handle.scroll_to_item(ix, ScrollStrategy::Nearest)
            }
        }
    }

    pub fn is_scrolled_to_end(&self) -> Option<bool> {
        match &self.element_container {
            ElementContainer::List(state) => state.is_scrolled_to_end(),
            ElementContainer::UniformList(scroll_handle) => scroll_handle.is_scrolled_to_end(),
        }
    }

    fn render_element(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        ix: usize,
    ) -> impl IntoElement + use<D> {
        let item_bounds = self.item_bounds.clone();
        let selectable =
            ix < self.delegate.match_count() && self.delegate.can_select(ix, window, cx);

        div()
            .id(("item", ix))
            .when(selectable, |this| this.cursor_pointer())
            .child(
                canvas(
                    move |bounds, _window, _cx| {
                        item_bounds.borrow_mut().insert(ix, bounds);
                    },
                    |_bounds, _state, _window, _cx| {},
                )
                .size_full()
                .absolute()
                .top_0()
                .left_0(),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.handle_click(ix, event.modifiers().secondary(), window, cx)
            }))
            // As of this writing, GPUI intercepts `ctrl-[mouse-event]`s on macOS
            // and produces right mouse button events. This matches platforms norms
            // but means that UIs which depend on holding ctrl down (such as the tab
            // switcher) can't be clicked on. Hence, this handler.
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseUpEvent, window, cx| {
                    // We specifically want to use the platform key here, as
                    // ctrl will already be held down for the tab switcher.
                    this.handle_click(ix, event.modifiers.platform, window, cx)
                }),
            )
            .when(self.delegate.select_on_hover(), |this| {
                this.on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                    if *hovered {
                        this.set_selected_index(ix, None, false, window, cx);
                        cx.notify();
                    }
                }))
            })
            .children(self.delegate.render_match_with_state(
                ix,
                ix == self.delegate.selected_index(),
                self.is_item_selected(ix),
                window,
                cx,
            ))
            .when(
                self.delegate.separators_after_indices().contains(&ix),
                |picker| {
                    picker
                        .border_color(cx.theme().colors().border_variant)
                        .border_b_1()
                        .py(px(-1.0))
                },
            )
    }

    fn render_element_container(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sizing_behavior = if self.max_height.is_some() {
            ListSizingBehavior::Infer
        } else {
            ListSizingBehavior::Auto
        };

        match &self.element_container {
            ElementContainer::UniformList(scroll_handle) => uniform_list(
                "candidates",
                self.delegate.match_count(),
                cx.processor(move |picker, visible_range: Range<usize>, window, cx| {
                    visible_range
                        .map(|ix| picker.render_element(window, cx, ix))
                        .collect()
                }),
            )
            .with_sizing_behavior(sizing_behavior)
            .when_some(self.widest_item, |el, widest_item| {
                el.with_width_from_item(Some(widest_item))
            })
            .flex_grow_1()
            .py(DynamicSpacing::Base04.rems(cx))
            .track_scroll(&scroll_handle)
            .into_any_element(),
            ElementContainer::List(state) => list(
                state.clone(),
                cx.processor(|this, ix, window, cx| {
                    this.render_element(window, cx, ix).into_any_element()
                }),
            )
            .with_sizing_behavior(sizing_behavior)
            .flex_grow_1()
            .py(DynamicSpacing::Base04.rems(cx))
            .into_any_element(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn logical_scroll_top_index(&self) -> usize {
        match &self.element_container {
            ElementContainer::List(state) => state.logical_scroll_top().item_ix,
            ElementContainer::UniformList(scroll_handle) => {
                scroll_handle.logical_scroll_top_index()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::{
        cell::{Cell, RefCell},
        collections::HashSet,
    };

    struct TestDelegate {
        items: Vec<(SharedString, bool)>,
        valid_item_ids: HashSet<SharedString>,
        selected_index: usize,
        confirmed_index: Rc<Cell<Option<usize>>>,
        confirmed_item_ids: Rc<RefCell<Vec<PickerItemId>>>,
    }

    impl TestDelegate {
        fn new(items: Vec<bool>) -> Self {
            let items: Vec<(SharedString, bool)> = items
                .into_iter()
                .enumerate()
                .map(|(index, selectable)| (index.to_string().into(), selectable))
                .collect::<Vec<_>>();
            let valid_item_ids = items.iter().map(|(id, _)| id.clone()).collect();
            Self {
                items,
                valid_item_ids,
                selected_index: 0,
                confirmed_index: Rc::new(Cell::new(None)),
                confirmed_item_ids: Rc::default(),
            }
        }

        fn with_ids(ids: &[&str]) -> Self {
            let items: Vec<(SharedString, bool)> = ids
                .iter()
                .map(|id| (SharedString::from(*id), true))
                .collect::<Vec<_>>();
            let valid_item_ids = items.iter().map(|(id, _)| id.clone()).collect();
            Self {
                items,
                valid_item_ids,
                selected_index: 0,
                confirmed_index: Rc::new(Cell::new(None)),
                confirmed_item_ids: Rc::default(),
            }
        }
    }

    impl PickerDelegate for TestDelegate {
        type ListItem = ui::ListItem;

        fn match_count(&self) -> usize {
            self.items.len()
        }

        fn selected_index(&self) -> usize {
            self.selected_index
        }

        fn set_selected_index(
            &mut self,
            ix: usize,
            _window: &mut Window,
            _cx: &mut Context<Picker<Self>>,
        ) {
            self.selected_index = ix;
        }

        fn can_select(
            &self,
            ix: usize,
            _window: &mut Window,
            _cx: &mut Context<Picker<Self>>,
        ) -> bool {
            self.items
                .get(ix)
                .map(|(_, selectable)| *selectable)
                .unwrap_or(false)
        }

        fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
            "Test".into()
        }

        fn update_matches(
            &mut self,
            _query: String,
            _window: &mut Window,
            _cx: &mut Context<Picker<Self>>,
        ) -> Task<()> {
            Task::ready(())
        }

        fn confirm(
            &mut self,
            _secondary: bool,
            _window: &mut Window,
            _cx: &mut Context<Picker<Self>>,
        ) {
            self.confirmed_index.set(Some(self.selected_index));
        }

        fn supports_multi_select(&self) -> bool {
            true
        }

        fn item_id(&self, index: usize) -> Option<PickerItemId> {
            Some(PickerItemId::new(self.items.get(index)?.0.clone()))
        }

        fn item_id_is_valid(&self, id: &PickerItemId, _cx: &App) -> bool {
            self.valid_item_ids.contains(id.as_str())
        }

        fn confirm_multi(
            &mut self,
            ids: Vec<PickerItemId>,
            _window: &mut Window,
            _cx: &mut Context<Picker<Self>>,
        ) {
            *self.confirmed_item_ids.borrow_mut() = ids;
        }

        fn dismissed(&mut self, _window: &mut Window, _cx: &mut Context<Picker<Self>>) {}

        fn render_match(
            &self,
            ix: usize,
            selected: bool,
            _window: &mut Window,
            _cx: &mut Context<Picker<Self>>,
        ) -> Option<Self::ListItem> {
            Some(
                ui::ListItem::new(ix)
                    .inset(true)
                    .toggle_state(selected)
                    .child(ui::Label::new(format!("Item {ix}"))),
            )
        }
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
    }

    #[gpui::test]
    async fn test_clicking_non_selectable_item_does_not_confirm(cx: &mut TestAppContext) {
        init_test(cx);

        let confirmed_index = Rc::new(Cell::new(None));
        let (picker, cx) = cx.add_window_view(|window, cx| {
            let mut delegate = TestDelegate::new(vec![true, false, true]);
            delegate.confirmed_index = confirmed_index.clone();
            Picker::uniform_list(delegate, window, cx)
        });

        picker.update(cx, |picker, _cx| {
            assert_eq!(picker.delegate.selected_index(), 0);
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.handle_click(1, false, window, cx);
        });
        assert!(
            confirmed_index.get().is_none(),
            "clicking a non-selectable item should not confirm"
        );

        picker.update_in(cx, |picker, window, cx| {
            picker.handle_click(0, false, window, cx);
        });
        assert_eq!(
            confirmed_index.get(),
            Some(0),
            "clicking a selectable item should confirm"
        );
    }

    #[gpui::test]
    async fn test_keyboard_navigation_skips_non_selectable_items(cx: &mut TestAppContext) {
        init_test(cx);

        let (picker, cx) = cx.add_window_view(|window, cx| {
            Picker::uniform_list(TestDelegate::new(vec![true, false, true]), window, cx)
        });

        picker.update(cx, |picker, _cx| {
            assert_eq!(picker.delegate.selected_index(), 0);
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.select_next(&menu::SelectNext, window, cx);
        });
        picker.update(cx, |picker, _cx| {
            assert_eq!(
                picker.delegate.selected_index(),
                2,
                "select_next should skip non-selectable item at index 1"
            );
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.select_previous(&menu::SelectPrevious, window, cx);
        });
        picker.update(cx, |picker, _cx| {
            assert_eq!(
                picker.delegate.selected_index(),
                0,
                "select_previous should skip non-selectable item at index 1"
            );
        });
    }

    #[gpui::test]
    async fn test_multi_select_uses_stable_ids_across_reordering_and_filtering(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let (picker, cx) = cx.add_window_view(|window, cx| {
            Picker::uniform_list(TestDelegate::with_ids(&["a", "b", "c"]), window, cx)
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.set_multi_select_enabled(true, cx);
            picker.toggle_item_selection(1, window, cx);
            assert_eq!(
                picker
                    .selected_item_ids()
                    .iter()
                    .map(PickerItemId::as_str)
                    .collect::<Vec<_>>(),
                vec!["b"]
            );

            picker.delegate.items =
                vec![("c".into(), true), ("b".into(), true), ("a".into(), true)];
            assert_eq!(
                picker
                    .selected_item_ids()
                    .iter()
                    .map(PickerItemId::as_str)
                    .collect::<Vec<_>>(),
                vec!["b"]
            );

            picker.delegate.items = vec![("a".into(), true)];
            assert_eq!(
                picker
                    .selected_item_ids()
                    .iter()
                    .map(PickerItemId::as_str)
                    .collect::<Vec<_>>(),
                vec!["b"],
                "filtering must not discard a still-valid identity"
            );

            picker.delegate.valid_item_ids.remove("b");
            picker.reconcile_multi_selection(cx);
            assert!(picker.selected_item_ids().is_empty());

            picker.delegate.items = vec![("disabled".into(), false)];
            picker.delegate.valid_item_ids.insert("disabled".into());
            picker.toggle_item_selection(0, window, cx);
            assert!(
                picker.selected_item_ids().is_empty(),
                "disabled entries must not become selected"
            );
        });
    }

    #[gpui::test]
    async fn test_multi_confirm_follows_current_result_order(cx: &mut TestAppContext) {
        init_test(cx);

        let confirmed_item_ids = Rc::new(RefCell::new(Vec::new()));
        let (picker, cx) = cx.add_window_view(|window, cx| {
            let mut delegate = TestDelegate::with_ids(&["a", "b", "c"]);
            delegate.confirmed_item_ids = confirmed_item_ids.clone();
            Picker::uniform_list(delegate, window, cx)
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.set_multi_select_enabled(true, cx);
            picker.toggle_item_selection(2, window, cx);
            picker.toggle_item_selection(0, window, cx);
            picker.delegate.items =
                vec![("b".into(), true), ("a".into(), true), ("c".into(), true)];
            picker.do_confirm(false, window, cx);
        });

        assert_eq!(
            confirmed_item_ids
                .borrow()
                .iter()
                .map(PickerItemId::as_str)
                .collect::<Vec<_>>(),
            vec!["a", "c"]
        );
    }

    #[gpui::test]
    async fn test_secondary_click_toggles_multi_selection(cx: &mut TestAppContext) {
        init_test(cx);

        let (picker, cx) = cx.add_window_view(|window, cx| {
            Picker::uniform_list(TestDelegate::with_ids(&["a", "b"]), window, cx)
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.handle_click(1, true, window, cx);
            assert!(picker.multi_select_enabled());
            assert_eq!(
                picker
                    .selected_item_ids()
                    .iter()
                    .map(PickerItemId::as_str)
                    .collect::<Vec<_>>(),
                vec!["b"]
            );
            assert_eq!(
                picker.delegate.confirmed_index.get(),
                None,
                "secondary-clicking a multi-select delegate must not confirm"
            );
        });
    }

    #[gpui::test]
    async fn test_restoration_state_preserves_query_mode_and_stable_ids(cx: &mut TestAppContext) {
        init_test(cx);

        let (picker, cx) = cx.add_window_view(|window, cx| {
            Picker::uniform_list(TestDelegate::with_ids(&["a", "b"]), window, cx)
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.restore_state(
                PickerRestorationState {
                    query: "needle".to_string(),
                    multi_select_enabled: true,
                    selected_item_ids: vec![PickerItemId::new("b")],
                },
                window,
                cx,
            );
        });
        cx.run_until_parked();

        picker.read_with(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "needle");
            assert!(picker.multi_select_enabled());
            assert_eq!(
                picker
                    .selected_item_ids()
                    .iter()
                    .map(PickerItemId::as_str)
                    .collect::<Vec<_>>(),
                vec!["b"]
            );
        });
    }
}

impl<D: PickerDelegate> EventEmitter<DismissEvent> for Picker<D> {}
impl<D: PickerDelegate> ModalView for Picker<D> {}

impl<D: PickerDelegate> Render for Picker<D> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui_font_size = ThemeSettings::get_global(cx).ui_font_size(cx);
        let window_size = window.viewport_size();
        let rem_size = window.rem_size();
        let is_wide_window = window_size.width / rem_size > rems_from_px(800.).0;

        let aside = self.delegate.documentation_aside(window, cx);

        let editor_position = self.delegate.editor_position();
        let picker_bounds = self.picker_bounds.clone();
        let menu = v_flex()
            .key_context("Picker")
            .size_full()
            .when_some(self.width, |el, width| el.w(width))
            .overflow_hidden()
            .child(
                canvas(
                    move |bounds, _window, _cx| {
                        picker_bounds.set(Some(bounds));
                    },
                    |_bounds, _state, _window, _cx| {},
                )
                .size_full()
                .absolute()
                .top_0()
                .left_0(),
            )
            // This is a bit of a hack to remove the modal styling when we're rendering the `Picker`
            // as a part of a modal rather than the entire modal.
            //
            // We should revisit how the `Picker` is styled to make it more composable.
            .when(self.is_modal, |this| this.elevation_3(cx))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::editor_move_down))
            .on_action(cx.listener(Self::editor_move_up))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::secondary_confirm))
            .on_action(cx.listener(Self::confirm_completion))
            .on_action(cx.listener(Self::confirm_input))
            .on_action(cx.listener(Self::toggle_multi_select))
            .on_action(cx.listener(Self::multi_select_next))
            .on_action(cx.listener(Self::toggle_preview))
            .on_action(cx.listener(Self::set_preview_right))
            .on_action(cx.listener(Self::set_preview_below))
            .on_action(cx.listener(Self::set_preview_hidden))
            .children(match &self.head {
                Head::Editor(editor) => {
                    if editor_position == PickerEditorPosition::Start {
                        Some(self.delegate.render_editor(&editor.clone(), window, cx))
                    } else {
                        None
                    }
                }
                Head::Empty(empty_head) => Some(div().child(empty_head.clone())),
            })
            .when(self.delegate.match_count() > 0, |el| {
                el.child(
                    v_flex()
                        .id("element-container")
                        .relative()
                        .flex_grow_1()
                        .when_some(self.max_height, |div, max_h| div.max_h(max_h))
                        .overflow_hidden()
                        .children(self.delegate.render_header(window, cx))
                        .child(self.render_element_container(cx))
                        .when(self.show_scrollbar, |this| {
                            let base_scrollbar_config = Scrollbars::new(ScrollAxes::Vertical);

                            this.map(|this| match &self.element_container {
                                ElementContainer::List(state) => this.custom_scrollbars(
                                    base_scrollbar_config.tracked_scroll_handle(state),
                                    window,
                                    cx,
                                ),
                                ElementContainer::UniformList(state) => this.custom_scrollbars(
                                    base_scrollbar_config.tracked_scroll_handle(state),
                                    window,
                                    cx,
                                ),
                            })
                        }),
                )
            })
            .when(self.delegate.match_count() == 0, |el| {
                el.when_some(self.delegate.no_matches_text(window, cx), |el, text| {
                    el.child(
                        v_flex()
                            .flex_grow_1()
                            .py(DynamicSpacing::Base04.rems(cx))
                            .child(
                                ListItem::new("empty_state")
                                    .inset(true)
                                    .spacing(ListItemSpacing::Sparse)
                                    .disabled(true)
                                    .child(Label::new(text).color(Color::Muted)),
                            ),
                    )
                })
            })
            .when(self.delegate.supports_multi_select(), |picker| {
                let selected_count = self.selected_item_ids.len();
                picker.child(
                    h_flex()
                        .w_full()
                        .px_2()
                        .py_1()
                        .justify_between()
                        .border_t_1()
                        .border_color(cx.theme().colors().border_variant)
                        .child(
                            Label::new(if self.multi_select_enabled {
                                localization::tr!(
                                    cx,
                                    "picker-selected-count",
                                    count = selected_count as i64
                                )
                            } else {
                                localization::text(cx, "picker-select-multiple")
                            })
                            .size(ui::LabelSize::Small)
                            .color(Color::Muted),
                        )
                        .child(
                            Button::new(
                                "toggle-picker-multi-select",
                                if self.multi_select_enabled {
                                    localization::text(cx, "picker-done")
                                } else {
                                    localization::text(cx, "picker-select-multiple-action")
                                },
                            )
                            .on_click(|_, window, cx| {
                                window.dispatch_action(ToggleMultiSelect.boxed_clone(), cx);
                            }),
                        ),
                )
            })
            .children(self.delegate.render_footer(window, cx))
            .when(self.preview.is_some(), |menu| {
                menu.child(self.render_preview_controls(cx))
            })
            .children(match &self.head {
                Head::Editor(editor) => {
                    if editor_position == PickerEditorPosition::End {
                        Some(self.delegate.render_editor(&editor.clone(), window, cx))
                    } else {
                        None
                    }
                }
                Head::Empty(empty_head) => Some(div().child(empty_head.clone())),
            });

        let results: AnyElement = if let Some(aside) = aside {
            let render_aside = |aside: DocumentationAside, cx: &mut Context<Self>| {
                WithRemSize::new(ui_font_size)
                    .occlude()
                    .elevation_2(cx)
                    .w_full()
                    .p_2()
                    .overflow_hidden()
                    .when(is_wide_window, |this| this.max_w_96())
                    .when(!is_wide_window, |this| this.max_w_48())
                    .child((aside.render)(cx))
            };

            if is_wide_window {
                let aside_index = self.delegate.documentation_aside_index();
                let picker_bounds = self.picker_bounds.get();
                let item_bounds =
                    aside_index.and_then(|ix| self.item_bounds.borrow().get(&ix).copied());

                let item_position = match (picker_bounds, item_bounds) {
                    (Some(picker_bounds), Some(item_bounds)) => {
                        let relative_top = item_bounds.origin.y - picker_bounds.origin.y;
                        let height = item_bounds.size.height;
                        Some((relative_top, height))
                    }
                    _ => None,
                };

                div()
                    .relative()
                    .child(menu)
                    // Only render the aside once we have bounds to avoid flicker
                    .when_some(item_position, |this, (top, height)| {
                        this.child(
                            h_flex()
                                .absolute()
                                .when(aside.side == DocumentationSide::Left, |el| {
                                    el.right_full().mr_1()
                                })
                                .when(aside.side == DocumentationSide::Right, |el| {
                                    el.left_full().ml_1()
                                })
                                .top(top)
                                .h(height)
                                .child(render_aside(aside, cx)),
                        )
                    })
                    .into_any_element()
            } else {
                v_flex()
                    .w_full()
                    .gap_1()
                    .justify_end()
                    .child(render_aside(aside, cx))
                    .child(menu)
                    .into_any_element()
            }
        } else {
            menu.into_any_element()
        };

        self.render_with_preview(results, window, cx)
    }
}

impl<D: PickerDelegate> Picker<D> {
    fn update_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        match self.delegate.try_get_preview_data_for_match(cx) {
            Some(update) => preview.update(update, window, cx),
            None => preview.clear(cx),
        }
    }

    fn render_with_preview(
        &mut self,
        results: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(preview) = self.preview.as_ref() else {
            return results;
        };
        match preview.layout {
            PreviewLayout::Hidden => results,
            PreviewLayout::Right => {
                let preview_width = self.preview_width(window);
                let preview_element = preview.render(cx);
                let border_color = cx.theme().colors().border_variant;
                h_flex()
                    .id("picker-preview-split-right")
                    .size_full()
                    .child(div().flex_1().min_w_0().overflow_hidden().child(results))
                    .child(
                        div()
                            .id("picker-preview-divider-right")
                            .w(px(1.))
                            .h_full()
                            .bg(border_color)
                            .cursor(CursorStyle::ResizeColumn)
                            .on_drag(
                                DividerDrag::Right {
                                    start_x: window.mouse_position().x,
                                    start_width: preview_width,
                                },
                                |_, _, _, cx| cx.new(|_| DividerDragView),
                            )
                            .on_drag_move::<DividerDrag>(cx.listener(
                                |this, event: &DragMoveEvent<DividerDrag>, _window, cx| {
                                    let DividerDrag::Right {
                                        start_x,
                                        start_width,
                                    } = event.drag(cx)
                                    else {
                                        return;
                                    };
                                    let delta = event.event.position.x - *start_x;
                                    this.set_preview_size(Some(*start_width - delta), _window, cx);
                                },
                            )),
                    )
                    .child(
                        div()
                            .w(preview_width)
                            .h_full()
                            .overflow_hidden()
                            .child(preview_element),
                    )
                    .into_any_element()
            }
            PreviewLayout::Below => {
                let preview_height = self.preview_height(window);
                let preview_element = preview.render(cx);
                let border_color = cx.theme().colors().border_variant;
                v_flex()
                    .id("picker-preview-split-below")
                    .size_full()
                    .child(div().flex_1().min_h_0().overflow_hidden().child(results))
                    .child(
                        div()
                            .id("picker-preview-divider-below")
                            .h(px(1.))
                            .w_full()
                            .bg(border_color)
                            .cursor(CursorStyle::ResizeRow)
                            .on_drag(
                                DividerDrag::Below {
                                    start_y: window.mouse_position().y,
                                    start_height: preview_height,
                                },
                                |_, _, _, cx| cx.new(|_| DividerDragView),
                            )
                            .on_drag_move::<DividerDrag>(cx.listener(
                                |this, event: &DragMoveEvent<DividerDrag>, _window, cx| {
                                    let DividerDrag::Below {
                                        start_y,
                                        start_height,
                                    } = event.drag(cx)
                                    else {
                                        return;
                                    };
                                    let delta = event.event.position.y - *start_y;
                                    this.set_preview_size(Some(*start_height - delta), _window, cx);
                                },
                            )),
                    )
                    .child(
                        div()
                            .h(preview_height)
                            .w_full()
                            .overflow_hidden()
                            .child(preview_element),
                    )
                    .into_any_element()
            }
        }
    }

    pub fn preview_layout(&self) -> Option<PreviewLayout> {
        self.preview.as_ref().map(|preview| preview.layout)
    }

    pub fn preview_current_path(
        &self,
        cx: &App,
    ) -> Option<std::sync::Arc<util::rel_path::RelPath>> {
        self.preview
            .as_ref()
            .and_then(|preview| preview.current_path(cx))
    }

    pub fn preview_width(&self, window: &mut Window) -> Pixels {
        let viewport = window.viewport_size().width;
        let max = (viewport - MIN_PREVIEW_PX).max(MIN_PREVIEW_PX);
        self.preview_size
            .unwrap_or_else(|| (viewport * 0.4).clamp(MIN_PREVIEW_PX, max))
            .clamp(MIN_PREVIEW_PX, max)
    }

    pub fn preview_height(&self, window: &mut Window) -> Pixels {
        let viewport = window.viewport_size().height;
        let max = (viewport - MIN_PREVIEW_PX).max(MIN_PREVIEW_PX);
        self.preview_size
            .unwrap_or_else(|| (viewport * 0.3).clamp(MIN_PREVIEW_PX, max))
            .clamp(MIN_PREVIEW_PX, max)
    }

    pub fn set_preview_size(
        &mut self,
        size: Option<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preview_size = size;
        cx.notify();
    }

    fn toggle_preview(&mut self, _: &TogglePreview, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_preview_visible(window, cx);
    }

    fn toggle_preview_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = match self.preview_layout() {
            Some(PreviewLayout::Hidden) | None => PreviewLayout::Right,
            Some(_) => PreviewLayout::Hidden,
        };
        self.set_preview_layout(next, window, cx);
    }

    fn render_preview_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current_layout = self.preview_layout().unwrap_or(PreviewLayout::Hidden);
        let preview_visible = current_layout != PreviewLayout::Hidden;

        h_flex()
            .w_full()
            .p_1p5()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                Button::new(
                    "picker-preview-toggle",
                    localization::text(cx, "picker-preview"),
                )
                .when(preview_visible, |button| button.color(Color::Accent))
                .on_click(cx.listener(|picker, _, window, cx| {
                    picker.toggle_preview_visible(window, cx);
                })),
            )
            .when(preview_visible, |controls| {
                controls
                    .child(Divider::vertical())
                    .child(
                        IconButton::new("picker-preview-right", IconName::DiffSplit)
                            .toggle_state(current_layout == PreviewLayout::Right)
                            .tooltip(Tooltip::text(localization::text(
                                cx,
                                "picker-preview-right",
                            )))
                            .on_click(cx.listener(|picker, _, window, cx| {
                                picker.set_preview_layout(PreviewLayout::Right, window, cx);
                            })),
                    )
                    .child(
                        IconButton::new("picker-preview-below", IconName::DiffUnified)
                            .toggle_state(current_layout == PreviewLayout::Below)
                            .tooltip(Tooltip::text(localization::text(
                                cx,
                                "picker-preview-below",
                            )))
                            .on_click(cx.listener(|picker, _, window, cx| {
                                picker.set_preview_layout(PreviewLayout::Below, window, cx);
                            })),
                    )
            })
    }

    fn set_preview_right(
        &mut self,
        _: &SetPreviewRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_preview_layout(PreviewLayout::Right, window, cx);
    }

    fn set_preview_below(
        &mut self,
        _: &SetPreviewBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_preview_layout(PreviewLayout::Below, window, cx);
    }

    fn set_preview_hidden(
        &mut self,
        _: &SetPreviewHidden,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_preview_layout(PreviewLayout::Hidden, window, cx);
    }

    fn set_preview_layout(
        &mut self,
        layout: PreviewLayout,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        preview.layout = layout;
        self.preview_size = None;
        persistence::store_layout(D::name(), Some(layout), cx);
        self.delegate
            .preview_layout_changed(matches!(layout, PreviewLayout::Right));
        cx.notify();
    }
}
