use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct MarkdownSettingsContent {
    /// How Markdown files are displayed when opened.
    pub open_mode: Option<MarkdownOpenMode>,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom,
)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownOpenMode {
    /// Open Markdown files with inline formatting while retaining editable source.
    #[default]
    EditableRendered,
    /// Open Markdown files as plain source.
    Source,
}
