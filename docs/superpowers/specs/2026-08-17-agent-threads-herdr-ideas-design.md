# Agent Threads: task grouping and cross-project attention

## Status

This is a design sketch, not an approved plan. No implementation work has
started. The goal is to record two ideas clearly enough that a later session
can pick one and build it, without re-deriving this analysis.

## Origin

These ideas come from [herdrdev/herdr](https://github.com/herdrdev/herdr), a
terminal-based runtime for coding agents. Herdr uses a Session → Workspace →
Tab → Pane → Agent hierarchy. Two of its mechanisms looked possibly
transferable to Flint's `AgentThreadsPanel`:

1. Tabs group panes by concern inside one workspace (for example "agents",
   "logs", "review").
2. State rollup: each pane reports `idle / working / blocked / done /
   unknown`. This state bubbles up to the tab, the workspace, and a
   cross-workspace sidebar, so a user scanning many projects can jump
   straight to "what needs a decision."

A third herdr mechanism, an agent-to-agent automation API (`agent wait
--until blocked`, named panes addressable from a script), is a bigger,
separate design commitment (naming scheme, a socket protocol) and is
deliberately out of scope for this document.

## Flint's current model

Facts below are read directly from the code, not assumed from memory.

- `AgentThreadsPanel` is a `Panel` (`crates/agent_threads/src/panel.rs:1974`)
  that lives in one `Workspace`'s `Dock`. Its `render` method
  (`panel.rs:2053`) groups threads into sections by CLI kind (Codex, Claude,
  Pi, OpenCode) via `render_section` (`panel.rs:1216`), scoped to that
  workspace's own `project_roots`. There is no sub-grouping above
  individual threads today, and no concept of a project-spanning "tab."
- `MultiWorkspace` (`crates/workspace/src/multi_workspace.rs:248`) can hold
  several `Workspace` entities in one window at once, in
  `retained_workspaces`. Only the `active_workspace` renders;
  `MultiWorkspace::activate` (`multi_workspace.rs:1522`) swaps which one is
  active. There is no visible tab strip; this exists to make switching
  projects fast without losing terminal/agent state, not as a user-facing
  grouping UI.
- Because each `Workspace` owns its own `Dock`, each retained workspace has
  its own independent `AgentThreadsPanel` instance and its own independent
  thread sections. A user must switch the active workspace to see another
  project's threads.
- A thread row carries no idle/working/blocked state. `AgentThreadRow`
  (`crates/agent_threads/src/store.rs:73`) is only `Historical` or
  `FreshLive`. A live row always renders one static green "running" dot
  (`panel.rs:1559`), regardless of whether the underlying process is
  actively working or sitting idle at a prompt.
- The only attention signal Flint has today is a one-shot PTY bell. An
  agent's Stop hook writes `\a`; Alacritty turns this into
  `TerminalBackendEvent::Bell` → `Event::Bell`
  (`crates/terminal/src/terminal.rs:1430`), which `terminal_view`
  (`crates/terminal_view/src/terminal_view.rs:1275`) turns into one OS
  notification and one window-attention request. Nothing is stored: once
  the bell fires and the notification is shown, there is no persisted
  "this thread is blocked" flag to read later. This differs from herdr's
  blocked detection, which pattern-matches the terminal's bottom buffer
  against known approval/question UI on every state check, not just on a
  bell character.

## Idea A: tab-grouping inside a project

Goal: let a user group threads inside one project by task (for example
"backend", "review"), not only by CLI kind, mirroring herdr's Tab layer.

Sketch:

- Add a group tag to `AgentThreadMetadata` (`store.rs:45`) and to
  `HistoricalThread` (`crates/agent_threads/src/history.rs:23`).
- Change `render_section` (`panel.rs:1216`) to filter its rows by the
  active group tag, in addition to the `project_roots` filter it already
  applies. Sections stay per-kind under each group, or the group becomes
  the top-level grouping and kind becomes a sub-grouping — either is a
  UI decision, not an architectural one.
- Add a group switcher above the section list (a chip row or dropdown).
- Let the user set a thread's group at launch (a picker in the "new
  thread" flow) or move it after launch. The existing right-click handoff
  menu (`this.deploy_handoff_menu`, `panel.rs:1549`) is a natural place to
  add a "Move to group" entry, since it already targets one thread by its
  metadata.
- Persist the group tag the same way other thread metadata already
  persists, so it survives restart.

Cost and shape: additive. One new persisted field, one new small UI for
creating/renaming/deleting groups, one new setting for default groups. Stays
entirely inside the `agent_threads` crate. No new state model, no new
cross-crate wiring.

## Idea B: state rollup to a cross-project sidebar

Goal: one place shows "what needs me" across every open project, mirroring
herdr's sidebar.

This idea is blocked by a real gap, not just missing UI: "needs me" is not a
stored state in Flint today, it is a one-shot event (the bell). Rolling
anything up requires turning that event into state first.

Sketch:

- Add a persisted `needs_attention` flag, keyed by thread (terminal item id
  or session id). Set it `true` when the bell event fires. Clear it when
  the user opens that thread (focuses its terminal item in the panel).
  This touches three places: where the bell event lands today
  (`terminal_view.rs:1275`), the agent thread store
  (`crates/agent_threads/src/store.rs`), and the panel row render
  (`panel.rs:1515` `render_live_row` and its historical-row counterpart).
- A rollup across projects needs a read point above single-`Workspace`
  scope. `MultiWorkspace` already holds every retained workspace
  (`multi_workspace.rs:568` `retained_workspaces`), so it is the natural
  place to read `needs_attention` flags from across projects. It has no
  view UI for this today.
- Two placement choices for the rollup UI:
  1. A new small badge or list owned by `MultiWorkspace` itself, separate
     from the per-workspace `AgentThreadsPanel`. Simpler to scope, but is
     a new UI surface with its own render path.
  2. Extend `AgentThreadsPanel` so it can optionally also list flagged
     threads from other retained workspaces, with a "jump to project"
     action that calls `MultiWorkspace::activate`
     (`multi_workspace.rs:1522`) to switch there. Reuses existing row
     rendering, but blurs the panel's current one-workspace scope.
- If Idea A ships first, its group tag is a natural unit for the rollup to
  organize by (for example, badge count per group across projects), rather
  than the rollup inventing its own grouping concept.

Cost and shape: larger than Idea A. It introduces a new persisted state
model (event → flag) that does not exist today, plus new cross-workspace
read wiring through `MultiWorkspace` that also does not exist today. It also
reaches into `terminal_view` and `workspace`, not just `agent_threads`.

## Not covered here

Herdr's agent-to-agent automation API (naming scheme, `agent wait --until
blocked`, a socket protocol) is a separate, larger design decision and is
intentionally not sketched in this document.

## Open questions for whoever picks this up

- Does "needs attention" mean only the bell (agent finished / is waiting),
  or should it also cover a thread that errored or exited unexpectedly?
  This changes what `needs_attention` must track.
- Should a group (Idea A) be scoped per-project, or should the same group
  name span projects (so "review" pulls threads from every open project
  into one view)? The latter starts to overlap with Idea B's cross-project
  reach.
- For Idea B's placement choice 2, does extending `AgentThreadsPanel` to
  show foreign-workspace rows conflict with its existing assumption
  (`panel.rs:2057`-`2058`) that it always renders against one
  `workspace.project()`?
