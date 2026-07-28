//! Persists each picker's last preview layout (hidden / right / below) so a
//! preview's placement survives across sessions. The preview's draggable size
//! is per-session.

use anyhow::{Context, anyhow};
use db::kvp::KeyValueStore;
use gpui::App;

use crate::preview;

const PICKERS_NAMESPACE: &str = "pickers";

pub(crate) fn store_layout(
    picker_delegate: &'static str,
    layout: Option<preview::Layout>,
    cx: &App,
) {
    let kvp = KeyValueStore::global(cx);
    let key = layout_key(picker_delegate);
    let value = layout_as_str(layout).to_string();
    db::write_and_log(cx, async move || {
        kvp.scoped(PICKERS_NAMESPACE).write(key, value).await
    });
}

pub(crate) fn load_layout(
    picker_delegate: &'static str,
    cx: &App,
) -> anyhow::Result<Option<preview::Layout>> {
    let Some(last_layout) = KeyValueStore::global(cx)
        .scoped(PICKERS_NAMESPACE)
        .read(&layout_key(picker_delegate))
        .context("Could not read last picker layout from KeyValueStore")?
    else {
        return Ok(None);
    };

    parse_layout(&last_layout)
}

fn layout_key(picker_delegate: &'static str) -> String {
    format!("{picker_delegate}/LAST_PREVIEW_LAYOUT")
}

fn layout_as_str(layout: Option<preview::Layout>) -> &'static str {
    match layout {
        Some(preview::Layout::Hidden) => "hidden",
        Some(preview::Layout::Below) => "below",
        Some(preview::Layout::Right) => "right",
        None => "none",
    }
}

fn parse_layout(s: &str) -> anyhow::Result<Option<preview::Layout>> {
    Ok(Some(match s {
        "hidden" => preview::Layout::Hidden,
        "below" => preview::Layout::Below,
        "right" => preview::Layout::Right,
        "none" => return Ok(None),
        _ => return Err(anyhow!("Unknown layout: `{}`", s)),
    }))
}
