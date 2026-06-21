use std::rc::Rc;

use editor::Editor;
use gpui::Focusable;
use ui::prelude::*;

#[derive(IntoElement)]
pub struct SettingsInputField {
    initial_text: Option<String>,
    placeholder: Option<&'static str>,
    confirm: Option<Rc<dyn Fn(Option<String>, &mut Window, &mut App)>>,
    tab_index: Option<isize>,
}

impl SettingsInputField {
    pub fn new() -> Self {
        Self {
            initial_text: None,
            placeholder: None,
            confirm: None,
            tab_index: None,
        }
    }

    pub fn with_initial_text(mut self, initial_text: String) -> Self {
        self.initial_text = Some(initial_text);
        self
    }

    pub fn with_placeholder(mut self, placeholder: &'static str) -> Self {
        self.placeholder = Some(placeholder);
        self
    }

    pub fn on_confirm(
        mut self,
        confirm: impl Fn(Option<String>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.confirm = Some(Rc::new(confirm));
        self
    }

    pub(crate) fn tab_index(mut self, arg: isize) -> Self {
        self.tab_index = Some(arg);
        self
    }
}

impl RenderOnce for SettingsInputField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl ui::IntoElement {
        let first_render_initial_text = window.use_state(cx, |_, _| self.initial_text.clone());

        let editor = window.use_state(cx, {
            let initial_text = self.initial_text.clone();
            let placeholder = self.placeholder;
            let mut confirm = self.confirm.clone();

            move |window, cx| {
                let mut editor = Editor::single_line(window, cx);
                let editor_focus_handle = editor.focus_handle(cx);
                if let Some(text) = initial_text {
                    editor.set_text(text, window, cx);
                }

                if let Some(confirm) = confirm.take() {
                    cx.on_focus_out(
                        &editor_focus_handle,
                        window,
                        move |editor, _, window, cx| {
                            let text = Some(editor.text(cx));
                            confirm(text, window, cx);
                        },
                    )
                    .detach();
                }

                if let Some(placeholder) = placeholder {
                    editor.set_placeholder_text(placeholder, window, cx);
                }
                editor
            }
        });

        // When settings change externally (e.g. editing settings.json), the page
        // re-renders but use_state returns the cached editor with stale text.
        // Reconcile with the expected initial_text when the editor is not focused,
        // so we don't clobber what the user is actively typing.
        if let Some(initial_text) = &self.initial_text
            && let Some(first_initial) = first_render_initial_text.read(cx)
        {
            if initial_text != first_initial && !editor.read(cx).is_focused(window) {
                *first_render_initial_text.as_mut(cx) = self.initial_text.clone();
                let weak_editor = editor.downgrade();
                let initial_text = initial_text.clone();

                window.defer(cx, move |window, cx| {
                    weak_editor
                        .update(cx, |editor, cx| {
                            editor.set_text(initial_text, window, cx);
                        })
                        .ok();
                });
            }
        }

        let weak_editor = editor.downgrade();
        let theme_colors = cx.theme().colors();

        h_flex()
            .py_1()
            .px_2()
            .h_8()
            .min_w_64()
            .rounded_md()
            .border_1()
            .border_color(theme_colors.border)
            .bg(theme_colors.editor_background)
            .when_some(self.tab_index, |this, tab_index| {
                let focus_handle = editor.focus_handle(cx).tab_index(tab_index).tab_stop(true);
                this.track_focus(&focus_handle)
                    .focus(|s| s.border_color(theme_colors.border_focused))
            })
            .child(editor)
            .when_some(self.confirm, |this, confirm| {
                this.on_action::<menu::Confirm>({
                    move |_, window, cx| {
                        let Some(editor) = weak_editor.upgrade() else {
                            return;
                        };
                        let new_value = editor.read_with(cx, |editor, cx| editor.text(cx));
                        let new_value = (!new_value.is_empty()).then_some(new_value);
                        confirm(new_value, window, cx);
                    }
                })
            })
    }
}
