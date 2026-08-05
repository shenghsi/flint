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
    /// Command used for new Pi agent threads.
    pub pi: Option<AgentThreadCommandContent>,
    /// Command used for new OpenCode agent threads.
    pub opencode: Option<AgentThreadCommandContent>,
    /// Maximum number of threads shown per agent section before a "Show
    /// more" control is offered.
    ///
    /// Default: 5
    pub max_visible_threads_per_agent: Option<usize>,
    /// Whether to show five-hour and weekly plan usage in section headings.
    ///
    /// Default: true
    pub show_plan_usage: Option<bool>,
    /// Whether to show a desktop notification when an agent thread finishes
    /// a turn or needs input, detected via the terminal bell that Claude
    /// Code and Codex CLI ring in those cases.
    ///
    /// Default: true
    pub notify_when_finished: Option<bool>,
    /// When to reopen live agent sessions from the previous app session.
    ///
    /// Default: matching_workspace
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
    Never,
    /// Reopen sessions when any matching workspace from the previous app session opens.
    #[default]
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
    /// Shell command to run before launching this agent. The agent launches
    /// only when this command succeeds.
    ///
    /// Default: ""
    pub initialization_command: Option<String>,
    /// Hide this agent's section from the Agent Threads panel.
    ///
    /// Default: false
    pub hidden: Option<bool>,
    /// Which of this agent's resume options (by label) to use by default
    /// when starting a new thread. None means launch with no extra
    /// arguments.
    pub default_launch_option: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_threads_initialization_command_round_trips() {
        let content: AgentThreadSettingsContent =
            serde_json::from_str(r#"{"codex":{"initialization_command":"source ~/.profile"}}"#)
                .expect("valid agent thread settings");

        assert_eq!(
            content
                .codex
                .as_ref()
                .and_then(|command| command.initialization_command.as_deref()),
            Some("source ~/.profile")
        );
        let serialized = serde_json::to_value(content).expect("serializable agent thread settings");
        assert_eq!(
            serialized["codex"]["initialization_command"],
            "source ~/.profile"
        );
    }

    #[test]
    fn opencode_agent_thread_settings_round_trip() {
        let content: AgentThreadSettingsContent = serde_json::from_str(
            r#"{"opencode":{"command":"custom-opencode","hidden":true,"initialization_command":"source ~/.profile"}}"#,
        )
        .expect("valid OpenCode agent thread settings");

        let opencode = content
            .opencode
            .as_ref()
            .expect("OpenCode settings should be present");
        assert_eq!(opencode.command.as_deref(), Some("custom-opencode"));
        assert_eq!(opencode.hidden, Some(true));
        assert_eq!(
            opencode.initialization_command.as_deref(),
            Some("source ~/.profile")
        );

        let serialized = serde_json::to_value(content).expect("serializable agent thread settings");
        assert_eq!(serialized["opencode"]["command"], "custom-opencode");
        assert_eq!(serialized["opencode"]["hidden"], true);
    }
}
