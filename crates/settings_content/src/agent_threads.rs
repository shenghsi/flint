use std::path::PathBuf;

use collections::HashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct AgentThreadSettingsContent {
    /// Command used for new Codex agent threads.
    pub codex: Option<AgentThreadCommandContent>,
    /// Command used for new Claude agent threads.
    pub claude: Option<AgentThreadCommandContent>,
    /// Maximum number of threads shown per agent section before a "Show
    /// more" control is offered.
    ///
    /// Default: 5
    pub max_visible_threads_per_agent: Option<usize>,
    /// Whether to show five-hour and weekly plan usage in section headings.
    ///
    /// Default: true
    pub show_plan_usage: Option<bool>,
    /// When to reopen live resumed agent sessions from the previous app session.
    ///
    /// Default: never
    pub reopen_sessions_on_startup: Option<AgentThreadReopenSessionsOnStartup>,
    /// Where to dock the agent threads panel.
    ///
    /// Default: left
    pub dock: Option<crate::DockSide>,
}

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    Eq,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentThreadReopenSessionsOnStartup {
    /// Do not reopen agent sessions.
    #[default]
    Never,
    /// Reopen sessions only when restoring workspaces from the previous app session.
    StartupRestore,
    /// Reopen sessions when any matching workspace from the previous app session opens.
    MatchingWorkspace,
}

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct AgentThreadCommandContent {
    /// Program to execute.
    pub command: Option<String>,
    /// Arguments passed to the program.
    pub args: Option<Vec<String>>,
    /// Environment overrides added to the terminal process.
    pub env: Option<HashMap<String, String>>,
    /// Working directory override for this agent kind.
    pub cwd: Option<PathBuf>,
    /// Hide this agent's section from the Agent Threads panel.
    ///
    /// Default: false
    pub hidden: Option<bool>,
    /// Which of this agent's resume options (by label) to use by default
    /// when starting a new thread. None means launch with no extra
    /// arguments.
    pub default_launch_option: Option<String>,
}
