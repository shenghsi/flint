use std::path::PathBuf;

use collections::HashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct TerminalThreadSettingsContent {
    /// Command used for new Codex terminal threads.
    pub codex: Option<TerminalThreadCommandContent>,
    /// Command used for new Claude terminal threads.
    pub claude: Option<TerminalThreadCommandContent>,
    /// Optional command override used for new shell terminal threads.
    pub shell: Option<TerminalThreadCommandContent>,
}

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct TerminalThreadCommandContent {
    /// Program to execute.
    pub command: Option<String>,
    /// Arguments passed to the program.
    pub args: Option<Vec<String>>,
    /// Environment overrides added to the terminal process.
    pub env: Option<HashMap<String, String>>,
    /// Working directory override for this thread kind.
    pub cwd: Option<PathBuf>,
}
