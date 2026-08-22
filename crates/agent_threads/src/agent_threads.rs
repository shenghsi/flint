pub mod agent_release;
pub mod artifact_cache;
mod attention_detection;
mod claude_history;
mod codex_history;
pub mod connect_proxy;
#[cfg(any(unix, windows))]
mod control;
#[cfg(windows)]
mod control_windows;
mod egress;
mod handoff;
mod history;
#[cfg(any(unix, windows))]
mod instructions;
pub mod managed_agent;
mod managed_agent_progress;
mod opencode_history;
mod panel;
mod pi_history;
mod plan_usage;
mod remote_process;
mod store;
#[cfg(any(unix, windows))]
mod terminal_control;

use std::path::PathBuf;
use std::sync::Arc;

use collections::HashMap;
use gpui::{App, Context, Entity, EntityId, SharedString, Window, actions};
use settings::{ExtendingVec, RegisterSetting, Settings};
use ui::IconName;
use workspace::Workspace;

use agent_release::{
    AgentRelease, AgentReleaseCatalog, AgentSelfUpdatePolicy, CLAUDE_RELEASES, CODEX_RELEASES,
    OPENCODE_RELEASES, PI_RELEASES,
};
use claude_history::ClaudeHistoryProvider;
use codex_history::CodexHistoryProvider;
use history::AgentHistoryProvider;
pub use history::HistoricalThread;
use opencode_history::OpenCodeHistoryProvider;
pub use panel::AgentThreadsPanel;
use pi_history::PiHistoryProvider;
pub use store::{
    AgentThreadStore, AgentThreadStoreEvent, checkpoint_live_agent_threads,
    prune_stale_session_restore_snapshots, restore_threads_for_workspace,
    snapshot_live_agent_threads,
};

actions!(
    agent_threads,
    [
        /// Starts a new Codex agent thread.
        NewCodexThread,
        /// Starts a new Claude agent thread.
        NewClaudeThread,
        /// Starts a new Pi agent thread.
        NewPiThread,
        /// Starts a new OpenCode agent thread.
        NewOpenCodeThread,
    ]
);

/// A command to launch or resume an agent CLI in a terminal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentLaunchCommand {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub initialization_command: Option<String>,
    pub hidden: bool,
    pub default_launch_option: Option<String>,
}

/// Whether a kind's CLI accepts an initial task prompt as a trailing
/// positional argument when starting a fresh session, used to seed a
/// cross-agent handoff target's first turn without requiring the user to
/// paste it manually.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InitialPromptStrategy {
    /// Append the prompt as the final positional argument, after every other
    /// flag (including the session-id flag).
    TrailingPositionalArg,
    /// Pass the prompt as the value of this CLI flag.
    Flag(&'static str),
    /// No known way to seed a first prompt; the caller must show it to the
    /// user for manual paste instead.
    Unsupported,
}

/// Appends `prompt` to `command`'s args as the CLI's initial task prompt, if
/// `kind` supports it, after every other flag has already been applied.
/// Returns `false` (leaving `command` unchanged) when unsupported, or when
/// `prompt` is empty or looks like a flag (starts with `-`) rather than plain
/// text -- some CLI argument parsers would consume a leading-dash string as an
/// option instead of the positional prompt.
pub(crate) fn seed_launch_command_with_prompt(
    command: &mut AgentLaunchCommand,
    kind: &AgentKindDefinition,
    prompt: &str,
) -> bool {
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt.starts_with('-') {
        return false;
    }
    match kind.initial_prompt_strategy {
        InitialPromptStrategy::TrailingPositionalArg => {
            command.args.push(prompt.to_string());
        }
        InitialPromptStrategy::Flag(flag) => {
            command.args.push(flag.to_string());
            command.args.push(prompt.to_string());
        }
        InitialPromptStrategy::Unsupported => return false,
    }
    true
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
    /// Environment variable honored when resolving this kind's history
    /// directory, and the directory name under `$HOME` used when that override
    /// is unset.
    pub home_env_var: &'static str,
    /// Optional path appended to the environment override. Used for XDG base
    /// directories whose value is shared by multiple applications.
    pub home_env_child: Option<&'static str>,
    pub home_dir_name: &'static str,
    pub history_provider: Option<Arc<dyn AgentHistoryProvider>>,
    pub resume_options: Vec<ResumeOption>,
    /// CLI flag for assigning a session id to a fresh session (e.g.
    /// `--session-id` for Claude Code). Without it the CLI generates an id
    /// internally that Flint never learns, so fresh threads can't be
    /// resumed or restored across app restarts.
    pub session_id_flag: Option<&'static str>,
    pub initial_prompt_strategy: InitialPromptStrategy,
    official_source_prefixes: &'static [&'static str],
    releases: &'static [AgentRelease],
    self_update_policy: AgentSelfUpdatePolicy,
    egress_hosts: &'static [&'static str],
    credential_policy: Option<AgentCredentialPolicy>,
    supports_plan_usage: bool,
    #[cfg(windows)]
    windows_agent_control: WindowsAgentControlCapability,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowsAgentControlCapability {
    pub(crate) instruction_shell: Option<WindowsInstructionShell>,
    pub(crate) cwd_authorization_verified: bool,
    pub(crate) unsupported_reason: Option<&'static str>,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowsInstructionShell {
    PowerShell,
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

    pub fn credential_policy(&self) -> Option<AgentCredentialPolicy> {
        self.credential_policy
    }

    pub fn supports_plan_usage(&self) -> bool {
        self.supports_plan_usage
    }

    #[cfg(windows)]
    pub(crate) fn supports_windows_agent_control(&self) -> bool {
        self.windows_agent_control.instruction_shell.is_some()
            && self.windows_agent_control.cwd_authorization_verified
            && self.windows_agent_control.unsupported_reason.is_none()
    }

    #[cfg(windows)]
    pub(crate) fn windows_agent_control_unsupported_reason(&self) -> Option<&'static str> {
        self.windows_agent_control.unsupported_reason
    }
}

pub fn agent_kind_registry() -> Vec<AgentKindDefinition> {
    // Antigravity CLI is intentionally not registered: its supported `/resume`
    // picker owns cross-project history, while this panel is project-scoped.
    // Matching the integrations below would require depending on private
    // SQLite/protobuf formats, and AGY exposes no supported quota API for the
    // usage header. Reconsider when stable host-integration APIs exist.
    vec![
        AgentKindDefinition {
            id: "codex",
            label: SharedString::new_static("Codex"),
            icon: IconName::AiOpenAi,
            default_command: "codex",
            home_env_var: "CODEX_HOME",
            home_env_child: None,
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
            initial_prompt_strategy: InitialPromptStrategy::TrailingPositionalArg,
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
            credential_policy: Some(AgentCredentialPolicy {
                login_arguments: &["login", "--device-auth"],
                status_arguments: &["login", "status"],
                logout_arguments: &["logout"],
                provider_management_url: "https://platform.openai.com/api-keys",
            }),
            supports_plan_usage: true,
            #[cfg(windows)]
            windows_agent_control: WindowsAgentControlCapability {
                // Official Codex docs confirm both native PowerShell execution
                // and the global CODEX_HOME/AGENTS.md convention on Windows.
                instruction_shell: Some(WindowsInstructionShell::PowerShell),
                cwd_authorization_verified: true,
                unsupported_reason: None,
            },
        },
        AgentKindDefinition {
            id: "claude",
            label: SharedString::new_static("Claude"),
            icon: IconName::AiClaude,
            default_command: "claude",
            home_env_var: "CLAUDE_CONFIG_DIR",
            home_env_child: None,
            home_dir_name: ".claude",
            history_provider: Some(Arc::new(ClaudeHistoryProvider)),
            resume_options: vec![ResumeOption {
                id: "skip-permission-prompts",
                label: SharedString::new_static("Skip permission prompts"),
                args: vec!["--dangerously-skip-permissions".to_string()],
            }],
            session_id_flag: Some("--session-id"),
            initial_prompt_strategy: InitialPromptStrategy::TrailingPositionalArg,
            official_source_prefixes: &["https://downloads.claude.ai/claude-code-releases/"],
            releases: CLAUDE_RELEASES,
            self_update_policy: AgentSelfUpdatePolicy {
                environment: &[("DISABLE_UPDATES", "1")],
                arguments: &[],
            },
            egress_hosts: &["api.anthropic.com", "claude.ai", "platform.claude.com"],
            credential_policy: Some(AgentCredentialPolicy {
                login_arguments: &["auth", "login"],
                status_arguments: &["auth", "status"],
                logout_arguments: &["auth", "logout"],
                provider_management_url: "https://claude.ai/settings/claude-code",
            }),
            supports_plan_usage: true,
            #[cfg(windows)]
            windows_agent_control: WindowsAgentControlCapability {
                instruction_shell: None,
                cwd_authorization_verified: false,
                unsupported_reason: Some(
                    "Claude Code uses Git Bash on native Windows; its cwd authorization path has not been verified",
                ),
            },
        },
        AgentKindDefinition {
            id: "pi",
            label: SharedString::new_static("Pi"),
            icon: IconName::AiPi,
            default_command: "pi",
            home_env_var: "PI_CODING_AGENT_DIR",
            home_env_child: None,
            home_dir_name: ".pi/agent",
            history_provider: Some(Arc::new(PiHistoryProvider)),
            resume_options: Vec::new(),
            session_id_flag: Some("--session-id"),
            initial_prompt_strategy: InitialPromptStrategy::TrailingPositionalArg,
            official_source_prefixes: &[
                "https://github.com/earendil-works/pi/releases/download/",
                "https://release-assets.githubusercontent.com/",
            ],
            releases: PI_RELEASES,
            self_update_policy: AgentSelfUpdatePolicy {
                environment: &[("PI_SKIP_VERSION_CHECK", "1"), ("PI_TELEMETRY", "0")],
                arguments: &[],
            },
            egress_hosts: &[
                "ai-gateway.vercel.sh",
                "api.ant-ling.com",
                "api.anthropic.com",
                "api.cerebras.ai",
                "api.cloudflare.com",
                "api.deepseek.com",
                "api.fireworks.ai",
                "api.github.com",
                "api.groq.com",
                "api.individual.githubcopilot.com",
                "api.kimi.com",
                "api.minimax.io",
                "api.minimaxi.com",
                "api.mistral.ai",
                "api.moonshot.ai",
                "api.moonshot.cn",
                "api.openai.com",
                "api.together.ai",
                "api.x.ai",
                "api.xiaomimimo.com",
                "api.z.ai",
                "auth.openai.com",
                "auth.x.ai",
                "chatgpt.com",
                "claude.ai",
                "gateway.ai.cloudflare.com",
                "generativelanguage.googleapis.com",
                "github.com",
                "huggingface.co",
                "integrate.api.nvidia.com",
                "open.bigmodel.cn",
                "openrouter.ai",
                "pi.dev",
                "platform.claude.com",
                "radius.pi.dev",
                "router.huggingface.co",
                "token-plan-ams.xiaomimimo.com",
                "token-plan-cn.xiaomimimo.com",
                "token-plan-sgp.xiaomimimo.com",
                "token-plan.ap-southeast-1.maas.aliyuncs.com",
                "token-plan.cn-beijing.maas.aliyuncs.com",
                "*.amazonaws.com",
                "*.amazonaws.com.cn",
                "*.githubcopilot.com",
                "*.googleapis.com",
                "*.openai.azure.com",
                "*.services.ai.azure.com",
            ],
            credential_policy: None,
            supports_plan_usage: false,
            #[cfg(windows)]
            windows_agent_control: WindowsAgentControlCapability {
                instruction_shell: None,
                cwd_authorization_verified: false,
                unsupported_reason: Some(
                    "Pi's Windows shell is configurable, so no single instruction block is safe to install",
                ),
            },
        },
        AgentKindDefinition {
            id: "opencode",
            label: SharedString::new_static("OpenCode"),
            icon: IconName::AiOpenCode,
            default_command: "opencode",
            home_env_var: "XDG_DATA_HOME",
            home_env_child: Some("opencode"),
            home_dir_name: ".local/share/opencode",
            history_provider: Some(Arc::new(OpenCodeHistoryProvider)),
            resume_options: vec![ResumeOption {
                id: "auto-approve-permissions",
                label: SharedString::new_static("Auto-approve permissions"),
                args: vec!["--auto".to_string()],
            }],
            // OpenCode's `--session` flag only resumes an existing session; it
            // cannot assign the id of a fresh session.
            session_id_flag: None,
            initial_prompt_strategy: InitialPromptStrategy::Flag("--prompt"),
            official_source_prefixes: &[
                "https://github.com/anomalyco/opencode/releases/download/",
                "https://release-assets.githubusercontent.com/",
            ],
            releases: OPENCODE_RELEASES,
            self_update_policy: AgentSelfUpdatePolicy {
                environment: &[("OPENCODE_DISABLE_AUTOUPDATE", "1")],
                arguments: &[],
            },
            egress_hosts: &[
                "ai-gateway.vercel.sh",
                "api.ant-ling.com",
                "api.anthropic.com",
                "api.cerebras.ai",
                "api.cloudflare.com",
                "api.deepseek.com",
                "api.fireworks.ai",
                "api.github.com",
                "api.groq.com",
                "api.individual.githubcopilot.com",
                "api.kimi.com",
                "api.minimax.io",
                "api.minimaxi.com",
                "api.mistral.ai",
                "api.moonshot.ai",
                "api.moonshot.cn",
                "api.openai.com",
                "api.opencode.ai",
                "api.together.ai",
                "api.x.ai",
                "api.xiaomimimo.com",
                "api.z.ai",
                "auth.openai.com",
                "auth.x.ai",
                "chatgpt.com",
                "claude.ai",
                "gateway.ai.cloudflare.com",
                "generativelanguage.googleapis.com",
                "github.com",
                "huggingface.co",
                "integrate.api.nvidia.com",
                "models.dev",
                "open.bigmodel.cn",
                "opencode.ai",
                "openrouter.ai",
                "platform.claude.com",
                "router.huggingface.co",
                "token-plan-ams.xiaomimimo.com",
                "token-plan-cn.xiaomimimo.com",
                "token-plan-sgp.xiaomimimo.com",
                "token-plan.ap-southeast-1.maas.aliyuncs.com",
                "token-plan.cn-beijing.maas.aliyuncs.com",
                "*.amazonaws.com",
                "*.amazonaws.com.cn",
                "*.githubcopilot.com",
                "*.googleapis.com",
                "*.openai.azure.com",
                "*.services.ai.azure.com",
            ],
            // OpenCode manages credentials for many providers interactively,
            // so there is no single non-interactive logout or provider page.
            credential_policy: None,
            supports_plan_usage: false,
            #[cfg(windows)]
            windows_agent_control: WindowsAgentControlCapability {
                instruction_shell: None,
                cwd_authorization_verified: false,
                unsupported_reason: Some(
                    "OpenCode recommends WSL on Windows; native cwd authorization has not been verified",
                ),
            },
        },
    ]
}

#[derive(Clone, Debug, RegisterSetting)]
pub struct AgentThreadSettings {
    pub codex: AgentLaunchCommand,
    pub claude: AgentLaunchCommand,
    pub pi: AgentLaunchCommand,
    pub opencode: AgentLaunchCommand,
    pub max_visible_threads_per_agent: usize,
    pub show_plan_usage: bool,
    pub notify_when_finished: bool,
    pub reopen_sessions_on_startup: settings::AgentThreadReopenSessionsOnStartup,
    pub dock: settings::DockSide,
    pub starts_open: bool,
    pub agent_control: bool,
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
            "pi" => &self.pi,
            "opencode" => &self.opencode,
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
            pi: launch_command_from_content(content.pi, "pi"),
            opencode: launch_command_from_content(content.opencode, "opencode"),
            max_visible_threads_per_agent: content.max_visible_threads_per_agent.unwrap_or(5),
            show_plan_usage: content.show_plan_usage.unwrap_or(true),
            notify_when_finished: content.notify_when_finished.unwrap_or(true),
            reopen_sessions_on_startup: content.reopen_sessions_on_startup.unwrap_or_default(),
            dock: content.dock.unwrap_or(settings::DockSide::Left),
            starts_open: content.starts_open.unwrap_or(true),
            agent_control: content.agent_control.unwrap_or(true),
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
        initialization_command: content
            .initialization_command
            .filter(|command| !command.trim().is_empty()),
        hidden: content.hidden.unwrap_or(false),
        default_launch_option: content.default_launch_option,
    }
}

pub fn init(cx: &mut App) {
    AgentThreadSettings::register(cx);
    RemoteAgentRoutingSettings::register(cx);
    store::init(cx);
    #[cfg(any(unix, windows))]
    terminal_control::init(cx);

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(new_codex_thread);
        workspace.register_action(new_claude_thread);
        workspace.register_action(new_pi_thread);
        workspace.register_action(new_opencode_thread);
    })
    .detach();
}

/// Returns the highest-priority live Terminal across `workspaces` using the
/// Agent Threads rollup order: Blocked, Finished, Working, then Idle.
pub fn highest_priority_terminal(
    workspaces: &[Entity<Workspace>],
    cx: &mut App,
) -> Option<(Entity<Workspace>, EntityId)> {
    let live_summaries = AgentThreadStore::try_global(cx)
        .map(|store| store.read(cx).live_summary_by_worktree_root())
        .unwrap_or_default();
    #[cfg(any(unix, windows))]
    let regular_terminals = terminal_control::regular_terminal_summaries(cx);

    let mut selected = None;
    for workspace in workspaces {
        let roots = workspace.read(cx).root_paths(cx);
        let mut status = store::ProjectAttentionStatus::Idle;
        let mut terminal_item_id = None;
        let mut attention_launched_at = None;
        let mut live_thread_count = 0;
        for root in roots {
            let Some(summary) = live_summaries.get(root.as_ref()) else {
                continue;
            };
            live_thread_count += summary.live_thread_count;
            let is_more_urgent = terminal_item_id.is_none() || summary.status > status;
            let is_tied_but_more_recent = summary.status == status
                && summary.most_urgent_launched_at.is_some_and(|launched_at| {
                    attention_launched_at.is_none_or(|current| launched_at > current)
                });
            if is_more_urgent || is_tied_but_more_recent {
                status = summary.status;
                terminal_item_id = summary.most_urgent_terminal_item_id;
                attention_launched_at = summary.most_urgent_launched_at;
            }
        }

        #[cfg(any(unix, windows))]
        if let Some(terminal) =
            regular_terminals
                .get(&workspace.entity_id())
                .and_then(|terminals| {
                    terminals
                        .iter()
                        .max_by_key(|terminal| (terminal.status, terminal.creation_sequence))
                })
            && (live_thread_count == 0 || terminal.status > status)
        {
            status = terminal.status;
            terminal_item_id = Some(terminal.terminal_item_id);
        }

        let Some(terminal_item_id) = terminal_item_id else {
            continue;
        };
        if selected
            .as_ref()
            .is_none_or(|(_, selected_status, _)| status > *selected_status)
        {
            selected = Some((workspace.clone(), status, terminal_item_id));
        }
    }
    selected.map(|(workspace, _, terminal_item_id)| (workspace, terminal_item_id))
}

/// Focuses a Terminal returned by `highest_priority_terminal`.
pub fn focus_priority_terminal(
    terminal_item_id: EntityId,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    let store = AgentThreadStore::try_global(cx)
        .ok_or_else(|| anyhow::anyhow!("Agent Thread store is not initialized"))?;
    if store
        .read(cx)
        .thread_display_status(terminal_item_id)
        .is_some()
    {
        store.update(cx, |store, cx| {
            store.focus_thread(terminal_item_id, window, cx)
        })
    } else {
        #[cfg(any(unix, windows))]
        {
            terminal_control::focus_terminal(terminal_item_id, window, cx)
        }
        #[cfg(not(any(unix, windows)))]
        {
            anyhow::bail!("Terminal no longer exists")
        }
    }
}

/// Starts the local-only agent-control server (see `control.rs`). Deliberately
/// **not** called from `init` above: `init` also
/// runs for every test (`crate::init(cx)` in test setup), and this binds a
/// real OS socket under the real `paths::data_dir()` -- a side effect no
/// test should trigger implicitly. The real app entry point
/// (`crates/flint/src/flint.rs`) calls this separately, once, after `init`.
/// No-op on unsupported platforms, where the control server doesn't exist.
pub fn init_control_server(cx: &mut App) {
    #[cfg(any(unix, windows))]
    {
        let instruction_sync_errors = Arc::new(instructions::synchronize_worktree_instructions());
        cx.observe_new(move |workspace: &mut Workspace, _window, cx| {
            instructions::show_sync_errors(&instruction_sync_errors, workspace, cx);
        })
        .detach();
        cx.observe_new(|remote_client: &mut remote::RemoteClient, _window, cx| {
            let remote_client_entity_id = cx.entity_id().as_u64();
            remote_client.proto_client().add_request_handler(
                cx.weak_entity(),
                move |remote_client,
                      envelope: rpc::TypedEnvelope<proto::RemoteTerminalControl>,
                      mut cx| async move {
                    if envelope.payload.envelope.len() > agent_control_protocol::MAX_REQUEST_BYTES {
                        let response = agent_control_protocol::ControlResponse::error(
                            agent_control_protocol::ControlErrorCode::InvalidRequest,
                            "remote terminal control request exceeds the byte limit",
                        );
                        return Ok(proto::RemoteTerminalControlResponse {
                            response: serde_json::to_vec(&response)?,
                        });
                    }
                    let envelope = match serde_json::from_slice(&envelope.payload.envelope) {
                        Ok(envelope) => envelope,
                        Err(error) => {
                            let response = agent_control_protocol::ControlResponse::error(
                                agent_control_protocol::ControlErrorCode::InvalidRequest,
                                format!("remote terminal control request is malformed: {error}"),
                            );
                            return Ok(proto::RemoteTerminalControlResponse {
                                response: serde_json::to_vec(&response)?,
                            });
                        }
                    };
                    let remote_connection_id =
                        cx.update(|cx| terminal_control::RemoteConnectionId {
                            client_entity_id: remote_client_entity_id,
                            generation: remote_client.read(cx).control_connection_generation(),
                        });
                    let store = cx.update(|cx| AgentThreadStore::global(cx));
                    let response =
                        control::dispatch_remote(remote_connection_id, &envelope, &store, &mut cx)
                            .await;
                    let response = serde_json::to_vec(&response)?;
                    if response.len() > agent_control_protocol::MAX_RESPONSE_BYTES {
                        let response = agent_control_protocol::ControlResponse::error(
                            agent_control_protocol::ControlErrorCode::ResponseTooLarge,
                            "remote terminal control response exceeds the byte limit",
                        );
                        return Ok(proto::RemoteTerminalControlResponse {
                            response: serde_json::to_vec(&response)?,
                        });
                    }
                    Ok(proto::RemoteTerminalControlResponse { response })
                },
            );
            cx.subscribe_self(move |_remote_client, event, cx| {
                if matches!(
                    event,
                    remote::RemoteClientEvent::ControlConnectionReset
                        | remote::RemoteClientEvent::Disconnected { .. }
                ) {
                    terminal_control::invalidate_remote_client(remote_client_entity_id, cx);
                }
            })
            .detach();
            cx.on_release(move |_remote_client, cx| {
                terminal_control::invalidate_remote_client(remote_client_entity_id, cx);
            })
            .detach();
        })
        .detach();
        control::init(cx);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = cx;
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

fn new_pi_thread(
    workspace: &mut Workspace,
    _: &NewPiThread,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(kind) = kind_by_id("pi") {
        launch_new_thread_with_default(workspace, &kind, window, cx);
    }
}

fn new_opencode_thread(
    workspace: &mut Workspace,
    _: &NewOpenCodeThread,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(kind) = kind_by_id("opencode") {
        launch_new_thread_with_default(workspace, &kind, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn registry_orders_opencode_after_existing_agents() {
        assert_eq!(
            agent_kind_registry()
                .into_iter()
                .map(|kind| kind.id)
                .collect::<Vec<_>>(),
            ["codex", "claude", "pi", "opencode"]
        );
    }

    #[test]
    fn opencode_registers_all_supported_capabilities() {
        let opencode = kind_by_id("opencode").expect("OpenCode should be registered");

        assert_eq!(opencode.label.as_ref(), "OpenCode");
        assert_eq!(opencode.icon, IconName::AiOpenCode);
        assert_eq!(opencode.default_command, "opencode");
        assert!(opencode.history_provider.is_some());
        assert_eq!(opencode.session_id_flag, None);
        assert_eq!(
            opencode.initial_prompt_strategy,
            InitialPromptStrategy::Flag("--prompt")
        );
        assert!(opencode.credential_policy().is_none());
        assert!(!opencode.supports_plan_usage());
        assert_eq!(
            opencode.self_update_policy().environment,
            [("OPENCODE_DISABLE_AUTOUPDATE", "1")]
        );
        assert!(
            opencode
                .release_for(remote::RemotePlatform {
                    os: remote::RemoteOs::MacOs,
                    arch: remote::RemoteArch::Aarch64,
                    libc: None,
                })
                .is_some()
        );
    }

    #[test]
    fn opencode_resume_uses_session_flag_and_project_directory() {
        let opencode = kind_by_id("opencode").expect("OpenCode should be registered");
        let provider = opencode
            .history_provider
            .as_ref()
            .expect("OpenCode should provide history");
        let base = AgentLaunchCommand {
            command: Some("custom-opencode".to_string()),
            env: HashMap::from_iter([("EXISTING".to_string(), "value".to_string())]),
            initialization_command: Some("source ~/.profile".to_string()),
            ..Default::default()
        };
        let thread = HistoricalThread {
            session_id: "ses_test".into(),
            title: "OpenCode task".into(),
            project_root: PathBuf::from("/work/project"),
            last_activity_at: std::time::SystemTime::UNIX_EPOCH,
        };

        let command = provider.resume_command(&base, &thread, &["--auto".to_string()]);

        assert_eq!(command.command.as_deref(), Some("custom-opencode"));
        assert_eq!(command.args, ["--session", "ses_test", "--auto"]);
        assert_eq!(command.cwd, Some(PathBuf::from("/work/project")));
        assert_eq!(
            command.env.get("EXISTING").map(String::as_str),
            Some("value")
        );
        assert_eq!(
            command.initialization_command.as_deref(),
            Some("source ~/.profile")
        );
    }

    #[test]
    fn seed_launch_command_appends_prompt_after_existing_args() {
        let codex = kind_by_id("codex").unwrap();
        let mut command = AgentLaunchCommand {
            args: vec!["--dangerously-bypass-approvals-and-sandbox".to_string()],
            ..Default::default()
        };
        assert!(seed_launch_command_with_prompt(
            &mut command,
            &codex,
            "read the handoff doc and continue"
        ));
        assert_eq!(
            command.args,
            vec![
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "read the handoff doc and continue".to_string(),
            ]
        );
    }

    #[test]
    fn seed_launch_command_rejects_dash_prefixed_prompt() {
        let claude = kind_by_id("claude").unwrap();
        let mut command = AgentLaunchCommand::default();
        assert!(!seed_launch_command_with_prompt(
            &mut command,
            &claude,
            "--dangerously-skip-permissions looks like a flag"
        ));
        assert!(command.args.is_empty());
    }

    #[test]
    fn seed_launch_command_rejects_empty_prompt() {
        let pi = kind_by_id("pi").unwrap();
        let mut command = AgentLaunchCommand::default();
        assert!(!seed_launch_command_with_prompt(&mut command, &pi, "   "));
        assert!(command.args.is_empty());
    }

    #[test]
    fn registered_kinds_use_their_supported_initial_prompt_form() {
        for kind in agent_kind_registry()
            .into_iter()
            .filter(|kind| kind.id != "opencode")
        {
            assert_eq!(
                kind.initial_prompt_strategy,
                InitialPromptStrategy::TrailingPositionalArg,
                "{} should support seeding a handoff prompt",
                kind.id
            );
        }

        let opencode = kind_by_id("opencode").expect("OpenCode should be registered");
        let mut command = AgentLaunchCommand {
            args: vec!["--auto".to_string()],
            ..Default::default()
        };
        assert!(seed_launch_command_with_prompt(
            &mut command,
            &opencode,
            "continue from the handoff"
        ));
        assert_eq!(
            command.args,
            ["--auto", "--prompt", "continue from the handoff"]
        );
    }

    #[test]
    fn pi_registers_history_without_provider_specific_controls() {
        let pi = kind_by_id("pi").expect("Pi should be registered");

        assert!(pi.history_provider.is_some());
        assert!(pi.credential_policy().is_none());
        assert!(!pi.supports_plan_usage());
    }

    #[test]
    fn remote_route_lookup_defaults_direct_and_matches_only_ssh_identity() {
        let settings = RemoteAgentRoutingSettings {
            ssh_connections: ExtendingVec(vec![settings::SshConnection {
                host: "build.example.com".to_string(),
                username: Some("dev".to_string()),
                port: Some(2222),
                nickname: Some("ignored nickname".to_string()),
                agent_route: Some(settings::RemoteAgentRoute::Tunneled),
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
            Some(settings::RemoteAgentRoute::Tunneled)
        );
        assert_eq!(
            settings.route_for(&unknown),
            Some(settings::RemoteAgentRoute::Direct)
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
            ["api.anthropic.com", "claude.ai", "platform.claude.com"]
        );
        let pi = kind_by_id("pi").expect("Pi should be registered");
        assert!(pi.egress_hosts().contains(&"api.anthropic.com"));
        assert!(pi.egress_hosts().contains(&"api.openai.com"));
        assert!(pi.egress_hosts().contains(&"*.amazonaws.com"));
        assert!(pi.egress_hosts().contains(&"*.googleapis.com"));
        assert!(pi.egress_hosts().contains(&"pi.dev"));

        let opencode = kind_by_id("opencode").expect("OpenCode should be registered");
        assert!(opencode.egress_hosts().contains(&"api.anthropic.com"));
        assert!(opencode.egress_hosts().contains(&"api.openai.com"));
        assert!(opencode.egress_hosts().contains(&"api.opencode.ai"));
        assert!(opencode.egress_hosts().contains(&"models.dev"));
        assert!(opencode.egress_hosts().contains(&"*.amazonaws.com"));
        assert!(opencode.egress_hosts().contains(&"*.googleapis.com"));
    }

    #[test]
    fn credential_policies_use_pinned_cli_commands_and_provider_surfaces() {
        let codex = kind_by_id("codex").expect("Codex kind should exist");
        let codex_policy = codex
            .credential_policy()
            .expect("Codex should have a credential policy");
        assert_eq!(codex_policy.login_arguments, ["login", "--device-auth"]);
        assert_eq!(codex_policy.status_arguments, ["login", "status"]);
        assert_eq!(codex_policy.logout_arguments, ["logout"]);
        assert_eq!(
            codex_policy.provider_management_url,
            "https://platform.openai.com/api-keys"
        );

        let claude = kind_by_id("claude").expect("Claude kind should exist");
        let claude_policy = claude
            .credential_policy()
            .expect("Claude should have a credential policy");
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
            initialization_command: None,
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
    fn pi_command_defaults_to_pi_when_settings_are_absent() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent::default());

        assert_eq!(
            settings.command_for_kind("pi").command.as_deref(),
            Some("pi")
        );
    }

    #[test]
    fn opencode_command_defaults_to_opencode_when_settings_are_absent() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent::default());

        assert_eq!(
            settings.command_for_kind("opencode").command.as_deref(),
            Some("opencode")
        );
    }

    #[test]
    fn initialization_command_is_per_agent_and_ignores_whitespace_only_values() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent {
            agent_threads: Some(settings::AgentThreadSettingsContent {
                codex: Some(settings::AgentThreadCommandContent {
                    initialization_command: Some(" source ~/.profile ".to_string()),
                    ..Default::default()
                }),
                claude: Some(settings::AgentThreadCommandContent {
                    initialization_command: Some("   ".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(
            settings
                .command_for_kind("codex")
                .initialization_command
                .as_deref(),
            Some(" source ~/.profile ")
        );
        assert!(
            settings
                .command_for_kind("claude")
                .initialization_command
                .is_none()
        );
        assert!(
            settings
                .command_for_kind("pi")
                .initialization_command
                .is_none()
        );
    }

    #[test]
    fn notify_when_finished_is_enabled_by_default() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent::default());
        assert!(settings.notify_when_finished);
    }

    #[test]
    fn reopen_sessions_on_startup_is_matching_workspace_by_default() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent::default());

        assert_eq!(
            settings.reopen_sessions_on_startup,
            settings::AgentThreadReopenSessionsOnStartup::MatchingWorkspace
        );
    }

    #[test]
    fn reopen_sessions_on_startup_reads_never_value() {
        let settings = AgentThreadSettings::from_settings(&settings::SettingsContent {
            agent_threads: Some(settings::AgentThreadSettingsContent {
                reopen_sessions_on_startup: Some(
                    settings::AgentThreadReopenSessionsOnStartup::Never,
                ),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(
            settings.reopen_sessions_on_startup,
            settings::AgentThreadReopenSessionsOnStartup::Never
        );
    }
}
