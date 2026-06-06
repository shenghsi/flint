## Terminal Thread Boundary Audit

Task: 3.1

## Existing Coupling Checked

- `crates/terminal_view` already owns `TerminalView`, terminal task spawning,
  terminal title/bell events, and center workspace item behavior.
- `crates/workspace` owns pane/item composition and action registration, but it
  should not own Codex/Claude-specific launch settings or terminal thread
  metadata.
- Existing thread sidebar concepts in `crates/workspace/src/multi_workspace.rs`
  and `crates/agent_ui` are tied to native agent/ACP thread state, ACP session
  IDs, agent settings, and agent sidebar lifecycle.

## Chosen Boundary

Terminal-backed thread organization lives in
`crates/terminal_view/src/terminal_threads.rs`.

This module owns:

- Codex, Claude, and shell thread launch settings.
- `New Codex Thread`, `New Claude Thread`, `New Shell Thread`, and terminal
  thread organizer actions.
- Center terminal session launch through existing project terminal creation.
- Thread metadata for project name, terminal item ID, thread kind, title,
  attention state, and last activity.
- A center workspace item organizer that lists terminal-backed threads and
  focuses existing terminal items.

Workspace remains responsible only for registering actions and focusing panes.
The module does not import ACP, native agent UI, native model provider
registries, or ACP server settings.

## Rationale

Keeping the boundary in `terminal_view` lets terminal threads reuse terminal
events, terminal item handles, and terminal launch behavior without reviving the
removed ACP/native-agent sidebar. It also keeps future pruning simpler: ACP and
native agent crates can be removed without moving terminal thread code again.
