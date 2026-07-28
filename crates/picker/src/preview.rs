use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use editor::{Editor, MultiBuffer};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, IntoElement, SharedString, Task, Window, div,
    prelude::*, px,
};
use gpui_util::ResultExt;
use language::{Bias, Buffer, Capability, ToPoint};
use project::{Project, Symbol};
use ui::{Color, Label, LabelCommon, v_flex};
use util::rel_path::RelPath;

/// The preview window of a [`Picker`](crate::Picker).
pub struct Preview {
    content: Entity<EditorPreview>,
    pub(crate) layout: Layout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    Hidden,
    Below,
    Right,
}

impl Preview {
    pub fn new_editor(project: Entity<Project>, window: &mut Window, cx: &mut App) -> Self {
        Preview {
            content: cx.new(|cx| EditorPreview::new(project, window, cx)),
            layout: Layout::default(),
        }
    }

    pub fn update(&mut self, update: Update, window: &mut Window, cx: &mut impl AppContext) {
        self.content
            .update(cx, |content, cx| content.update(update, window, cx));
    }

    pub fn render(&self, cx: &mut App) -> AnyElement {
        let layout = self.layout;
        self.content.update(cx, |content, cx| {
            content.render(layout, cx).into_any_element()
        })
    }

    pub fn current_path(&self, cx: &App) -> Option<Arc<RelPath>> {
        self.content.read(cx).current_path.clone()
    }

    pub(crate) fn clear(&mut self, cx: &mut App) {
        self.content.update(cx, |content, cx| {
            content.clear();
            cx.notify();
        });
    }
}

pub enum Update {
    Path(PathBuf),
    Buffer {
        buffer: Entity<Buffer>,
        match_range: Range<language::Anchor>,
    },
    Symbol(Symbol),
}

impl Update {
    pub fn from_path(abs_path: PathBuf) -> Self {
        Self::Path(abs_path)
    }

    pub fn from_buffer(buffer: Entity<Buffer>, match_range: Range<language::Anchor>) -> Self {
        Self::Buffer {
            buffer,
            match_range,
        }
    }

    pub fn from_symbol(symbol: Symbol) -> Self {
        Self::Symbol(symbol)
    }
}

pub struct EditorPreview {
    project: Entity<Project>,
    current_path: Option<Arc<RelPath>>,
    message: Option<SharedString>,
    preview_editor: Entity<Editor>,
    load_guard: PreviewLoadGuard,
    load_task: Task<()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PreviewLoadId(u64);

#[derive(Default)]
struct PreviewLoadGuard {
    current_id: u64,
}

impl PreviewLoadGuard {
    fn begin_load(&mut self) -> PreviewLoadId {
        self.invalidate();
        PreviewLoadId(self.current_id)
    }

    fn is_current(&self, load_id: PreviewLoadId) -> bool {
        load_id.0 == self.current_id
    }

    fn invalidate(&mut self) {
        self.current_id = self.current_id.wrapping_add(1);
    }
}

impl EditorPreview {
    fn new(project: Entity<Project>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let preview_editor = cx.new(|cx: &mut Context<Editor>| {
            let capability = Capability::ReadWrite;
            let multi_buffer = cx.new(|_| MultiBuffer::without_headers(capability));
            let mut editor = Editor::for_multibuffer(multi_buffer, None, window, cx);

            // The editor acts as a read-only preview: never editable.
            editor.set_read_only(true);
            editor.disable_scrollbars_and_minimap(window, cx);
            editor.disable_inline_diagnostics();
            editor.disable_diagnostics(cx);
            editor.disable_expand_excerpt_buttons(cx);
            editor.disable_mouse_wheel_zoom();
            editor.set_show_gutter(true, cx);
            editor.set_show_line_numbers(true, cx);
            editor.set_show_breakpoints(false, cx);
            editor.set_show_bookmarks(false, cx);
            editor.set_show_code_actions(false, cx);
            editor.set_show_runnables(false, cx);
            editor.set_show_git_diff_gutter(false, cx);
            editor.set_show_wrap_guides(false, cx);
            editor.set_show_indent_guides(false, cx);
            editor.set_show_cursor_when_unfocused(true, cx);
            editor.set_soft_wrap_mode(language::language_settings::SoftWrap::None, cx);
            editor
        });

        let mut this = Self {
            project,
            preview_editor,
            current_path: None,
            message: None,
            load_guard: PreviewLoadGuard::default(),
            load_task: Task::ready(()),
        };
        // The picker starts with no results, so show a placeholder.
        this.clear();
        this
    }

    fn clear(&mut self) {
        self.load_guard.invalidate();
        self.load_task = Task::ready(());
        self.current_path = None;
        self.message = Some("No results to preview".into());
    }

    fn update(&mut self, update: Update, window: &mut Window, cx: &mut Context<Self>) {
        match update {
            Update::Path(abs_path) => self.update_from_path(abs_path, window, cx),
            Update::Buffer {
                buffer,
                match_range,
            } => {
                self.load_guard.begin_load();
                self.load_task = Task::ready(());
                self.update_from_buffer(buffer, Some(match_range), window, cx);
                cx.notify();
            }
            Update::Symbol(symbol) => self.update_from_symbol(symbol, window, cx),
        }
    }

    fn update_from_path(&mut self, abs_path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let load_id = self.load_guard.begin_load();

        let open_task = self.project.update(cx, |project, cx| {
            match project.project_path_for_absolute_path(&abs_path, cx) {
                Some(project_path) => {
                    if let Some(buffer) = project.get_open_buffer(&project_path, cx) {
                        Task::ready(Ok(buffer))
                    } else {
                        project.open_buffer(project_path, cx)
                    }
                }
                None => project.open_local_buffer(&abs_path, cx),
            }
        });

        self.load_task = cx.spawn_in(window, async move |this, cx| match open_task.await {
            Ok(buffer) => {
                this.update_in(cx, |this, window, cx| {
                    if !this.load_guard.is_current(load_id) {
                        return;
                    }
                    this.update_from_buffer(buffer, None, window, cx);
                    cx.notify();
                })
                .log_err();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    if !this.load_guard.is_current(load_id) {
                        return;
                    }
                    this.current_path = None;
                    this.message = Some(format!("Unable to preview file: {error:#}").into());
                    cx.notify();
                })
                .log_err();
            }
        });
    }

    fn update_from_symbol(&mut self, symbol: Symbol, window: &mut Window, cx: &mut Context<Self>) {
        let load_id = self.load_guard.begin_load();
        let open_task = self.project.update(cx, |project, cx| {
            project.open_buffer_for_symbol(&symbol, cx)
        });

        self.load_task = cx.spawn_in(window, async move |this, cx| match open_task.await {
            Ok(buffer) => {
                this.update_in(cx, |this, window, cx| {
                    if !this.load_guard.is_current(load_id) {
                        return;
                    }
                    let snapshot = buffer.read(cx).text_snapshot();
                    let start = snapshot.clip_point_utf16(symbol.range.start, Bias::Left);
                    let end = snapshot.clip_point_utf16(symbol.range.end, Bias::Left);
                    let match_range = snapshot.anchor_before(start)..snapshot.anchor_after(end);
                    this.update_from_buffer(buffer, Some(match_range), window, cx);
                    cx.notify();
                })
                .log_err();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    if !this.load_guard.is_current(load_id) {
                        return;
                    }
                    this.current_path = None;
                    this.message = Some(format!("Unable to preview symbol: {error:#}").into());
                    cx.notify();
                })
                .log_err();
            }
        });
    }

    fn update_from_buffer(
        &mut self,
        buffer: Entity<Buffer>,
        match_range: Option<Range<language::Anchor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.message = None;
        self.current_path = buffer.read(cx).file().map(|file| file.path().clone());

        const MIN_LINE_HEIGHT_PX: gpui::Pixels = px(6.0);
        const MARGIN: u32 = 2;
        let max_rows = (window.viewport_size().height / MIN_LINE_HEIGHT_PX).ceil() as u32 + MARGIN;

        self.preview_editor.update(cx, |editor, cx| {
            let focus_row = match_range
                .as_ref()
                .map(|range| range.start.to_point(&buffer.read(cx).text_snapshot()).row)
                .unwrap_or_default()
                .min(max_rows);
            let multi_buffer = editor.buffer().clone();
            multi_buffer.update(cx, |multi_buffer, cx| {
                multi_buffer.clear(cx);
                // Anchor the excerpt at the start of the file; `max_rows` bounds
                // how much is materialized for a very large file.
                multi_buffer.set_excerpts_for_buffer(
                    buffer,
                    [rope::Point::new(focus_row, 0)..rope::Point::new(focus_row, 0)],
                    max_rows,
                    cx,
                );
            });
        });
    }

    pub(crate) fn render(&self, layout: Layout, _cx: &mut App) -> impl IntoElement {
        match layout {
            Layout::Hidden => div().into_any_element(),
            Layout::Right | Layout::Below => v_flex()
                .size_full()
                .child(if let Some(message) = &self.message {
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .child(Label::new(message.clone()).color(Color::Muted))
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(self.preview_editor.clone())
                        .into_any_element()
                })
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PreviewLoadGuard;

    #[test]
    fn only_latest_preview_load_is_current() {
        let mut guard = PreviewLoadGuard::default();

        let first_load = guard.begin_load();
        let second_load = guard.begin_load();

        assert!(!guard.is_current(first_load));
        assert!(guard.is_current(second_load));
    }

    #[test]
    fn invalidating_preview_load_rejects_pending_completion() {
        let mut guard = PreviewLoadGuard::default();
        let pending_load = guard.begin_load();

        guard.invalidate();

        assert!(!guard.is_current(pending_load));
    }
}
