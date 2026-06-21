use gpui::{IntoElement, ParentElement, Styled};
use ui::{Divider, DividerColor, prelude::*};

#[derive(IntoElement)]
pub struct SettingsSectionHeader {
    label: SharedString,
}

impl SettingsSectionHeader {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

impl RenderOnce for SettingsSectionHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let label = Label::new(self.label)
            .size(LabelSize::Small)
            .color(Color::Muted)
            .buffer_font(cx);

        v_flex()
            .w_full()
            .px_8()
            .gap_1p5()
            .child(label)
            .child(Divider::horizontal().color(DividerColor::BorderFaded))
    }
}
