## 1. Product Surface Pruning

- [x] 1.1 Audit startup initialization in `crates/zed/src/main.rs` and list init calls for ACP, native agent/chat, native model providers, Copilot, edit prediction, collaboration, calls, debugger, feedback, onboarding, telemetry UI, and extension marketplace UI.
- [x] 1.2 Remove or guard removed feature init calls while keeping terminal, editor, project, git, search, settings, themes, keymaps, Markdown, and language basics initialized.
- [x] 1.3 Update workspace panel loading in `crates/zed/src/zed.rs` so removed feature panels are not loaded or registered.
- [x] 1.4 Update `crates/zed/src/zed/app_menus.rs` to remove menu entries for removed features and rename terminal menu entries away from bottom-panel language.
- [x] 1.5 Remove or disable deep links and URL handling for ACP/native agent entry points in `crates/zed/src/zed/open_listener.rs`.
- [x] 1.6 Update default settings to remove or disable native agent, ACP, native model provider, Copilot, edit prediction, collaboration, call, debugger, and Zed cloud defaults.
- [x] 1.7 Add smoke coverage or action metadata verification that removed panels and menu entries are not exposed in a fresh workspace.

## 2. Terminal-First Workspace

- [x] 2.1 Trace current `TerminalView::deploy`, `NewTerminal`, `NewCenterTerminal`, and `TerminalPanel` creation paths.
- [x] 2.2 Change user-facing new terminal actions so the default path opens a center `TerminalView` item.
- [x] 2.3 Preserve terminal creation, task spawning, working directory resolution, and persistence while avoiding bottom-dock-first behavior.
- [x] 2.4 Update terminal menus, command palette names, and keybinding descriptions to describe terminal items or terminal threads instead of terminal panels.
- [x] 2.5 Ensure terminal tab titles update from terminal title events in center workspace items.
- [x] 2.6 Ensure terminal bell events mark background center terminal items as needing attention without stealing focus.
- [x] 2.7 Add tests for opening a terminal in the center pane, splitting a terminal beside another item, and restoring terminal layout metadata.

## 3. Terminal Agent Threads

- [x] 3.1 Choose the module boundary for terminal-thread organization after checking coupling in `terminal_view`, `workspace`, and sidebar code.
- [x] 3.2 Define terminal-thread settings for Codex, Claude, and shell launch commands, arguments, environment, and working directory behavior.
- [x] 3.3 Add `New Codex Thread`, `New Claude Thread`, and `New Shell Thread` actions that spawn real center terminal sessions.
- [x] 3.4 Implement thread metadata tracking for project identity, terminal item handle, configured thread kind, title, attention state, and last activity.
- [x] 3.5 Add a thread organizer UI that lists terminal-backed threads and focuses existing terminal items instead of spawning duplicates.
- [x] 3.6 Wire terminal title changes and bell events into the thread organizer's displayed title and attention state.
- [x] 3.7 Verify Codex and Claude thread launchers work without ACP registry, ACP server settings, native model providers, or native agent credentials.
- [x] 3.8 Add tests for launching Codex, launching Claude, grouping threads by project, updating attention state, and refocusing an existing thread.

## 4. External Command Commit Messages

- [x] 4.1 Extract git commit message generation settings into git-owned settings instead of `agent_settings`.
- [x] 4.2 Implement an external command runner for commit message generation with stdin input, stdout capture, stderr capture, timeout, and cancellation.
- [x] 4.3 Reuse existing git diff selection behavior so staged changes are used when present and worktree changes are used otherwise.
- [x] 4.4 Preserve or adapt existing diff compression before sending prompt input to the command.
- [x] 4.5 Preserve the existing commit-message prompt constraints and include project/user commit-message rules where available.
- [x] 4.6 Replace `LanguageModelRegistry` usage in `crates/git_ui/src/git_panel.rs` with the external command runner.
- [x] 4.7 Update the generate commit message button state and tooltip for configured, missing, running, failed, and canceled command states.
- [x] 4.8 Surface missing executable, non-zero exit, stderr, timeout, and cancellation errors in the git UI.
- [x] 4.9 Add tests for staged diff input, worktree diff input, large diff compression, stdout insertion, missing command, non-zero exit, stderr display, and timeout handling.

## 5. Compile-Prune Retired Dependencies

- [ ] 5.1 Remove ACP and native agent workspace members after their startup, UI, project, and settings references are gone.
- [ ] 5.2 Remove native model provider, Copilot, edit prediction, and web search workspace members after git commit generation no longer depends on native AI.
- [ ] 5.3 Remove collaboration, call, audio, LiveKit, channel, debugger, and DAP workspace members after related UI and project references are gone.
- [ ] 5.4 Remove stale actions, settings schemas, keybindings, tests, docs references, and assets for retired features.
- [ ] 5.5 Run targeted builds or checks after each removal batch to keep dependency cleanup bisectable.
- [ ] 5.6 Run `./script/clippy` or a scoped equivalent once the major removal batches compile.

## 6. Markdown Inline Editing

- [ ] 6.1 Audit `crates/markdown`, `crates/markdown_preview`, and editor rendering APIs for reusable parsing and rendering primitives.
- [ ] 6.2 Define a Markdown editable rendered mode setting and open behavior for Markdown files.
- [ ] 6.3 Implement inline rendering for headings, emphasis, lists, quotes, links, and code fences while preserving source-buffer edits.
- [ ] 6.4 Implement inline rich block handling for tables, images, and Mermaid blocks where existing rendering support is available.
- [ ] 6.5 Preserve source view switching with no data loss.
- [ ] 6.6 Preserve editor-quality cursor movement, selection, copy, paste, undo, redo, search, and save behavior in editable rendered mode.
- [ ] 6.7 Keep existing split Markdown preview available as an optional workflow.
- [ ] 6.8 Add Markdown editing tests for source switching, inline edits, undo/redo, search, copy/paste, rich blocks, and preview coexistence.

## 7. Verification and Documentation

- [ ] 7.1 Update `docs/terminal-first-fork.md` if implementation decisions differ from the proposal.
- [ ] 7.2 Document terminal-thread settings and external commit message generator settings.
- [ ] 7.3 Add manual verification steps for a fresh workspace, terminal thread launch, Codex/Claude command execution, git diff review, commit message generation, and Markdown editing.
- [ ] 7.4 Verify removed product surfaces are absent from menus, command palette, settings defaults, startup panels, and restored workspaces.
- [ ] 7.5 Verify retained workflows still work: project open, file finder, search, editor edits, git status/diff, terminal tabs/splits, settings, themes, keymaps, and Markdown preview.
