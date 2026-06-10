## ADDED Requirements

### Requirement: No AI crates in workspace
The Flint workspace SHALL contain no crates whose sole purpose is AI functionality (LLM providers, agent, copilot, edit predictions, MCP servers, AI onboarding, prompt storage, web search).

#### Scenario: Workspace compiles without AI dependencies
- **WHEN** `cargo build` is run on the Flint workspace
- **THEN** no crate related to language models, copilot, agent, edit predictions, or MCP is compiled

#### Scenario: No AI symbols in binary
- **WHEN** the Flint binary is inspected
- **THEN** no symbols from removed AI crates are present

### Requirement: Editor has no inline edit prediction code
The `editor` crate SHALL contain no code for inline AI edit predictions, including no `edit_prediction.rs` module, no edit prediction fields on the `Editor` struct, and no edit prediction rendering in the element layer.

#### Scenario: Editor compiles without edit_prediction_types
- **WHEN** `cargo build -p editor` is run
- **THEN** compilation succeeds with no reference to `edit_prediction_types`, `EditPredictionDelegate`, or `CopilotEditPrediction`

#### Scenario: No edit prediction tests
- **WHEN** `cargo test -p editor` is run
- **THEN** no tests from `edit_prediction_tests.rs` are present

### Requirement: Project crate has no AI infrastructure
The `project` crate SHALL contain no `context_server_store`, `agent_server_store`, or `agent_registry_store` modules, and the `Project` struct SHALL have no fields for these subsystems.

#### Scenario: Project compiles without context_server crate
- **WHEN** `cargo build -p project` is run
- **THEN** compilation succeeds with no reference to `context_server` or `ContextServerStore`

### Requirement: Settings contain no AI keys
The settings system SHALL contain no AI-specific settings types (`AgentSettings`, `LanguageModelSettings`, `EditPredictionSettings`, `CopilotSettings`) and `default.json` SHALL contain no `agent`, `edit_predictions`, or `language_models` blocks.

#### Scenario: Default settings parse without AI keys
- **WHEN** `default.json` is parsed
- **THEN** no `agent`, `edit_predictions`, `language_models`, `disable_ai`, `show_edit_predictions` keys are present

### Requirement: No disable_ai setting
The `disable_ai` setting and `SaturatingBool` gating mechanism SHALL be removed from the codebase, as they have no meaning once AI code is absent.

#### Scenario: Settings compile without disable_ai
- **WHEN** `cargo build -p settings` is run
- **THEN** no `disable_ai` field or `SaturatingBool` type is present in settings types

### Requirement: All remaining tests pass
After AI code removal, all non-AI tests SHALL continue to pass without modification.

#### Scenario: Full test suite passes
- **WHEN** `cargo test` is run across the workspace (excluding removed crates)
- **THEN** all remaining tests pass with no failures introduced by the removal
