mod claude_history;
mod codex_history;
mod history;
mod panel;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use collections::HashMap;
use gpui::{App, Context, SharedString, Window, actions};
use settings::{RegisterSetting, Settings};
use ui::IconName;
use workspace::Workspace;

pub use history::HistoricalThread;
pub use panel::AgentThreadsPanel;
pub use store::AgentThreadStore;

use claude_history::ClaudeHistoryProvider;
use codex_history::CodexHistoryProvider;
use history::AgentHistoryProvider;

actions!(
    agent_threads,
    [
        /// Starts a new Codex agent thread.
        NewCodexThread,
        /// Starts a new Claude agent thread.
        NewClaudeThread,
    ]
);

/// A command to launch or resume an agent CLI in a terminal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentLaunchCommand {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
}

/// A resume-time flag a user can opt into, e.g. `--dangerously-skip-permissions`.
#[derive(Clone, Debug)]
pub struct ResumeOption {
    pub label: SharedString,
    pub args: Vec<String>,
}

/// A registered agent kind. New kinds are added here without touching the
/// store or panel rendering code.
#[derive(Clone)]
pub struct AgentKindDefinition {
    pub id: &'static str,
    pub label: SharedString,
    pub icon: IconName,
    pub default_command: &'static str,
    /// `CLAUDE_CONFIG_DIR`-style env var honored when resolving this
    /// kind's config directory, and the directory name under `$HOME`
    /// used when that override is unset.
    pub home_env_var: &'static str,
    pub home_dir_name: &'static str,
    pub history_provider: Option<Arc<dyn AgentHistoryProvider>>,
    pub resume_options: Vec<ResumeOption>,
}

pub fn agent_kind_registry() -> Vec<AgentKindDefinition> {
    vec![
        AgentKindDefinition {
            id: "codex",
            label: SharedString::new_static("Codex"),
            icon: IconName::Sparkle,
            default_command: "codex",
            home_env_var: "CODEX_HOME",
            home_dir_name: ".codex",
            history_provider: Some(Arc::new(CodexHistoryProvider)),
            resume_options: vec![ResumeOption {
                label: SharedString::new_static("Bypass approvals & sandbox"),
                args: vec!["--dangerously-bypass-approvals-and-sandbox".to_string()],
            }],
        },
        AgentKindDefinition {
            id: "claude",
            label: SharedString::new_static("Claude"),
            icon: IconName::Sparkle,
            default_command: "claude",
            home_env_var: "CLAUDE_CONFIG_DIR",
            home_dir_name: ".claude",
            history_provider: Some(Arc::new(ClaudeHistoryProvider)),
            resume_options: vec![ResumeOption {
                label: SharedString::new_static("Skip permission prompts"),
                args: vec!["--dangerously-skip-permissions".to_string()],
            }],
        },
    ]
}

#[derive(Clone, Debug, RegisterSetting)]
pub struct AgentThreadSettings {
    pub codex: AgentLaunchCommand,
    pub claude: AgentLaunchCommand,
    pub max_visible_threads_per_agent: usize,
    pub dock: settings::DockSide,
}

impl AgentThreadSettings {
    pub fn command_for_kind(&self, kind_id: &str) -> &AgentLaunchCommand {
        match kind_id {
            "codex" => &self.codex,
            "claude" => &self.claude,
            _ => &self.claude,
        }
    }
}

impl Settings for AgentThreadSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.agent_threads.clone().unwrap_or_default();
        Self {
            codex: launch_command_from_content(content.codex, "codex"),
            claude: launch_command_from_content(content.claude, "claude"),
            max_visible_threads_per_agent: content.max_visible_threads_per_agent.unwrap_or(5),
            dock: content.dock.unwrap_or(settings::DockSide::Left),
        }
    }
}

fn launch_command_from_content(
    content: Option<settings::AgentThreadCommandContent>,
    default_command: &'static str,
) -> AgentLaunchCommand {
    let content = content.unwrap_or_default();
    AgentLaunchCommand {
        command: content.command.or(Some(default_command.to_string())),
        args: content.args.unwrap_or_default(),
        env: content.env.unwrap_or_default().into_iter().collect(),
        cwd: content.cwd,
    }
}

pub fn init(cx: &mut App) {
    AgentThreadSettings::register(cx);
    store::init(cx);

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(new_codex_thread);
        workspace.register_action(new_claude_thread);
    })
    .detach();
}

fn kind_by_id(id: &str) -> Option<AgentKindDefinition> {
    agent_kind_registry().into_iter().find(|kind| kind.id == id)
}

fn new_codex_thread(
    workspace: &mut Workspace,
    _: &NewCodexThread,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(kind) = kind_by_id("codex") {
        store::launch_new_thread(workspace, &kind, window, cx);
    }
}

fn new_claude_thread(
    workspace: &mut Workspace,
    _: &NewClaudeThread,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(kind) = kind_by_id("claude") {
        store::launch_new_thread(workspace, &kind, window, cx);
    }
}
