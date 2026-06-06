## Context

The current application is a broad editor product with terminal, git, editor,
native AI, ACP external agents, collaboration, debugger, extension marketplace,
and cloud-facing surfaces. The fork's target user runs Codex, Claude Code, or a
similar coding agent in a terminal and uses the app to organize those sessions,
inspect file changes, make focused edits, generate commit messages, and write
Markdown.

The current terminal implementation already has useful primitives:
`TerminalView` is a workspace item and can be opened in the center pane, while
`TerminalPanel` provides terminal creation, persistence, task integration, and
bottom-dock behavior. The fork should promote the center item path and avoid
keeping the bottom panel as the main terminal model.

## Goals / Non-Goals

**Goals:**

- Make terminal sessions first-class center workspace items.
- Provide organized Codex, Claude, and shell terminal threads without ACP.
- Keep git review and diff workflows as primary companions to terminal agents.
- Generate commit messages through configured external commands.
- Preserve useful editor, project, search, settings, theme, and language basics.
- Add a Typora-like Markdown editing mode without depending on the agent system.
- Remove product surfaces and dependencies that do not support the focused fork.

**Non-Goals:**

- Build a native replacement for Codex or Claude Code.
- Keep ACP as an internal agent-thread implementation.
- Keep native LLM providers, Copilot, or edit prediction for future optionality.
- Preserve collaboration, calls, debugger, or Zed cloud workflows.
- Complete all crate pruning before terminal and git workflows are stable.

## Decisions

### Promote `TerminalView` to the Primary Terminal Surface

User-facing terminal actions will open center workspace items by default.
`TerminalPanel` may remain temporarily as internal plumbing for terminal
creation, persistence, and task integration, but the bottom dock is not the
primary model.

Alternative considered: keep the current bottom terminal panel and only change
its dock position. This was rejected because the fork's core workflow needs
terminals to behave like primary workspace content, not auxiliary output.

### Build Terminal Threads Without ACP

Organized Codex and Claude threads will be terminal-backed sessions. A small
thread organizer will track terminal sessions by project, configured command,
title, bell/attention state, and last activity. It will not own model
configuration, authentication, tool permissions, MCP forwarding, or chat
transcripts.

Alternative considered: reuse the ACP external-agent stack and hide ACP from the
user. This was rejected because ACP would retain a large native-agent dependency
surface that conflicts with the fork's terminal-owned agent boundary.

### Use External Commands for Commit Message Generation

Git commit message generation will call a configured command. The git UI will
continue to collect diffs and build a concise prompt, then pass that input to
the external process and insert stdout into the commit editor.

Alternative considered: keep `LanguageModelRegistry` and native model providers
only for commit messages. This was rejected because it preserves the native LLM
stack for a single feature and contradicts the CLI-owned AI model.

### Surface-Prune Before Compile-Prune

The first implementation pass should remove menus, settings defaults, panel
registration, startup initialization, deep links, and command palette exposure
for removed features. After core terminal and git workflows are stable, remove
workspace members and dependencies.

Alternative considered: delete all unwanted crates first. This was rejected
because many current features are cross-wired through app startup, project,
workspace, settings, and tests. Immediate crate deletion would turn the project
into dependency cleanup before validating the new product shape.

### Treat Markdown Inline Editing as a New Editor Mode

The Markdown work should reuse parsing and rendering code where useful, but the
Typora-like experience is an editable rendered mode, not just a preview pane.
The source buffer remains the source of truth, and source editing remains
available.

Alternative considered: only improve the existing split preview. This was
rejected because the requested experience is inline authoring, not better
preview fidelity alone.

## Risks / Trade-offs

- Terminal panel internals may be tightly coupled to bottom-dock behavior ->
  Mitigation: keep them temporarily while redirecting user actions to center
  terminal items, then remove panel-specific behavior once center workflows are
  complete.
- Removing native agent settings may break git UI commit generation ->
  Mitigation: replace commit-message settings with git-owned settings before
  removing `agent_settings`.
- ACP and native agent references may be spread through project and workspace ->
  Mitigation: remove UI/deep-link/startup references first, then prune project
  data structures and dependencies in smaller compile-verified steps.
- External command invocation can expose confusing failures ->
  Mitigation: surface missing executable, timeout, non-zero exit, and stderr
  clearly in the git UI.
- Markdown inline editing can become a large editor project ->
  Mitigation: stage it after terminal and git scope reduction, and preserve
  source editing and preview as fallback paths.

## Migration Plan

1. Remove or hide product surfaces for ACP, native agent/chat, native LLM
   providers, Copilot, edit prediction, collaboration, calls, debugger, and Zed
   cloud surfaces.
2. Change terminal actions and defaults so terminals open in center panes.
3. Add Codex, Claude, and shell terminal-thread launch actions and a small
   thread organizer.
4. Replace git commit message generation with a git-owned external command
   runner.
5. Compile-prune removed crates and dependencies in focused batches.
6. Build Markdown inline editing after the smaller product surface is stable.

Rollback for early phases is to retain hidden startup registration or feature
flags until center terminal workflows and git commit generation are verified.

## Open Questions

- Should the thread organizer live in `terminal_view`, `workspace`, or a new
  focused crate?
- Should terminal-thread state restore only metadata, or should it attempt to
  restore running processes where possible?
- Which extension functionality is required for the first fork release?
- Should LSP remain full-featured or be trimmed to diagnostics and navigation?
