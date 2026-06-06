## Why

Zed's current product surface includes collaboration, native AI, ACP, debugger,
and cloud-oriented workflows that are not needed for a terminal-first fork. This
change focuses the application around terminal-run coding agents, code change
review, lightweight commit assistance, and a stronger Markdown writing
experience.

## What Changes

- Make terminals first-class center workspace items instead of bottom-dock-first
  panels.
- Add organized terminal-backed Codex, Claude, and shell threads grouped by
  project and status.
- Remove ACP, native Zed agent/chat, native model provider, Copilot, edit
  prediction, collaboration, calls, debugger, and Zed cloud/product surfaces.
- Replace native LLM-backed git commit message generation with a configurable
  external command that can call `codex`, `claude`, or another user-provided
  command.
- Improve Markdown authoring with an editable rendered mode inspired by Typora
  while preserving source editing and existing preview workflows.
- Keep editor, project navigation, git review, diff, search, terminal, settings,
  themes, and language basics as core surfaces.

## Capabilities

### New Capabilities

- `focused-product-surface`: Defines which upstream Zed product areas are
  removed, hidden, or retained for the terminal-first fork.
- `terminal-first-workspace`: Defines terminal-as-center-item behavior and
  terminal-first defaults.
- `terminal-agent-threads`: Defines organized terminal-backed Codex, Claude, and
  shell thread management without ACP.
- `external-command-commit-messages`: Defines git commit message generation via
  configured external commands instead of native LLM providers.
- `markdown-inline-editing`: Defines the Typora-like Markdown authoring
  experience.

### Modified Capabilities

No existing OpenSpec capabilities are present.

## Impact

- App startup and registration: `crates/zed/src/main.rs`
- Workspace panel loading and actions: `crates/zed/src/zed.rs`
- App menus: `crates/zed/src/zed/app_menus.rs`
- Terminal UI and persistence: `crates/terminal_view/src/terminal_view.rs`,
  `crates/terminal_view/src/terminal_panel.rs`
- Git UI commit generation: `crates/git_ui/src/git_panel.rs`,
  `crates/git_ui/src/commit_message_prompt.txt`
- Markdown rendering and preview: `crates/markdown`, `crates/markdown_preview`
- Workspace dependencies and crate pruning: root `Cargo.toml`, crate
  `Cargo.toml` files, settings defaults, action registration, and keymap schema
  generation.
