## 1. Phase 1 — Delete purely AI crates

- [x] 1.1 Remove agent/ACP crates from Cargo.toml and delete directories: `acp_thread`, `acp_tools`, `agent`, `agent_servers`, `agent_settings`, `agent_skills`, `action_log`, `skill_creator`
- [x] 1.2 Remove LLM provider crates from Cargo.toml and delete directories: `anthropic`, `bedrock`, `codestral`, `deepseek`, `google_ai`, `lmstudio`, `mistral`, `ollama`, `open_ai`, `open_router`, `opencode`, `x_ai`
- [x] 1.3 Remove LLM abstraction crates from Cargo.toml and delete directories: `language_model`, `language_model_core`, `language_models`, `language_models_cloud`, `cloud_llm_client`, `cloud_api_client`
- [x] 1.4 Remove edit prediction crates from Cargo.toml and delete directories: `edit_prediction`, `edit_prediction_context`, `edit_prediction_metrics`, `edit_prediction_types`, `edit_prediction_ui`, `zeta_prompt`
- [x] 1.5 Remove copilot crates from Cargo.toml and delete directories: `copilot`, `copilot_chat`, `copilot_ui`
- [x] 1.6 Remove remaining AI crates from Cargo.toml and delete directories: `context_server`, `ai_onboarding`, `prompt_store`, `web_search`, `web_search_providers`
- [x] 1.7 Remove all deleted crates from `[workspace.dependencies]` in root Cargo.toml

## 2. Phase 2 — Fix `project` crate

- [x] 2.1 Delete `crates/project/src/agent_server_store.rs` and `crates/project/src/agent_registry_store.rs`
- [x] 2.2 Delete `crates/project/src/context_server_store/` directory (all files)
- [x] 2.3 Remove `context_server_store`, `agent_server_store`, `agent_registry_store` fields from `Project` struct in `project.rs`
- [x] 2.4 Remove `DisableAiSettings`, `AgentLocation`, `AgentLocationChanged` and all AI-related initialization from `project.rs`
- [x] 2.5 Remove `context_server` and other removed AI crates from `crates/project/Cargo.toml`
- [x] 2.6 Verify `cargo build -p project` compiles cleanly

## 3. Phase 3 — Fix `editor` crate

- [x] 3.1 Delete `crates/editor/src/edit_prediction.rs` and `crates/editor/src/edit_prediction_tests.rs`
- [x] 3.2 Remove all edit prediction fields (~15) from the `Editor` struct in `editor.rs`
- [x] 3.3 Remove all edit prediction methods (~20) from `editor.rs` and related `impl` blocks
- [x] 3.4 Remove edit prediction popover rendering from `element.rs`
- [x] 3.5 Remove `edit_prediction_types` and other removed crates from `crates/editor/Cargo.toml`
- [x] 3.6 Verify `cargo build -p editor` compiles cleanly and `cargo test -p editor` passes

## 4. Phase 4 — Fix `settings_content` and `default.json`

- [x] 4.1 Delete `crates/settings_content/src/agent.rs` and `crates/settings_content/src/language_model.rs`
- [x] 4.2 Remove `EditPredictionProvider` variants (Copilot, Flint, Codestral, Ollama, etc.) from `language.rs`, keeping only `None`
- [x] 4.3 Remove `EditPredictionSettingsContent` sub-structs (copilot, codestral, ollama, open_ai_compatible_api) from `language.rs`
- [x] 4.4 Remove `context_servers`, `context_server_timeout`, `agent_servers`, `disable_ai` fields from `project.rs`
- [x] 4.5 Remove `agent_ui_font_size`, `agent_buffer_font_size`, `show_edit_predictions`, `edit_predictions_disabled_in`, `edit_predictions`, `agent`, `language_models` fields from the top-level settings struct in `settings_content.rs`
- [x] 4.6 Remove `SaturatingBool` type and its `MergeFrom` impl from `settings_content.rs`
- [x] 4.7 Remove `disable_ai` special-case merging logic from `crates/settings/src/settings_store.rs`
- [x] 4.8 Remove AI settings keys from `assets/settings/default.json`: `agent_ui_font_size`, `agent_buffer_font_size`, `show_edit_predictions`, `edit_predictions_disabled_in`, the full `agent` block, the full `edit_predictions` block, the full `language_models` block, `context_server_timeout`, `context_servers`, `agent_servers`, `disable_ai`
- [x] 4.9 Verify `cargo build -p settings` and `cargo build -p settings_content` compile cleanly

## 5. Phase 5 — Fix `settings_ui`

- [x] 5.1 Delete AI settings page files: `src/pages/edit_prediction_provider_setup.rs`, `src/pages/skills_setup.rs`, `src/pages/tool_permissions_setup.rs`
- [x] 5.2 Delete AI component files: `src/components/ollama_model_picker.rs`
- [x] 5.3 Remove all AI settings sections from the main `settings_ui.rs` file
- [x] 5.4 Remove removed AI crates from `crates/settings_ui/Cargo.toml`
- [x] 5.5 Verify `cargo build -p settings_ui` compiles cleanly (crate excluded from workspace)

## 6. Phase 6 — Fix `client` and `cloud_api_types`

- [x] 6.1 Delete `crates/client/src/llm_token.rs`
- [x] 6.2 Remove `cached_llm_token()`, `refresh_llm_token()` methods and cloud LLM client imports from `client.rs`
- [x] 6.3 Remove `edit_prediction_usage` field and `EditPredictionUsage`/`RequestUsage` structs from `user.rs`
- [x] 6.4 Remove `edit_prediction_docs()`, `acp_registry_blog()`, `shared_agent_thread_url()` from `flint_urls.rs`
- [x] 6.5 Strip AI-specific types from `cloud_api_types` (LLM token types, streaming types), keeping `Plan` and billing types
- [x] 6.6 Remove `cloud_llm_client`, `cloud_api_client` from `crates/client/Cargo.toml`
- [x] 6.7 Verify `cargo build -p client` compiles cleanly

## 7. Phase 7 — Fix remaining crates

- [x] 7.1 `flint_actions`: Remove `pub mod agent` and `agents_sidebar` module blocks; remove `InlineAssist` action
- [x] 7.2 `workspace`: Remove `ToggleEditPrediction`, agent panel position, `handle_agent_location_changed`, `active_item_for_agent`; remove `is_agent_panel()` from `Panel` trait in `dock.rs`
- [x] 7.3 `onboarding`: Remove agent installation UI from `basics_page.rs`; remove agent state from `onboarding.rs`
- [x] 7.4 `extensions_ui`: Remove `ExtensionProvides::ContextServers`/`AgentServers` labels and configure buttons; remove featured external agent links
- [x] 7.5 `extension` + `extension_host`: Remove context server and language model provider proxy fields and registration methods
- [x] 7.6 `vim`: Remove `accept_edit_prediction` keybinding handler and `hide_edit_predictions` field
- [x] 7.7 `language`: Remove `EditPredictionSettings`, `CopilotSettings`, `show_edit_predictions` logic from `language_settings.rs`
- [x] 7.8 `language_tools`: Remove copilot-enabled check from `lsp_button.rs`
- [x] 7.9 `feature_flags`: Remove `AgentSharing`, `AgentMultithread`, `AgentThreadWorktreeLabel`, `AgentSandbox` feature flag structs
- [x] 7.10 `settings` vscode import: Remove `agent_settings_content()`, `edit_predictions_settings_content()`, `context_servers()` import helpers from `vscode_import.rs`
- [x] 7.11 `title_bar`/`quick_action_bar`: Remove edit predictions context menu entry from `quick_action_bar.rs`
- [x] 7.12 `collab` server: Remove `share_agent_thread`/`get_shared_agent_thread` RPC handlers and AI-related extension version fields
- [x] 7.13 Verify `cargo build` for all modified crates compiles cleanly

## 8. Final validation

- [x] 8.1 Run `cargo build` on the full workspace — zero errors, zero AI-related warnings
- [x] 8.2 Run `./script/clippy` — passes cleanly
- [x] 8.3 Run `cargo test` across all remaining crates — all tests pass
- [x] 8.4 Confirm `default.json` contains no AI keys
- [x] 8.5 Confirm `test_settings()` in `settings_file.rs` no longer needs `disable_ai`/`enable_next_edit_suggestions` overrides — remove them
