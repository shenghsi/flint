#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("The Flint visual test runner is only available on macOS.");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
use {
    anyhow::{Context as _, Result, ensure},
    assets::Assets,
    gpui::{
        AppContext as _, HeadlessAppContext, IntoElement, ParentElement, Render, Styled, Window,
        div, px, rgb, size,
    },
    localization::UiLanguage,
    std::{path::PathBuf, sync::Arc},
};

#[cfg(target_os = "macos")]
fn main() {
    if let Err(error) = run() {
        eprintln!("Flint visual test runner failed: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
struct ChineseLocalizationPreview;

#[cfg(target_os = "macos")]
impl Render for ChineseLocalizationPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x000000))
            .child(localization::text(cx, "menu-flint"))
            .child(localization::text(cx, "menu-settings"))
            .child(localization::text(cx, "panel-agent-threads"))
            .child(localization::text(cx, "common-copy"))
    }
}

#[cfg(target_os = "macos")]
fn run() -> Result<()> {
    let output_directory = std::env::var_os("VISUAL_TEST_OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/visual_tests"));
    std::fs::create_dir_all(&output_directory).context("creating visual test output directory")?;

    let platform = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        platform.text_system(),
        Arc::new(Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        Assets.load_fonts(cx)?;
        localization::init(UiLanguage::SimplifiedChinese, cx)
    })?;

    for (name, width) in [("narrow", 320.), ("wide", 1280.)] {
        let window = cx.open_window(size(px(width), px(240.)), |_, cx| {
            cx.new(|_| ChineseLocalizationPreview)
        })?;
        cx.run_until_parked();

        let screenshot = cx
            .capture_screenshot(window.into())
            .with_context(|| format!("capturing {name} Chinese localization preview"))?;
        ensure!(
            screenshot.width() > 0 && screenshot.height() > 0,
            "{name} Chinese localization preview has no pixels"
        );
        ensure!(
            screenshot
                .pixels()
                .any(|pixel| pixel.0[..3] != [0xff, 0xff, 0xff]),
            "{name} Chinese localization preview has no visible content"
        );
        screenshot
            .save(output_directory.join(format!("chinese-localization-{name}.png")))
            .with_context(|| format!("saving {name} Chinese localization preview"))?;
    }

    Ok(())
}
