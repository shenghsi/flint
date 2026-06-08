# Terminal-First Flint Fork

## Purpose

This fork turns Flint into a terminal-first code review and writing workspace.
Codex, Claude Code, and similar tools run in real terminals. The IDE's job is
to organize those terminal sessions, show their code changes clearly, provide
fast editing when needed, and offer a stronger Markdown writing experience.

The fork should avoid becoming another native AI client. Agent authentication,
model selection, permissions, tools, subscriptions, and native configuration
belong to the CLI or TUI running inside the terminal.

## Product Shape

The first screen should feel like a workspace built around terminals and code
changes, not a traditional editor with a small terminal dock.

Core workflows:

- Start a Codex or Claude terminal thread from the main UI.
- Keep multiple agent terminal threads organized by project, title, status, and
  recent activity.
- Review generated changes through the git panel, diff views, and editor tabs.
- Make small manual edits without leaving the app.
- Generate commit messages through a configured external command.
- Write Markdown with a Typora-like inline editing experience.

## Terminal Is First Class

Terminal sessions should open as center workspace items, similar to files and
diffs. They should not default to the bottom dock.

Keep `TerminalView` as the primary terminal surface. It is already registered as
a serializable workspace item and supports main-pane usage. Treat the existing
terminal panel as temporary plumbing only if needed during migration.

Desired terminal behavior:

- `New Terminal Thread` opens a terminal in the center pane.
- `New Codex Thread` opens a center terminal running the configured Codex
  command.
- `New Claude Thread` opens a center terminal running the configured Claude
  command.
- Terminal tabs and splits are normal workspace content, not hidden panel
  content.
- Terminal titles and bell notifications are used to show agent status.
- Project panel and git panel stay available as side panels for navigation and
  review.

Terminal-thread settings:

```json
{
  "terminal_threads": {
    "codex": {
      "command": "codex",
      "args": [],
      "env": {},
      "cwd": null
    },
    "claude": {
      "command": "claude",
      "args": [],
      "env": {},
      "cwd": null
    },
    "shell": {
      "command": null,
      "args": [],
      "env": {},
      "cwd": null
    }
  }
}
```

`codex` and `claude` default to their matching executable names when no command
is configured. `shell` falls back to the normal terminal shell unless a command
override is provided. `cwd` can pin a launcher to a directory; otherwise the
workspace/project context is used.

## Organized Agent Threads

Agent threads are terminal-backed sessions, not ACP or native chat threads.

Keep or build a small thread organizer that owns:

- thread list and grouping by project
- terminal title/status display
- unread or attention state from terminal bell events
- quick reopen/focus of existing terminal sessions
- start actions for Codex, Claude, and plain shell terminals

The organizer should not own:

- model/provider configuration
- agent auth
- tool permission policy
- MCP forwarding
- ACP protocol sessions
- chat rendering
- agent transcript persistence beyond terminal/session state

## Keep

Keep these areas as core product functionality:

- App shell: windows, panes, tabs, docks, settings, themes, keymaps.
- Project navigation: project panel, file finder, search, outline if still
  useful.
- Editor: text editing, syntax highlighting, selections, search, language
  basics.
- Terminal: `terminal`, `terminal_view`, terminal persistence, terminal
  notifications, path hyperlinks.
- Git review: `git`, `git_ui`, git panel, staged/unstaged changes, diff views,
  branch/status UI.
- Diffs: `buffer_diff`, multi-buffer diff views, file diff views.
- Markdown: `markdown`, `markdown_preview`, Mermaid rendering if useful, image
  support if useful.
- Task launching if it supports terminal workflows without dragging in unwanted
  debugger or agent features.
- Extension host only if it remains necessary for language support or bundled
  language functionality.

## Remove

Remove these product areas from the fork:

- ACP: `acp_thread`, `acp_tools`, `agent_servers`, ACP registry, ACP imports,
  external-agent protocol handling.
- Native Flint agent/chat: agent panel chat, inline assistant, agent tools, tool
  permissions, model-backed text threads, ACP thread import/history.
- Native model providers: Anthropic, OpenAI, Google, Bedrock, Ollama,
  OpenRouter, DeepSeek, Mistral, LM Studio, xAI, OpenCode provider UI, Flint cloud
  model providers, and the full language model registry where it only serves
  native AI features.
- Copilot and edit prediction: `copilot*`, `edit_prediction*`.
- Collaboration and calls: `collab*`, `channel`, `call`, `audio`, `livekit*`,
  screen sharing, channel notes.
- Debugger: `dap*`, `debug_adapter_extension`, `debugger_tools`,
  `debugger_ui`, debugger panels, debug tasks UI.
- Flint cloud/product surfaces: account settings, hosted model onboarding,
  feedback, social/help links specific to Flint, telemetry UI.
- Extension marketplace UI if extension installation and management are not
  part of the fork's first product.

## Reshape

Some areas should be kept but narrowed.

### Git Commit Messages

Keep AI-assisted commit message generation, but do not keep Flint's full native
LLM stack just for this feature.

Replace the current `LanguageModelRegistry`-based flow with a small external
command runner:

- Git UI collects staged or worktree diff.
- Existing commit-message prompt text is reused.
- The prompt and compressed diff are sent to a configured command.
- The command writes the proposed commit message to stdout.
- The UI inserts stdout into the commit message editor.
- Non-zero exit status, stderr, timeout, and missing executable errors are
  surfaced in the UI.

Example setting shape:

```json
{
  "git": {
    "commit_message_generator": {
      "command": "codex",
      "args": [],
      "env": {},
      "cwd": null,
      "timeout_seconds": 30,
      "max_diff_bytes": 60000,
      "instructions": "Use concise imperative commit messages."
    }
  }
}
```

This should support `codex`, `claude`, or any user-provided command.
The generated prompt is sent to the command on stdin. Stdout is inserted into
the commit message editor. Missing commands, non-zero exits, stderr, timeouts,
and empty output are shown as git UI errors.

### Markdown

Keep existing Markdown parsing and preview code, but the Typora-like experience
should be treated as a new editor mode rather than just a preview pane.

Target behavior:

- Markdown files open in an editable rendered mode by default or by setting.
- Source syntax remains available and recoverable.
- Headings, lists, code fences, quotes, tables, images, links, and Mermaid
  blocks render inline.
- Cursor movement, selection, copy/paste, undo, search, and save remain editor
  quality.
- Preview pane remains optional for users who prefer split preview.

Implementation boundary:

- The existing `MarkdownPreviewView` remains a separate, read-only workspace
  item. It observes an editor, copies its source into a `Markdown` entity, and
  owns independent selection, search, and scrolling state, so it is not the
  editable rendered surface.
- Editable rendered mode is presentation state attached to the existing
  `Editor`. The editor's buffer remains the only source of truth.
- Reuse source ranges from `markdown::parser` to identify Markdown syntax and
  rendered regions. Inline syntax can use editor text styles, inlays, and
  source-range folds; tables, images, Mermaid diagrams, and other rich regions
  can use editor custom blocks.
- Presentation state must use buffer anchors and be rebuilt after edits.
  Switching to source mode removes Markdown-specific folds, inlays, styles, and
  blocks without replacing the editor item or buffer.
- Existing split and following preview actions remain available independently
  of editable rendered mode.

Current Markdown setting:

```json
{
  "markdown": {
    "open_mode": "editable_rendered"
  }
}
```

Use `"source"` to open Markdown files as source by default. The editor tab
context menu can switch an individual Markdown editor between rendered and
source modes without replacing the buffer.

Current implementation details:

- Inline presentation uses editor highlight layers over the source buffer.
- Tables, images, and Mermaid blocks are rendered as editor custom blocks
  anchored below their source ranges.
- Source mode clears Markdown-specific highlights and rich blocks.
- `.md`, `.markdown`, `.mdown`, `.mkd`, and `.mkdn` files are treated as
  Markdown even before language metadata finishes loading.

### Terminal Panel

Do not make the bottom terminal panel the default user-facing terminal model.

Possible migration path:

- Keep terminal panel internals temporarily if they are needed for terminal
  creation, persistence, or task integration.
- Move user actions toward center terminal items.
- Rename or remove menu entries that say `Terminal Panel`.
- Ensure new terminal actions open center items by default.
- Remove bottom-dock-specific terminal behavior once center terminal workflows
  are complete.

## Initial Implementation Phases

1. Product surface pruning

   Remove menus, settings defaults, onboarding links, command palette entries,
   and panel registration for removed features. Keep compilation broad at first.

2. Terminal-first defaults

   Make terminal creation open center `TerminalView` items by default. Add Codex
   and Claude terminal-thread launch actions. Remove bottom-panel-first UI.

3. Agent thread organizer

   Build or extract a small terminal-thread sidebar/list. It should manage
   terminal sessions only, without ACP or native agent chat.

4. Git commit message command runner

   Replace native LLM commit generation with an external command setting.
   Preserve the existing diff compression and prompt-building behavior where it
   is still useful.

5. Compile-prune removed crates

   Remove dependencies and workspace members after product surface references
   are gone. This should happen after the desired terminal and git paths are
   stable.

6. Markdown editing upgrade

   Build the Typora-like Markdown mode after the fork has a smaller, stable
   product surface.

## Important Code Entry Points

- App startup feature registration: `crates/flint/src/main.rs`
- Workspace panel loading: `crates/flint/src/flint.rs`
- App menus: `crates/flint/src/flint/app_menus.rs`
- Terminal item: `crates/terminal_view/src/terminal_view.rs`
- Terminal panel plumbing: `crates/terminal_view/src/terminal_panel.rs`
- Git commit message generation: `crates/git_ui/src/git_panel.rs`
- Commit message prompt: `crates/git_ui/src/commit_message_prompt.txt`
- Terminal-thread organizer and launchers:
  `crates/terminal_view/src/terminal_threads.rs`
- Markdown editable rendered mode:
  `crates/editor/src/markdown_actions.rs`

## Manual Verification

Fresh workspace:

- Open a project and confirm the project panel, editor, search, git panel,
  settings, themes, keymaps, terminal threads, and Markdown preview are
  available.
- Confirm removed surfaces are absent from default menus and command palette:
  ACP, native agent/chat, native model providers, Copilot/edit prediction,
  collaboration/calls, debugger/DAP panels, and Flint cloud product entries.

Terminal threads:

- Run `New Codex Thread`; confirm a center terminal item opens and runs the
  configured `terminal_threads.codex` command.
- Run `New Claude Thread`; confirm a center terminal item opens and runs the
  configured `terminal_threads.claude` command.
- Run `New Shell Thread`; confirm a center terminal item opens with the shell or
  configured override.
- Rename a terminal title from the running process and confirm the tab/thread
  organizer updates.
- Emit a terminal bell in a background terminal and confirm attention state is
  updated without stealing focus.

Git:

- Create unstaged changes and run generate commit message; confirm the external
  command receives worktree diff context and stdout fills the commit editor.
- Stage changes and run generate commit message; confirm staged diff context is
  used.
- Configure a missing command and confirm the git UI reports the missing
  executable.
- Configure a command that exits non-zero or writes stderr and confirm the error
  is surfaced.

Markdown:

- Open a `.md` file and confirm it opens in editable rendered mode by default.
- Switch to source mode and back from the editor tab context menu; confirm the
  source text is unchanged.
- Edit headings, emphasis, links, quotes, code fences, tables, images, and
  Mermaid blocks; confirm save, undo, redo, search, copy, and paste operate on
  the source buffer.
- Open split Markdown preview while editable rendered mode is active and confirm
  the preview follows or edits the same source buffer.

## Non-Goals

- Do not build a native replacement for Codex or Claude Code.
- Do not keep ACP as a hidden implementation detail.
- Do not keep native provider configuration only for future flexibility.
- Do not optimize for collaboration, calls, or debugger workflows.
- Do not make Markdown improvements depend on the agent system.

## Open Decisions

- Whether the terminal-thread organizer should live inside `terminal_view`,
  `workspace`, or a new focused crate.
- Whether extension installation is needed in the first fork release.
- Whether LSP features should remain full-featured or be trimmed to navigation
  and diagnostics.
- Whether terminal-thread state should persist across app restarts, and if so,
  whether process restoration or only session metadata should persist.
