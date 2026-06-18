## Why

The current Codex/Claude terminal-thread organizer
(`crates/terminal_view/src/terminal_threads.rs`) only tracks threads that are
open right now in this app session, inside a full-pane tab the user has to
explicitly open. It groups across every open project rather than scoping to
the project you're in, and has no way to find or resume a conversation after
its terminal closes or the app restarts — even though both `claude` and
`codex` already persist resumable session history to disk. Users want a
docked, always-available panel scoped to the current project, with shell
dropped (it has no resumable history and isn't an agent conversation), and a
way to resume a past conversation with extra CLI flags like
`--dangerously-skip-permissions`.

## What Changes

- **BREAKING**: Remove the full-pane `TerminalThreadOrganizer` item, the
  `OpenTerminalThreads` action, and `TerminalThreadKind` /
  `TerminalThreadStore` / `TerminalThreadSettings` from `crates/terminal_view`
- Add a new `agent_threads` crate providing a docked sidebar panel
  (`AgentThreadsPanel`, default left dock) scoped to the current workspace's
  project
- Add a code-level agent registry (Codex, Claude; no shell) so adding a future
  agent kind is an isolated registry entry, not a restructuring
- Add per-agent history providers that read persisted session data
  (`~/.claude/history.jsonl`; `~/.codex/sessions/**` joined with
  `session_index.jsonl`), honoring `CLAUDE_CONFIG_DIR`/`CODEX_HOME`
  overrides, resolving the home directory correctly on macOS, Linux, and
  Windows, and resolving it on the correct host (local or remote) for SSH
  projects via `project.fs()` and the existing remote-environment resolver
- Merge live (currently-open terminal) and historical (resumable, on-disk)
  threads per agent section, deduplicated by exact session id (resumed
  threads) and a same-kind/same-project/launch-time heuristic (brand-new
  threads)
- Cap each agent section to a configurable number of visible threads
  (default 5, `agent_threads.max_visible_threads_per_agent`), with a "Show
  more" row and an independent fold/unfold control per section
- Add a way to resume a historical thread with extra CLI flags (e.g.
  `--dangerously-skip-permissions` for Claude,
  `--dangerously-bypass-approvals-and-sandbox` for Codex) via a per-row
  context menu, defined data-driven per agent kind
- Rename the `terminal_threads` settings key to `agent_threads` (codex/claude
  only, no migration alias)

## Capabilities

### Modified Capabilities

- `terminal-agent-threads`: now a docked, project-scoped panel with
  historical resume, capped/foldable sections, and resume-with-options,
  instead of a full-pane live-only organizer

## Impact

- Removed: `crates/terminal_view/src/terminal_threads.rs`,
  `crates/settings_content/src/terminal_threads.rs`, the 4 related menu items
  in `crates/flint/src/flint/app_menus.rs`, the `"terminal_threads"` block in
  `assets/settings/default.json`
- Added: `crates/agent_threads` crate (registry, history providers, panel,
  store), `crates/settings_content/src/agent_threads.rs`,
  `agent_threads::{Toggle, ToggleFocus}` in `flint_actions`, panel
  registration in `crates/flint/src/flint.rs` `initialize_panels`
- No settings migration: existing user overrides under `terminal_threads`
  silently stop applying
