pub mod agent_release;
pub mod artifact_cache;
mod claude_history;
mod codex_history;
pub mod connect_proxy;
mod egress;
mod history;
pub mod managed_agent;
mod panel;
mod plan_usage;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use collections::HashMap;
use gpui::{App, Context, SharedString, Window, actions};
use settings::{ExtendingVec, RegisterSetting, Settings};
use ui::IconName;
use workspace::Workspace;

pub use history::HistoricalThread;
pub use panel::AgentThreadsPanel;
pub use store::{
    AgentThreadStore, AgentThreadStoreEvent, restore_threads_for_workspace,
    snapshot_live_agent_threads,
};

use agent_release::{
    AgentRelease, AgentReleaseCatalog, AgentSelfUpdatePolicy, CLAUDE_RELEASES, CODEX_RELEASES,
};
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
    egress_hosts: &'static [&'static str],
    credential_policy: AgentCredentialPolicy,
}

#[derive(Clone, Copy)]
pub struct AgentCredentialPolicy {
    pub login_arguments: &'static [&'static str],
    pub status_arguments: &'static [&'static str],
    pub logout_arguments: &'static [&'static str],
    pub provider_management_url: &'static str,
}

impl AgentKindDefinition {
    pub fn release_for(&self, target: remote::RemotePlatform) -> Option<&AgentRelease> {
        AgentReleaseCatalog::new(self.id, self.official_source_prefixes, self.releases)
            .release_for(target)
    }

    pub fn self_update_policy(&self) -> AgentSelfUpdatePolicy {
        self.self_update_policy
    }

    pub fn official_source_prefixes(&self) -> &'static [&'static str] {
        self.official_source_prefixes
    }

    pub fn egress_hosts(&self) -> &'static [&'static str] {
        self.egress_hosts
    }

    pub fn credential_policy(&self) -> AgentCredentialPolicy {
        self.credential_policy
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
            official_source_prefixes: &[
                "https://github.com/openai/codex/releases/download/",
                "https://release-assets.githubusercontent.com/",
            ],
            releases: CODEX_RELEASES,
            self_update_policy: AgentSelfUpdatePolicy {
                environment: &[],
                arguments: &["--config", "check_for_update_on_startup=false"],
            },
            egress_hosts: &["api.openai.com", "auth.openai.com", "chatgpt.com"],
            credential_policy: AgentCredentialPolicy {
                login_arguments: &["login", "--device-auth"],
                status_arguments: &["login", "status"],
                logout_arguments: &["logout"],
                provider_management_url: "https://platform.openai.com/api-keys",
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
            releases: CLAUDE_RELEASES,
            self_update_policy: AgentSelfUpdatePolicy {
                environment: &[("DISABLE_UPDATES", "1")],
                arguments: &[],
            },
            egress_hosts: &["api.anthropic.com", "claude.ai", "console.anthropic.com"],
            credential_policy: AgentCredentialPolicy {
                login_arguments: &["auth", "login"],
                status_arguments: &["auth", "status"],
                logout_arguments: &["auth", "logout"],
                provider_management_url: "https://claude.ai/settings/claude-code",
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

#[derive(RegisterSetting)]
pub(crate) struct RemoteAgentRoutingSettings {
    ssh_connections: ExtendingVec<settings::SshConnection>,
}

impl Settings for RemoteAgentRoutingSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self {
            ssh_connections: content
                .remote
                .ssh_connections
                .clone()
                .unwrap_or_default()
                .into(),
        }
    }
}

impl RemoteAgentRoutingSettings {
    pub fn route_for(
        &self,
        connection_options: &remote::RemoteConnectionOptions,
    ) -> Option<settings::RemoteAgentRoute> {
        let remote::RemoteConnectionIdentity::Ssh {
            host,
            username,
            port,
        } = remote::remote_connection_identity(connection_options)
        else {
            return None;
        };
        Some(
            self.ssh_connections
                .0
                .iter()
                .find(|connection| {
                    connection.host == host
                        && connection.username == username
                        && connection.port == port
                })
                .map(settings::SshConnection::effective_agent_route)
                .unwrap_or_default(),
        )
    }
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
    RemoteAgentRoutingSettings::register(cx);
    store::init(cx);

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(new_codex_thread);
        workspace.register_action(new_claude_thread);
    })
    .detach();
}

pub fn active_remote_agent_thread_count(
    connection_options: &remote::RemoteConnectionOptions,
    cx: &App,
) -> usize {
    store::AgentThreadStore::try_global(cx)
        .map(|store| {
            store
                .read(cx)
                .active_thread_count_for_connection(connection_options, cx)
        })
        .unwrap_or(0)
}

pub fn close_remote_agent_threads(
    connection_options: remote::RemoteConnectionOptions,
    cx: &mut App,
) -> gpui::Task<anyhow::Result<()>> {
    if store::AgentThreadStore::try_global(cx).is_none() {
        return gpui::Task::ready(Ok(()));
    }
    store::AgentThreadStore::close_threads_for_connection(connection_options, cx)
}

pub fn begin_remote_agent_route_change(
    connection_options: &remote::RemoteConnectionOptions,
    cx: &mut App,
) -> anyhow::Result<()> {
    store::AgentThreadStore::init_global(cx);
    store::AgentThreadStore::global(cx)
        .update(cx, |store, _| store.begin_route_change(connection_options))
}

pub fn finish_remote_agent_route_change(
    connection_options: &remote::RemoteConnectionOptions,
    cx: &mut App,
) {
    if let Some(store) = store::AgentThreadStore::try_global(cx) {
        store.update(cx, |store, _| {
            store.finish_route_change(connection_options);
        });
    }
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
    use gpui::TestAppContext;

    #[test]
    fn remote_route_lookup_defaults_direct_and_matches_only_ssh_identity() {
        let settings = RemoteAgentRoutingSettings {
            ssh_connections: ExtendingVec(vec![settings::SshConnection {
                host: "build.example.com".to_string(),
                username: Some("dev".to_string()),
                port: Some(2222),
                nickname: Some("ignored nickname".to_string()),
                agent_route: Some(settings::RemoteAgentRoute::ThroughFlint),
                ..Default::default()
            }]),
        };
        let matching = remote::RemoteConnectionOptions::Ssh(remote::SshConnectionOptions {
            host: "build.example.com".into(),
            username: Some("dev".to_string()),
            port: Some(2222),
            nickname: Some("runtime nickname".to_string()),
            ..Default::default()
        });
        let unknown = remote::RemoteConnectionOptions::Ssh(remote::SshConnectionOptions {
            host: "new.example.com".into(),
            ..Default::default()
        });

        assert_eq!(
            settings.route_for(&matching),
            Some(settings::RemoteAgentRoute::ThroughFlint)
        );
        assert_eq!(
            settings.route_for(&unknown),
            Some(settings::RemoteAgentRoute::NotThroughFlint)
        );
        assert_eq!(
            settings.route_for(&remote::RemoteConnectionOptions::Wsl(
                remote::WslConnectionOptions {
                    distro_name: "Ubuntu".to_string(),
                    user: None,
                }
            )),
            None
        );
    }

    #[test]
    fn destination_policy_contains_only_required_model_and_authentication_hosts() {
        let codex = kind_by_id("codex").expect("Codex should be registered");
        let claude = kind_by_id("claude").expect("Claude should be registered");

        assert_eq!(
            codex.egress_hosts(),
            ["api.openai.com", "auth.openai.com", "chatgpt.com"]
        );
        assert_eq!(
            claude.egress_hosts(),
            ["api.anthropic.com", "claude.ai", "console.anthropic.com"]
        );
    }

    #[test]
    fn credential_policies_use_pinned_cli_commands_and_provider_surfaces() {
        let codex = kind_by_id("codex").expect("Codex kind should exist");
        let codex_policy = codex.credential_policy();
        assert_eq!(codex_policy.login_arguments, ["login", "--device-auth"]);
        assert_eq!(codex_policy.status_arguments, ["login", "status"]);
        assert_eq!(codex_policy.logout_arguments, ["logout"]);
        assert_eq!(
            codex_policy.provider_management_url,
            "https://platform.openai.com/api-keys"
        );

        let claude = kind_by_id("claude").expect("Claude kind should exist");
        let claude_policy = claude.credential_policy();
        assert_eq!(claude_policy.login_arguments, ["auth", "login"]);
        assert_eq!(claude_policy.status_arguments, ["auth", "status"]);
        assert_eq!(claude_policy.logout_arguments, ["auth", "logout"]);
        assert_eq!(
            claude_policy.provider_management_url,
            "https://claude.ai/settings/claude-code"
        );
    }

    #[gpui::test]
    fn route_change_guard_is_connection_scoped(cx: &mut TestAppContext) {
        let first = remote::RemoteConnectionOptions::Ssh(remote::SshConnectionOptions {
            host: "first.example.com".into(),
            ..Default::default()
        });
        let second = remote::RemoteConnectionOptions::Ssh(remote::SshConnectionOptions {
            host: "second.example.com".into(),
            ..Default::default()
        });

        cx.update(|cx| {
            begin_remote_agent_route_change(&first, cx).expect("first route change should begin");
            assert!(begin_remote_agent_route_change(&first, cx).is_err());
            begin_remote_agent_route_change(&second, cx)
                .expect("another host should remain independent");
            finish_remote_agent_route_change(&first, cx);
            begin_remote_agent_route_change(&first, cx)
                .expect("finished route change should release the guard");
            finish_remote_agent_route_change(&first, cx);
            finish_remote_agent_route_change(&second, cx);
        });
    }

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
