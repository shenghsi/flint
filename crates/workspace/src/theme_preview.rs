#![allow(unused, dead_code)]
use gpui::{
    AnyElement, App, Entity, EventEmitter, FocusHandle, Focusable, Hsla, Task, actions, hsla,
};
use strum::IntoEnumIterator;
use theme::all_theme_colors;
use ui::{
    Avatar, ButtonLike, Checkbox, DecoratedIcon, ElevationIndex, IconDecoration, Indicator,
    KeybindingHint, Switch, TintColor, Tooltip, prelude::*, utils::calculate_contrast_ratio,
};

use crate::{Item, Workspace};

actions!(
    dev,
    [
        /// Opens the theme preview window.
        OpenThemePreview
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &OpenThemePreview, window, cx| {
            let theme_preview = cx.new(|cx| ThemePreview::new(window, cx));
            workspace.add_item_to_active_pane(Box::new(theme_preview), None, true, window, cx)
        });
    })
    .detach();
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, strum::EnumIter)]
enum ThemePreviewPage {
    Overview,
    Typography,
}

impl ThemePreviewPage {
    pub fn identifier(&self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Typography => "Typography",
        }
    }

    pub fn title(&self, cx: &App) -> SharedString {
        match self {
            Self::Overview => localization::text(cx, "workspace-theme-overview"),
            Self::Typography => localization::text(cx, "workspace-theme-typography"),
        }
    }
}

struct ThemePreview {
    current_page: ThemePreviewPage,
    focus_handle: FocusHandle,
}

impl ThemePreview {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            current_page: ThemePreviewPage::Overview,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn view(
        &self,
        page: ThemePreviewPage,
        window: &mut Window,
        cx: &mut Context<ThemePreview>,
    ) -> impl IntoElement {
        match page {
            ThemePreviewPage::Overview => self.render_overview_page(window, cx).into_any_element(),
            ThemePreviewPage::Typography => {
                self.render_typography_page(window, cx).into_any_element()
            }
        }
    }
}

impl EventEmitter<()> for ThemePreview {}

impl Focusable for ThemePreview {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}
impl ThemePreview {}

impl Item for ThemePreview {
    type Event = ();

    fn to_item_events(_: &Self::Event, _: &mut dyn FnMut(crate::item::ItemEvent)) {}

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        let name = cx.theme().name.clone();
        localization::tr!(cx, "workspace-theme-tab", theme = name.as_ref())
    }

    fn can_split(&self) -> bool {
        true
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<crate::WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>>
    where
        Self: Sized,
    {
        Task::ready(Some(cx.new(|cx| Self::new(window, cx))))
    }
}

const AVATAR_URL: &str = "https://avatars.githubusercontent.com/u/1714999?v=4";

impl ThemePreview {
    fn preview_bg(window: &mut Window, cx: &mut App) -> Hsla {
        cx.theme().colors().editor_background
    }

    fn render_text(
        &self,
        layer: ElevationIndex,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bg = layer.bg(cx);

        let label_with_contrast = |label: SharedString, fg: Hsla| {
            let contrast = calculate_contrast_ratio(fg, bg);
            format!("{} ({:.2})", label, contrast)
        };

        v_flex()
            .gap_1()
            .child(
                Headline::new(localization::text(cx, "workspace-theme-text"))
                    .size(HeadlineSize::Small)
                    .color(Color::Muted),
            )
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Headline::new(localization::text(
                                    cx,
                                    "workspace-theme-headline-sizes",
                                ))
                                .size(HeadlineSize::Small)
                                .color(Color::Muted),
                            )
                            .child(
                                Headline::new(localization::text(
                                    cx,
                                    "workspace-theme-headline-xlarge",
                                ))
                                .size(HeadlineSize::XLarge),
                            )
                            .child(
                                Headline::new(localization::text(
                                    cx,
                                    "workspace-theme-headline-large",
                                ))
                                .size(HeadlineSize::Large),
                            )
                            .child(
                                Headline::new(localization::text(
                                    cx,
                                    "workspace-theme-headline-medium",
                                ))
                                .size(HeadlineSize::Medium),
                            )
                            .child(
                                Headline::new(localization::text(
                                    cx,
                                    "workspace-theme-headline-small",
                                ))
                                .size(HeadlineSize::Small),
                            )
                            .child(
                                Headline::new(localization::text(
                                    cx,
                                    "workspace-theme-headline-xsmall",
                                ))
                                .size(HeadlineSize::XSmall),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Headline::new(localization::text(
                                    cx,
                                    "workspace-theme-text-colors",
                                ))
                                .size(HeadlineSize::Small)
                                .color(Color::Muted),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-default"),
                                    Color::Default.color(cx),
                                ))
                                .color(Color::Default),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-accent"),
                                    Color::Accent.color(cx),
                                ))
                                .color(Color::Accent),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-conflict"),
                                    Color::Conflict.color(cx),
                                ))
                                .color(Color::Conflict),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-created"),
                                    Color::Created.color(cx),
                                ))
                                .color(Color::Created),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-deleted"),
                                    Color::Deleted.color(cx),
                                ))
                                .color(Color::Deleted),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-disabled"),
                                    Color::Disabled.color(cx),
                                ))
                                .color(Color::Disabled),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-error"),
                                    Color::Error.color(cx),
                                ))
                                .color(Color::Error),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-hidden"),
                                    Color::Hidden.color(cx),
                                ))
                                .color(Color::Hidden),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-hint"),
                                    Color::Hint.color(cx),
                                ))
                                .color(Color::Hint),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-ignored"),
                                    Color::Ignored.color(cx),
                                ))
                                .color(Color::Ignored),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-info"),
                                    Color::Info.color(cx),
                                ))
                                .color(Color::Info),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-modified"),
                                    Color::Modified.color(cx),
                                ))
                                .color(Color::Modified),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-muted"),
                                    Color::Muted.color(cx),
                                ))
                                .color(Color::Muted),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-placeholder"),
                                    Color::Placeholder.color(cx),
                                ))
                                .color(Color::Placeholder),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-selected"),
                                    Color::Selected.color(cx),
                                ))
                                .color(Color::Selected),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-success"),
                                    Color::Success.color(cx),
                                ))
                                .color(Color::Success),
                            )
                            .child(
                                Label::new(label_with_contrast(
                                    localization::text(cx, "workspace-theme-color-warning"),
                                    Color::Warning.color(cx),
                                ))
                                .color(Color::Warning),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Headline::new(localization::text(
                                    cx,
                                    "workspace-theme-wrapping-heading",
                                ))
                                .size(HeadlineSize::Small)
                                .color(Color::Muted),
                            )
                            .child(
                                div()
                                    .max_w(px(200.))
                                    .child(localization::text(cx, "workspace-theme-wrapping-text")),
                            ),
                    ),
            )
    }

    fn render_colors(
        &self,
        layer: ElevationIndex,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bg = layer.bg(cx);
        let all_colors = all_theme_colors(cx);

        v_flex()
            .gap_1()
            .child(
                Headline::new(localization::text(cx, "workspace-theme-colors"))
                    .size(HeadlineSize::Small)
                    .color(Color::Muted),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .children(all_colors.into_iter().map(|(color, name)| {
                        let id = ElementId::Name(format!("{:?}-preview", color).into());
                        div().size_8().flex_none().child(
                            ButtonLike::new(id)
                                .child(
                                    div()
                                        .size_8()
                                        .bg(color)
                                        .border_1()
                                        .border_color(cx.theme().colors().border)
                                        .overflow_hidden(),
                                )
                                .size(ButtonSize::None)
                                .style(ButtonStyle::Transparent)
                                .tooltip(move |window, cx| {
                                    let name = name.clone();
                                    Tooltip::with_meta(name, None, format!("{:?}", color), cx)
                                }),
                        )
                    })),
            )
    }

    fn render_theme_layer(
        &self,
        layer: ElevationIndex,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .p_4()
            .bg(layer.bg(cx))
            .text_color(cx.theme().colors().text)
            .gap_2()
            .child(Headline::new(layer.clone().to_string()).size(HeadlineSize::Medium))
            .child(self.render_text(layer, window, cx))
            .child(self.render_colors(layer, window, cx))
    }

    fn render_overview_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .id("theme-preview-overview")
            .overflow_scroll()
            .size_full()
            .child(
                v_flex()
                    .child(
                        Headline::new(localization::text(cx, "workspace-theme-preview-title"))
                            .size(HeadlineSize::Large),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_color(cx.theme().colors().text_muted)
                            .child(localization::text(
                                cx,
                                "workspace-theme-preview-description",
                            )),
                    ),
            )
            .child(self.render_theme_layer(ElevationIndex::Background, window, cx))
            .child(self.render_theme_layer(ElevationIndex::Surface, window, cx))
            .child(self.render_theme_layer(ElevationIndex::EditorSurface, window, cx))
            .child(self.render_theme_layer(ElevationIndex::ElevatedSurface, window, cx))
    }

    fn render_typography_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .id("theme-preview-typography")
            .overflow_scroll()
            .size_full()
            .child(
                v_flex()
                    .gap_4()
                    .child(
                        Headline::new(localization::text(cx, "workspace-theme-heading-one"))
                            .size(HeadlineSize::XLarge),
                    )
                    .child(Label::new(localization::text(
                        cx,
                        "workspace-theme-sample-one",
                    )))
                    .child(
                        Headline::new(localization::text(cx, "workspace-theme-heading-two"))
                            .size(HeadlineSize::Large),
                    )
                    .child(Label::new(localization::text(
                        cx,
                        "workspace-theme-sample-two",
                    )))
                    .child(
                        Headline::new(localization::text(cx, "workspace-theme-heading-three"))
                            .size(HeadlineSize::Medium),
                    )
                    .child(Label::new(localization::text(
                        cx,
                        "workspace-theme-sample-three",
                    )))
                    .child(
                        Headline::new(localization::text(cx, "workspace-theme-heading-four"))
                            .size(HeadlineSize::Small),
                    )
                    .child(Label::new(localization::text(
                        cx,
                        "workspace-theme-sample-four",
                    )))
                    .child(
                        Headline::new(localization::text(cx, "workspace-theme-heading-five"))
                            .size(HeadlineSize::XSmall),
                    )
                    .child(Label::new(localization::text(
                        cx,
                        "workspace-theme-sample-five",
                    )))
                    .child(
                        Headline::new(localization::text(cx, "workspace-theme-body-text"))
                            .size(HeadlineSize::Small),
                    )
                    .child(Label::new(localization::text(
                        cx,
                        "workspace-theme-sample-body",
                    ))),
            )
    }

    fn render_page_nav(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("theme-preview-nav")
            .items_center()
            .gap_4()
            .py_2()
            .bg(Self::preview_bg(window, cx))
            .children(ThemePreviewPage::iter().map(|p| {
                Button::new(ElementId::Name(p.identifier().into()), p.title(cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.current_page = p;
                        cx.notify();
                    }))
                    .toggle_state(p == self.current_page)
                    .selected_style(ButtonStyle::Tinted(TintColor::Accent))
            }))
    }
}

impl Render for ThemePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl ui::IntoElement {
        v_flex()
            .id("theme-preview")
            .key_context("ThemePreview")
            .items_start()
            .overflow_hidden()
            .size_full()
            .max_h_full()
            .track_focus(&self.focus_handle)
            .px_2()
            .bg(Self::preview_bg(window, cx))
            .child(self.render_page_nav(window, cx))
            .child(self.view(self.current_page, window, cx))
    }
}
