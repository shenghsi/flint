use super::*;
use markdown::{
    CodeBlockRenderer, CopyButtonVisibility, Markdown, MarkdownElement, MarkdownFont,
    MarkdownOptions, MarkdownStyle, WrapButtonVisibility,
    parser::{CodeBlockKind, MarkdownEvent, MarkdownTag, parse_markdown_events},
};
use settings::RegisterSetting;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownViewMode {
    EditableRendered,
    Source,
}

#[derive(Clone, Debug, RegisterSetting)]
pub struct MarkdownSettings {
    pub open_mode: MarkdownViewMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkdownInlineStyle {
    BlockQuote,
    Code,
    Emphasis,
    Heading,
    Link,
    Strikethrough,
    Strong,
    Syntax,
}

struct ActiveMarkdownTag {
    source_range: Range<usize>,
    content_range: Option<Range<usize>>,
    style: Option<MarkdownInlineStyle>,
    fade_outer_syntax: bool,
}

#[derive(Clone, Copy)]
enum MarkdownRichBlockKind {
    Table,
    Image,
    Mermaid,
}

impl Settings for MarkdownSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let open_mode = content
            .markdown
            .as_ref()
            .and_then(|markdown| markdown.open_mode)
            .unwrap_or_default();

        Self {
            open_mode: match open_mode {
                settings::MarkdownOpenMode::EditableRendered => MarkdownViewMode::EditableRendered,
                settings::MarkdownOpenMode::Source => MarkdownViewMode::Source,
            },
        }
    }
}

impl Editor {
    pub fn markdown_view_mode(&self) -> Option<MarkdownViewMode> {
        self.is_markdown_document.then_some(self.markdown_view_mode)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn markdown_rich_block_count(&self) -> usize {
        self.markdown_rich_block_ids.len()
    }

    pub fn show_rendered_markdown(
        &mut self,
        _: &ShowRendered,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_markdown_view_mode(MarkdownViewMode::EditableRendered, cx);
    }

    pub fn show_markdown_source(&mut self, _: &ShowSource, _: &mut Window, cx: &mut Context<Self>) {
        self.set_markdown_view_mode(MarkdownViewMode::Source, cx);
    }

    pub fn toggle_rendered_markdown(
        &mut self,
        _: &ToggleRendered,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = match self.markdown_view_mode {
            MarkdownViewMode::EditableRendered => MarkdownViewMode::Source,
            MarkdownViewMode::Source => MarkdownViewMode::EditableRendered,
        };
        self.set_markdown_view_mode(mode, cx);
    }

    fn set_markdown_view_mode(&mut self, mode: MarkdownViewMode, cx: &mut Context<Self>) {
        if !self.is_markdown_document || self.markdown_view_mode == mode {
            return;
        }

        self.markdown_view_mode = mode;
        self.markdown_view_mode_overridden = true;
        self.refresh_markdown_presentation(cx);
        cx.notify();
    }

    pub(super) fn refresh_markdown_view_mode(&mut self, cx: &mut Context<Self>) {
        self.is_markdown_document = self.buffer.read(cx).as_singleton().is_some_and(|buffer| {
            let buffer = buffer.read(cx);
            buffer
                .language()
                .is_some_and(|language| language.name() == "Markdown")
                || buffer.file().is_some_and(|file| {
                    Path::new(file.file_name(cx))
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            matches!(
                                extension.to_ascii_lowercase().as_str(),
                                "md" | "markdown" | "mdown" | "mkd" | "mkdn"
                            )
                        })
                })
        });

        if !self.markdown_view_mode_overridden {
            self.markdown_view_mode = MarkdownSettings::get_global(cx).open_mode;
        }

        self.refresh_markdown_presentation(cx);
    }

    pub(super) fn refresh_markdown_presentation(&mut self, cx: &mut Context<Self>) {
        self.clear_markdown_presentation(cx);
        if !self.is_markdown_document
            || self.markdown_view_mode != MarkdownViewMode::EditableRendered
        {
            return;
        }

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let source = snapshot.text();
        let events = parse_markdown_events(&source);
        let mut active_tags: Vec<ActiveMarkdownTag> = Vec::new();
        let mut styled_ranges: Vec<(MarkdownInlineStyle, Range<usize>)> = Vec::new();
        let mut rich_blocks: Vec<(Range<usize>, MarkdownRichBlockKind)> = Vec::new();

        for (source_range, event) in events {
            match event {
                MarkdownEvent::Start(tag) => {
                    let (style, fade_outer_syntax) = match tag {
                        MarkdownTag::Table(_) => {
                            rich_blocks.push((source_range.clone(), MarkdownRichBlockKind::Table));
                            (None, false)
                        }
                        MarkdownTag::Image { .. } => {
                            rich_blocks.push((source_range.clone(), MarkdownRichBlockKind::Image));
                            (None, false)
                        }
                        MarkdownTag::CodeBlock { kind, metadata }
                            if metadata.is_fenced_closed && is_mermaid_code_block(&kind) =>
                        {
                            rich_blocks
                                .push((source_range.clone(), MarkdownRichBlockKind::Mermaid));
                            (Some(MarkdownInlineStyle::Code), true)
                        }
                        MarkdownTag::Heading { .. } => (Some(MarkdownInlineStyle::Heading), true),
                        MarkdownTag::BlockQuote(_) => (Some(MarkdownInlineStyle::BlockQuote), true),
                        MarkdownTag::CodeBlock { .. } => (Some(MarkdownInlineStyle::Code), true),
                        MarkdownTag::Item => (None, true),
                        MarkdownTag::Emphasis => (Some(MarkdownInlineStyle::Emphasis), true),
                        MarkdownTag::Strong => (Some(MarkdownInlineStyle::Strong), true),
                        MarkdownTag::Strikethrough => {
                            (Some(MarkdownInlineStyle::Strikethrough), true)
                        }
                        MarkdownTag::Link { .. } => (Some(MarkdownInlineStyle::Link), true),
                        _ => (None, false),
                    };
                    active_tags.push(ActiveMarkdownTag {
                        source_range,
                        content_range: None,
                        style,
                        fade_outer_syntax,
                    });
                }
                MarkdownEvent::End(_) => {
                    let Some(active_tag) = active_tags.pop() else {
                        continue;
                    };
                    let Some(content_range) = active_tag.content_range else {
                        continue;
                    };

                    if let Some(style) = active_tag.style {
                        styled_ranges.push((style, content_range.clone()));
                    }
                    if active_tag.fade_outer_syntax {
                        if active_tag.source_range.start < content_range.start {
                            styled_ranges.push((
                                MarkdownInlineStyle::Syntax,
                                active_tag.source_range.start..content_range.start,
                            ));
                        }
                        if content_range.end < active_tag.source_range.end {
                            styled_ranges.push((
                                MarkdownInlineStyle::Syntax,
                                content_range.end..active_tag.source_range.end,
                            ));
                        }
                    }
                }
                MarkdownEvent::Text => {
                    for active_tag in &mut active_tags {
                        match &mut active_tag.content_range {
                            Some(content_range) => {
                                content_range.start = content_range.start.min(source_range.start);
                                content_range.end = content_range.end.max(source_range.end);
                            }
                            None => active_tag.content_range = Some(source_range.clone()),
                        }
                    }
                }
                MarkdownEvent::Code => {
                    for active_tag in &mut active_tags {
                        match &mut active_tag.content_range {
                            Some(content_range) => {
                                content_range.start = content_range.start.min(source_range.start);
                                content_range.end = content_range.end.max(source_range.end);
                            }
                            None => active_tag.content_range = Some(source_range.clone()),
                        }
                    }
                    styled_ranges.push((MarkdownInlineStyle::Code, source_range));
                }
                _ => {}
            }
        }

        for style in [
            MarkdownInlineStyle::BlockQuote,
            MarkdownInlineStyle::Code,
            MarkdownInlineStyle::Emphasis,
            MarkdownInlineStyle::Heading,
            MarkdownInlineStyle::Link,
            MarkdownInlineStyle::Strikethrough,
            MarkdownInlineStyle::Strong,
            MarkdownInlineStyle::Syntax,
        ] {
            let ranges = styled_ranges
                .iter()
                .filter(|(range_style, _)| *range_style == style)
                .map(|(_, range)| {
                    snapshot.anchor_before(MultiBufferOffset(range.start))
                        ..snapshot.anchor_after(MultiBufferOffset(range.end))
                })
                .collect();
            let (key, highlight_style) = markdown_highlight_style(style, cx);
            self.highlight_text(key, ranges, highlight_style, cx);
        }
        self.insert_markdown_rich_blocks(&source, rich_blocks, &snapshot, cx);
    }

    fn clear_markdown_presentation(&mut self, cx: &mut Context<Self>) {
        self.clear_markdown_rich_blocks(cx);
        for key in [
            HighlightKey::MarkdownBlockQuote,
            HighlightKey::MarkdownCode,
            HighlightKey::MarkdownEmphasis,
            HighlightKey::MarkdownHeading,
            HighlightKey::MarkdownLink,
            HighlightKey::MarkdownStrikethrough,
            HighlightKey::MarkdownStrong,
            HighlightKey::MarkdownSyntax,
        ] {
            self.clear_highlights(key, cx);
        }
    }

    fn clear_markdown_rich_blocks(&mut self, cx: &mut Context<Self>) {
        if self.markdown_rich_block_ids.is_empty() {
            return;
        }

        let block_ids = std::mem::take(&mut self.markdown_rich_block_ids);
        self.remove_blocks(block_ids, None, cx);
    }

    fn insert_markdown_rich_blocks(
        &mut self,
        source: &str,
        rich_blocks: Vec<(Range<usize>, MarkdownRichBlockKind)>,
        snapshot: &multi_buffer::MultiBufferSnapshot,
        cx: &mut Context<Self>,
    ) {
        if rich_blocks.is_empty() {
            return;
        }

        let blocks = rich_blocks
            .into_iter()
            .map(|(range, kind)| {
                let markdown = cx.new(|cx| {
                    Markdown::new_with_options(
                        SharedString::from(source[range.clone()].to_string()),
                        None,
                        None,
                        MarkdownOptions {
                            parse_html: true,
                            render_mermaid_diagrams: true,
                            parse_heading_slugs: false,
                            render_metadata_blocks: true,
                            ..Default::default()
                        },
                        cx,
                    )
                });
                BlockProperties {
                    placement: BlockPlacement::Below(
                        snapshot.anchor_after(MultiBufferOffset(range.end)),
                    ),
                    height: Some(rich_block_height(&source[range], kind)),
                    style: BlockStyle::Flex,
                    render: render_markdown_rich_block(markdown),
                    priority: 0,
                }
            })
            .collect::<Vec<_>>();

        self.markdown_rich_block_ids = self.insert_blocks(blocks, None, cx).into_iter().collect();
    }

    pub fn toggle_markdown_block_quote(
        &mut self,
        _: &ToggleBlockQuote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_mutable_lines_in_markdown(window, cx, |lines| {
            let all_lines_quoted = lines.iter().all(|line| line.starts_with('>'));

            for line in lines.iter_mut() {
                let stripped_line = match line.strip_prefix("> ").or_else(|| line.strip_prefix('>'))
                {
                    Some(rest) => rest.to_string(),
                    None => line.to_string(),
                };

                *line = if all_lines_quoted {
                    Cow::Owned(stripped_line)
                } else if stripped_line.trim().is_empty() {
                    Cow::Borrowed(">")
                } else {
                    Cow::Owned(format!("> {stripped_line}"))
                };
            }
        });
    }

    fn manipulate_mutable_lines_in_markdown<Fn>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        callback: Fn,
    ) where
        Fn: FnMut(&mut Vec<Cow<'_, str>>),
    {
        if !self.is_in_markdown_language(cx) {
            return;
        }

        self.manipulate_mutable_lines(window, cx, callback);
    }

    fn is_in_markdown_language(&self, cx: &mut App) -> bool {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let head = self
            .selections
            .newest::<MultiBufferOffset>(&self.display_snapshot(cx))
            .head();
        snapshot
            .language_at(head)
            .is_some_and(|language| language.name() == "Markdown")
    }
}

fn is_mermaid_code_block(kind: &CodeBlockKind) -> bool {
    match kind {
        CodeBlockKind::FencedLang(language) => {
            language.as_ref().trim().eq_ignore_ascii_case("mermaid")
        }
        CodeBlockKind::FencedSrc(path_range) => std::path::Path::new(path_range.path.as_ref())
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "mermaid" | "mmd")),
        _ => false,
    }
}

fn rich_block_height(source: &str, kind: MarkdownRichBlockKind) -> u32 {
    let source_lines = source.lines().count() as u32;
    match kind {
        MarkdownRichBlockKind::Table => source_lines.saturating_add(2).clamp(3, 12),
        MarkdownRichBlockKind::Image => 12,
        MarkdownRichBlockKind::Mermaid => source_lines.saturating_add(4).clamp(6, 18),
    }
}

fn render_markdown_rich_block(markdown: Entity<Markdown>) -> RenderBlock {
    Arc::new(move |block_cx| {
        let style = MarkdownStyle::themed(MarkdownFont::Editor, block_cx.window, block_cx.app);
        div()
            .px_2()
            .py_1()
            .child(
                MarkdownElement::new(markdown.clone(), style).code_block_renderer(
                    CodeBlockRenderer::Default {
                        copy_button_visibility: CopyButtonVisibility::VisibleOnHover,
                        wrap_button_visibility: WrapButtonVisibility::Hidden,
                        border: true,
                    },
                ),
            )
            .into_any_element()
    })
}

fn markdown_highlight_style(
    style: MarkdownInlineStyle,
    cx: &App,
) -> (HighlightKey, HighlightStyle) {
    match style {
        MarkdownInlineStyle::BlockQuote => (
            HighlightKey::MarkdownBlockQuote,
            HighlightStyle {
                color: Some(cx.theme().colors().text_muted),
                ..Default::default()
            },
        ),
        MarkdownInlineStyle::Code => (
            HighlightKey::MarkdownCode,
            HighlightStyle {
                background_color: Some(cx.theme().colors().background),
                ..Default::default()
            },
        ),
        MarkdownInlineStyle::Emphasis => (
            HighlightKey::MarkdownEmphasis,
            HighlightStyle {
                font_style: Some(FontStyle::Italic),
                ..Default::default()
            },
        ),
        MarkdownInlineStyle::Heading => (
            HighlightKey::MarkdownHeading,
            HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
        ),
        MarkdownInlineStyle::Link => (
            HighlightKey::MarkdownLink,
            HighlightStyle {
                color: Some(cx.theme().colors().link_text_hover),
                underline: Some(UnderlineStyle {
                    thickness: px(1.),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        MarkdownInlineStyle::Strikethrough => (
            HighlightKey::MarkdownStrikethrough,
            HighlightStyle {
                strikethrough: Some(gpui::StrikethroughStyle {
                    thickness: px(1.),
                    color: None,
                }),
                ..Default::default()
            },
        ),
        MarkdownInlineStyle::Strong => (
            HighlightKey::MarkdownStrong,
            HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
        ),
        MarkdownInlineStyle::Syntax => (
            HighlightKey::MarkdownSyntax,
            HighlightStyle {
                fade_out: Some(0.45),
                ..Default::default()
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_open_mode_defaults_to_editable_rendered() {
        let settings = MarkdownSettings::from_settings(&settings::SettingsContent::default());

        assert_eq!(settings.open_mode, MarkdownViewMode::EditableRendered);
    }

    #[test]
    fn markdown_open_mode_can_default_to_source() {
        let mut content = settings::SettingsContent::default();
        content.markdown = Some(settings::MarkdownSettingsContent {
            open_mode: Some(settings::MarkdownOpenMode::Source),
        });

        let settings = MarkdownSettings::from_settings(&content);

        assert_eq!(settings.open_mode, MarkdownViewMode::Source);
    }
}
