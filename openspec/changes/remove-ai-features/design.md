## Context

Flint is a fork of Zed. The upstream Zed codebase has grown an extensive AI surface: ~36 crates covering LLM provider clients, a chat agent, GitHub Copilot, inline edit predictions, MCP context servers, and related UI and settings. These crates were disabled in Flint via `disable_ai: true` in `default.json`, but remained compiled and tested. The goal is permanent removal so Flint compiles and ships with none of this code.

The work is purely subtractive — no new behavior is introduced. The challenge is that AI code is both concentrated (dedicated crates) and diffuse (fields and methods scattered throughout `editor`, `project`, `settings_content`).

## Goals / Non-Goals

**Goals:**
- Remove all AI crates from the Cargo workspace
- Remove AI-specific fields, methods, and modules from mixed crates (`editor`, `project`, `settings_content`, `client`, `workspace`, `settings_ui`, etc.)
- Clean up `default.json` and settings types of all AI keys
- Ensure the binary compiles cleanly with no AI code paths remaining
- Remove AI-specific tests; keep and maintain all other tests

**Non-Goals:**
- Changing any non-AI editor behavior
- Removing auto-update (not AI; separate distribution decision)
- Removing telemetry (separate privacy decision)
- Removing the extension system (non-AI; extensions may add non-AI tooling)
- Keeping any "stub" or "disabled" AI code for future re-enablement

## Decisions

**Delete crates wholesale, then fix compilation errors.**
Rather than surgically editing each file first, delete the 36 purely-AI crates from `Cargo.toml` and their directories. The resulting compile errors are the authoritative guide to exactly what needs cleanup in mixed crates. This is faster than manual auditing and avoids leaving orphaned code.

**Phased execution, one compilable state between phases.**
Each phase ends with the codebase compiling. This makes it possible to run the test suite incrementally and catch regressions early. Phase order: (1) delete AI crates → (2) fix `project` → (3) fix `editor` → (4) fix `settings_content`/`default.json` → (5) fix `settings_ui` → (6) fix `client`/`cloud_api_types` → (7) fix remaining small crates.

**Keep `cloud_api_types` reduced, not deleted.**
`cloud_api_types` contains both AI types (LLM token, streaming) and non-AI types (`Plan`, billing). Rather than deleting the crate and migrating `Plan` everywhere, strip the AI content and keep the crate. Revisit deletion in a follow-up if `Plan` display is also removed.

**Remove `disable_ai` setting entirely.**
Once all AI code is gone, the `SaturatingBool` `disable_ai` field in settings has no meaning. Remove it rather than keeping a dead setting.

**Remove accumulated test fixes for AI features.**
Test fixes made during the "disabled by default" phase (copilot tests, edit prediction tests, MCP tool tests, agent tests) are for features being removed. Delete these tests along with the feature code.

## Risks / Trade-offs

`settings_ui` is the largest and most complex file (~6,148 lines) with AI settings sections scattered throughout → Tackle last after all AI settings types are gone; compile errors will locate every reference precisely.

`cloud_api_types` is used by `client`, `title_bar`, `extension`, `extensions_ui` for both AI and non-AI content → Strip AI content from the crate rather than deleting it; verify `Plan` and `ExtensionProvides` (non-AI fields only) still work correctly.

Upstream sync will conflict in removed files → Deleted crates/files will produce clean conflicts (upstream adds, we delete); resolve by re-deleting. Mixed-crate cleanups are more work to re-apply after a sync.

`edit_prediction.rs` in `editor` is 2,600 lines tightly coupled to the `Editor` struct → Use compile errors as the guide; do not attempt to manually trace all references before deleting.

## Migration Plan

No user-facing migration needed — users of Flint never had AI features enabled. The settings keys being removed were already ignored (default `disable_ai: true` meant they had no effect).

For the codebase:
1. One PR per phase, keeping main compilable between merges
2. Each PR removes the crates/code for that phase and fixes all resulting compile errors
3. Run `./script/clippy` after each phase to catch warnings
4. Run the full test suite after Phase 4 (settings cleanup) to confirm no regressions

## Open Questions

- Should auto-update be removed in this change or a follow-up? (Current decision: separate)
- Should telemetry be removed? (Current decision: separate)
- Does `language_onboarding` (Python LSP banner, not AI) belong in this removal? (Current decision: keep it — it's not AI)
