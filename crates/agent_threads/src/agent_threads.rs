pub mod agent_release;
mod claude_history;
mod codex_history;
mod history;
mod panel;
mod plan_usage;
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
pub use store::{
    AgentThreadStore, AgentThreadStoreEvent, restore_threads_for_workspace,
    snapshot_live_agent_threads,
};

use agent_release::{AgentRelease, AgentReleaseCatalog, AgentSelfUpdatePolicy};
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
    pub hidden: bool,
    pub default_launch_option: Option<String>,
}

/// A resume-time flag a user can opt into, e.g. `--dangerously-skip-permissions`.
#[derive(Clone, Debug)]
pub struct ResumeOption {
    /// Stable key used to persist a user's remembered choice (see
    /// `store::LAUNCH_OPTION_NAMESPACE`). Must not change once shipped, since
    /// existing persisted choices are matched against it; `label` is display
    /// copy and can be edited freely without losing those choices.
    pub id: &'static str,
    pub label: SharedString,
    pub args: Vec<String>,
}

/// Resolves the extra arguments to use when starting a *new* thread for
/// `kind`, based on the user's persisted default launch option (matched by
/// label against `kind.resume_options`). Returns an empty slice if no
/// default is set, or if the persisted label no longer matches any
/// registered option.
pub(crate) fn resolve_default_launch_args<'a>(
    command: &'a AgentLaunchCommand,
    kind: &'a AgentKindDefinition,
) -> &'a [String] {
    let Some(label) = command.default_launch_option.as_deref() else {
        return &[];
    };
    kind.resume_options
        .iter()
        .find(|option| option.label.as_ref() == label)
        .map(|option| option.args.as_slice())
        .unwrap_or(&[])
}

/// Resolves the `ResumeOption::id` of `kind`'s settings-configured default
/// launch option, if any. Used to compare the settings-facing (label-based)
/// default against the store's persisted (id-based) remembered choice when
/// deciding which menu entry to show as selected.
pub(crate) fn resolve_default_launch_option_id<'a>(
    command: &AgentLaunchCommand,
    kind: &'a AgentKindDefinition,
) -> Option<&'a str> {
    let label = command.default_launch_option.as_deref()?;
    kind.resume_options
        .iter()
        .find(|option| option.label.as_ref() == label)
        .map(|option| option.id)
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
    /// CLI flag for assigning a session id to a fresh session (e.g.
    /// `--session-id` for Claude Code). Without it the CLI generates an id
    /// internally that Flint never learns, so fresh threads can't be
    /// resumed or restored across app restarts.
    pub session_id_flag: Option<&'static str>,
    official_source_prefixes: &'static [&'static str],
    releases: &'static [AgentRelease],
    self_update_policy: AgentSelfUpdatePolicy,
}

impl AgentKindDefinition {
    pub fn release_for(&self, target: remote::RemotePlatform) -> Option<&AgentRelease> {
        AgentReleaseCatalog::new(self.id, self.official_source_prefixes, self.releases)
            .release_for(target)
    }

    pub fn self_update_policy(&self) -> AgentSelfUpdatePolicy {
        self.self_update_policy
    }
}

pub fn agent_kind_registry() -> Vec<AgentKindDefinition> {
    vec![
        AgentKindDefinition {
            id: "codex",
            label: SharedString::new_static("Codex"),
            icon: IconName::AiOpenAi,
            default_command: "codex",
            home_env_var: "CODEX_HOME",
            home_dir_name: ".codex",
            history_provider: Some(Arc::new(CodexHistoryProvider)),
            resume_options: vec![ResumeOption {
                id: "bypass-approvals-and-sandbox",
                label: SharedString::new_static("Bypass approvals & sandbox"),
                args: vec!["--dangerously-bypass-approvals-and-sandbox".to_string()],
            }],
            // Codex CLI has no flag for assigning a session id to a fresh
            // session, so fresh Codex threads stay non-restorable.
            session_id_flag: None,
            official_source_prefixes: &["https://github.com/openai/codex/releases/download/"],
            releases: &[],
            self_update_policy: AgentSelfUpdatePolicy {
                environment: &[],
                arguments: &["--config", "check_for_update_on_startup=false"],
            },
        },
        AgentKindDefinition {
            id: "claude",
            label: SharedString::new_static("Claude"),
            icon: IconName::AiClaude,
            default_command: "claude",
            home_env_var: "CLAUDE_CONFIG_DIR",
            home_dir_name: ".claude",
            history_provider: Some(Arc::new(ClaudeHistoryProvider)),
            resume_options: vec![ResumeOption {
                id: "skip-permission-prompts",
                label: SharedString::new_static("Skip permission prompts"),
                args: vec!["--dangerously-skip-permissions".to_string()],
            }],
            session_id_flag: Some("--session-id"),
            official_source_prefixes: &["https://downloads.claude.ai/claude-code-releases/"],
            releases: &[],
            self_update_policy: AgentSelfUpdatePolicy {
                environment: &[("DISABLE_UPDATES", "1")],
                arguments: &[],
            },
        },
    ]
}

#[derive(Clone, Debug, RegisterSetting)]
pub struct AgentThreadSettings {
    pub codex: AgentLaunchCommand,
    pub claude: AgentLaunchCommand,
    pub max_visible_threads_per_agent: usize,
    pub show_plan_usage: bool,
    pub notify_when_finished: bool,
    pub reopen_sessions_on_startup: settings::AgentThreadReopenSessionsOnStartup,
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
            show_plan_usage: content.show_plan_usage.unwrap_or(true),
            notify_when_finished: content.notify_when_finished.unwrap_or(true),
            reopen_sessions_on_startup: content.reopen_sessions_on_startup.unwrap_or_default(),
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
        hidden: content.hidden.unwrap_or(false),
        default_launch_option: content.default_launch_option,
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

/// Launches a new thread for `kind`, using whatever launch option the user
/// has set as the default for it (if any).
pub(crate) fn launch_new_thread_with_default(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let extra_args = store::resolve_new_thread_launch_args(cx, kind);
    store::launch_new_thread(workspace, kind, &extra_args, window, cx);
}

fn new_codex_thread(
    workspace: &mut Workspace,
    _: &NewCodexThread,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(kind) = kind_by_id("codex") {
        launch_new_thread_with_default(workspace, &kind, window, cx);
    }
}

fn new_claude_thread(
    workspace: &mut Workspace,
    _: &NewClaudeThread,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(kind) = kind_by_id("claude") {
        launch_new_thread_with_default(workspace, &kind, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_with_default(default_launch_option: Option<&str>) -> AgentLaunchCommand {
        AgentLaunchCommand {
            command: Some("codex".to_string()),
            args: Vec::new(),
            env: HashMap::default(),
            cwd: None,
            hidden: false,
            default_launch_option: default_launch_option.map(str::to_string),
        }
    }

    #[test]
    fn resolve_default_launch_args_returns_empty_when_unset() {
        let kind = kind_by_id("codex").unwrap();
        let command = command_with_default(None);
        assert!(resolve_default_launch_args(&command, &kind).is_empty());
    }

    #[test]
    fn resolve_default_launch_args_returns_matching_option_args() {
        let kind = kind_by_id("codex").unwrap();
        let command = command_with_default(Some("Bypass approvals & sandbox"));
        assert_eq!(
            resolve_default_launch_args(&command, &kind),
            ["--dangerously-bypass-approvals-and-sandbox".to_string()]
        );
    }

    #[test]
    fn resolve_default_launch_args_returns_empty_when_label_unknown() {
        let kind = kind_by_id("codex").unwrap();
        let command = command_with_default(Some("nonexistent option"));
        assert!(resolve_default_launch_args(&command, &kind).is_empty());
    }

    #[test]
    fn plan_usage_is_enabled_by_default() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent::default());
        assert!(settings.show_plan_usage);
    }

    #[test]
    fn notify_when_finished_is_enabled_by_default() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent::default());
        assert!(settings.notify_when_finished);
    }

    #[test]
    fn reopen_sessions_on_startup_is_never_by_default() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent::default());

        assert_eq!(
            settings.reopen_sessions_on_startup,
            settings::AgentThreadReopenSessionsOnStartup::Never
        );
    }

    #[test]
    fn reopen_sessions_on_startup_reads_matching_workspace_value() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent {
            agent_threads: Some(settings::AgentThreadSettingsContent {
                reopen_sessions_on_startup: Some(
                    settings::AgentThreadReopenSessionsOnStartup::MatchingWorkspace,
                ),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(
            settings.reopen_sessions_on_startup,
            settings::AgentThreadReopenSessionsOnStartup::MatchingWorkspace
        );
    }
}
