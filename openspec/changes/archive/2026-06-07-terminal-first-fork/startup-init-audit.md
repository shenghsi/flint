# Startup Initialization Audit

This audit maps `crates/zed/src/main.rs` startup initialization for the
terminal-first fork. Line numbers refer to the current working tree at the time
of this change.

## Remove or Guard

These init paths belong to product surfaces the fork plans to remove.

### ACP and Native Agent/Chat

- `agent`, `agent_client_protocol`, and `agent_ui` imports at lines 17-19.
- `acp_tools::init(cx)` at line 697.
- `project::AgentRegistryStore::init_global(...)` at lines 706-710.
- `agent_ui::init(...)` at lines 711-718.
- `zed::watch_user_agents_md(...)` at line 719.
- Agent URL/session handling paths around lines 1031-1100.

### Native Model Providers, Copilot, Web Search, and Edit Prediction

- `copilot_chat::CopilotChatConfiguration` construction at lines 675-681.
- `copilot_chat::init(...)` at lines 682-687.
- `copilot_ui::init(...)` at line 689.
- `language_model::init(cx)` at line 690.
- `RefreshLlmTokenListener::register(...)` at lines 691-695.
- `language_models::init(...)` at line 696.
- `edit_prediction_ui::init(cx)` at line 700.
- `web_search::init(cx)` at line 701.
- `web_search_providers::init(...)` at line 702.
- `edit_prediction_registry::init(...)` at line 704.
- `edit_prediction::init(cx)` at line 778.

Note: git commit message generation currently depends on `language_model` and
`agent_settings`. Remove these after the external command commit-message runner
is implemented.

### Debugger and DAP

- `debug_adapter_extension::init(...)` at line 559.
- `debugger_ui::init(cx)` at line 589.
- `debugger_tools::init(cx)` at line 590.
- `dap_adapters::init(cx)` at line 656.
- `zed::remote_debug::init(cx)` at line 699.

### Collaboration, Calls, Channels, and Audio

- `collab_ui::channel_view::ChannelView` import at line 24.
- `channel::init(...)` at line 745.
- `audio::init(cx)` at line 732.
- `call::init(...)` at line 766.
- `collab_ui::init(...)` at line 768.
- Channel request handling around lines 1388-1421.

### Zed Cloud/Product Surfaces

- `zed::telemetry_log::init(cx)` at line 698.
- Telemetry startup and user-info subscription at lines 598-622.
- Telemetry events and flush at lines 626-640 and 829-839.
- `feedback::init(cx)` at line 770.
- `onboarding::init(cx)` at line 774.
- `show_onboarding_view(...)` path around line 1635.
- `extensions_ui::init(cx)` at line 777 if extension marketplace UI is not in
  the first fork release.

## Keep

These init paths support retained core workflows and should stay during the
first pruning pass.

- Config and app shell: `settings::init`, `zlog_settings::init`,
  `zed_actions::init`, `menu::init`, `release_channel::init`, `gpui_tokio::init`.
- Files/projects/workspaces: trusted worktrees, `Fs` global setup,
  `project::Project::init`, `workspace::init`, recent projects.
- Language basics: language registry setup, `languages::init`,
  `language_extension::init`, `language_tools::init`.
- Editor and navigation: `editor::init`, `go_to_line::init`,
  `file_finder::init`, `tab_switcher::init`, `outline::init`,
  `project_symbols::init`, `project_panel::init`, `outline_panel::init`.
- Search: `search::init` and `PaneSearchBarCallbacks`.
- Terminal: `terminal_view::init`.
- Git: `GitHostingProviderRegistry`, `git_hosting_providers::init`,
  `git_ui::init`.
- Markdown and file previews: `markdown_preview::init`, `image_viewer::init`,
  `csv_preview::init`, `svg_preview::init`.
- UI and settings surfaces: `theme_settings::init`, `theme_extension::init`,
  `theme_selector::init`, `settings_profile_selector::init`,
  `settings_ui::init`, `keymap_editor::init`, `which_key::init`,
  `json_schema_store::init`.
- Extension host can stay temporarily if language support or bundled language
  functionality still depends on it.

## Defer Pending Product Decisions

- `auto_update::init` and `auto_update_ui::init`: keep until the fork's update
  strategy is decided.
- `dev_container::init`, remote project handling, and `RemoteConnectionOptions`:
  not part of the first removal list; decide separately based on terminal-agent
  workflow needs.
- `repl::init` and `repl::notebook::init`: not core to the fork, but they are
  lower priority than ACP/native-agent/collab/debugger removal.
- `extension::init`, `extension_host::init`, and `theme_extension::init`: keep
  temporarily, then narrow once language and theme extension requirements are
  clear.
