use collections::HashMap;
use gpui::{AnyElement, App, Context, ImageSource, RenderImage, Task, img};
use latex_render::{LatexRenderer, RenderRequest};
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use theme_settings::ThemeSettings;
use ui::prelude::*;

use crate::parser::MarkdownEvent;
use settings::Settings as _;

use super::{Markdown, MarkdownStyle, ParsedMarkdown};

type MathEquationCache = HashMap<ParsedMarkdownMathEquationContents, Arc<CachedMathEquation>>;

/// Pixels-per-`ex` is derived from the markdown body font size. MathJax sizes
/// math relative to the surrounding text's x-height, which is roughly half the
/// font size for typical fonts.
const EX_TO_FONT_SIZE_RATIO: f32 = 0.5;

#[derive(Clone, Debug)]
pub(crate) struct ParsedMarkdownMathEquation {
    pub(crate) contents: ParsedMarkdownMathEquationContents,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ParsedMarkdownMathEquationContents {
    pub(crate) tex: SharedString,
    pub(crate) display: bool,
}

#[derive(Default, Clone)]
pub(crate) struct MathState {
    cache: MathEquationCache,
}

#[derive(Clone)]
pub(crate) struct RenderedMathImage {
    pub(crate) image: Arc<RenderImage>,
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) vertical_align: f32,
}

struct CachedMathEquation {
    rendered: Arc<OnceLock<anyhow::Result<RenderedMathImage>>>,
    _task: Task<()>,
}

impl MathState {
    pub(crate) fn clear(&mut self) {
        self.cache.clear();
    }

    pub(crate) fn update(&mut self, parsed: &ParsedMarkdown, cx: &mut Context<Markdown>) {
        let mut wanted = HashSet::new();
        for equation in parsed.math_equations.values() {
            wanted.insert(equation.contents.clone());
            if !self.cache.contains_key(&equation.contents) {
                self.cache.insert(
                    equation.contents.clone(),
                    Arc::new(CachedMathEquation::new(equation.contents.clone(), cx)),
                );
            }
        }
        self.cache.retain(|contents, _| wanted.contains(contents));
    }

    fn rendered(&self, contents: &ParsedMarkdownMathEquationContents) -> Option<RenderedMathImage> {
        self.cache
            .get(contents)?
            .rendered
            .get()?
            .as_ref()
            .ok()
            .cloned()
    }
}

impl CachedMathEquation {
    fn new(contents: ParsedMarkdownMathEquationContents, cx: &mut Context<Markdown>) -> Self {
        let rendered = Arc::new(OnceLock::<anyhow::Result<RenderedMathImage>>::new());
        let rendered_clone = rendered.clone();
        let svg_renderer = cx.svg_renderer();
        let renderer = LatexRenderer::try_global(cx);
        let color = math_color(cx);
        let ex_px = math_ex_px(cx);

        let task = cx.spawn(async move |this, cx| {
            let value = cx
                .background_spawn(async move {
                    let renderer =
                        renderer.ok_or_else(|| anyhow::anyhow!("latex renderer unavailable"))?;
                    let math = renderer
                        .render(RenderRequest {
                            tex: &contents.tex,
                            display: contents.display,
                            color: Some(&color),
                            ex_px,
                        })
                        .await?;
                    let image = svg_renderer
                        .render_single_frame(math.svg.as_bytes(), 1.0)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    anyhow::Ok(RenderedMathImage {
                        image,
                        width: math.width,
                        height: math.height,
                        vertical_align: math.vertical_align,
                    })
                })
                .await;
            let _ = rendered_clone.set(value);
            this.update(cx, |_, cx| {
                cx.notify();
            })
            .ok();
        });

        Self {
            rendered,
            _task: task,
        }
    }
}

fn math_color(cx: &App) -> String {
    let color = gpui::Rgba::from(cx.theme().colors().text);
    format!(
        "#{:02x}{:02x}{:02x}",
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8,
    )
}

fn math_ex_px(cx: &App) -> f32 {
    let font_size = ThemeSettings::get_global(cx).buffer_font_size(cx);
    f32::from(font_size) * EX_TO_FONT_SIZE_RATIO
}

pub(crate) fn extract_math_equations(
    events: &[(Range<usize>, MarkdownEvent)],
) -> BTreeMap<usize, ParsedMarkdownMathEquation> {
    let mut equations = BTreeMap::default();
    for (range, event) in events {
        let (tex, display) = match event {
            MarkdownEvent::DisplayMath(tex) => (tex, true),
            MarkdownEvent::InlineMath(tex) => (tex, false),
            _ => continue,
        };
        equations.insert(
            range.start,
            ParsedMarkdownMathEquation {
                contents: ParsedMarkdownMathEquationContents {
                    tex: tex.clone().into(),
                    display,
                },
            },
        );
    }
    equations
}

/// Renders display (block) math as a centered image, or `None` while the
/// equation is still rendering or if rendering failed (the caller falls back to
/// the source text).
pub(crate) fn render_display_math(
    equation: &ParsedMarkdownMathEquation,
    math_state: &MathState,
    style: &MarkdownStyle,
) -> Option<AnyElement> {
    let rendered = math_state.rendered(&equation.contents)?;
    let mut container = div().w_full().flex().justify_center().my_2();
    container.style().refine(&style.code_block);
    Some(
        container
            .child(img(ImageSource::Render(rendered.image)).max_w_full())
            .into_any_element(),
    )
}

/// Returns the rendered inline equation image, if ready. The caller is
/// responsible for placing it inline and falling back to source text otherwise.
pub(crate) fn rendered_inline_math(
    equation: &ParsedMarkdownMathEquation,
    math_state: &MathState,
) -> Option<RenderedMathImage> {
    math_state.rendered(&equation.contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_markdown_with_options;

    #[test]
    fn test_extract_math_equations() {
        let markdown = "Inline $x^2$ and\n\n$$E = mc^2$$";
        let events = parse_markdown_with_options(markdown, false, false, false, true).events;
        let equations = extract_math_equations(&events);
        assert_eq!(equations.len(), 2);

        // Ordered by source offset: the inline equation precedes the display one.
        let mut values = equations.values();
        let inline = values.next().unwrap();
        assert_eq!(inline.contents.tex.as_ref(), "x^2");
        assert!(!inline.contents.display);

        let display = values.next().unwrap();
        assert!(display.contents.tex.contains("E = mc^2"));
        assert!(display.contents.display);
    }

    #[test]
    fn test_no_math_extracted_when_disabled() {
        let markdown = "These cost $5 and $10.";
        let events = parse_markdown_with_options(markdown, false, false, false, false).events;
        assert!(extract_math_equations(&events).is_empty());
    }
}
