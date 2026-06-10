## Why

Flint's goal is a lean editor that intentionally excludes AI. The current codebase carries ~36 AI-specific crates (agent, copilot, language models, MCP servers, edit predictions) that are disabled by default but still compiled, tested, and maintained on every upstream sync — adding ongoing cost with no user value.

## What Changes

- **BREAKING**: Remove all AI crates from the workspace (agent, copilot, LLM providers, edit predictions, MCP/context servers, AI onboarding, prompt store, web search)
- Remove AI-specific code from mixed crates: `editor` (inline edit predictions), `project` (context server store, agent registry), `settings_content` (AI settings types), `client` (LLM token handling), `workspace`, `vim`, `language`, `extensions`
- Strip ~350 lines of AI settings from `assets/settings/default.json`
- Remove AI-related settings UI pages and sections from `settings_ui`
- Remove `disable_ai` setting and `SaturatingBool` gating (no longer needed once code is gone)
- Remove AI-related test infrastructure and test fixes accumulated during the disabled-by-default phase

## Capabilities

### New Capabilities

- `lean-editor-core`: Editor codebase with all AI dependencies removed — compiles and ships without any LLM, copilot, agent, edit prediction, or MCP code

### Modified Capabilities

<!-- No existing specs have requirement changes — this is purely removal -->

## Impact

- ~36 crates deleted from `crates/` and removed from root `Cargo.toml`
- Significant surgery in `editor`, `project`, `settings_content`, `settings_ui`, `client`
- Binary size reduction; faster compile times; simpler upstream sync process
- All AI-related tests removed; remaining test suite covers only core editor functionality
- `cloud_api_types` partially reduced (keep `Plan`/billing types, remove LLM types)
