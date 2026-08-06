# Agent-Created Worktree Threads Design

**Date:** 2026-08-06
**Status:** Draft for written-spec review

No implementation is authorized by this design review alone.

## Problem

Flint can create and retain linked Git worktrees in one window, and Agent
Threads can run a CLI inside any loaded `Workspace`. Those two features do not
currently share enough identity or navigation state.

The failure is easiest to see when an agent running in `main` delegates a
feature to another agent in a new worktree:

1. `git_ui::create_worktree_workspace` creates the linked worktree and adds its
   `Workspace` to `MultiWorkspace` in the background.
2. The child Agent Thread starts in that workspace while the user remains in
   `main`.
3. `AgentThreadMetadata` records only one `project_root`; it does not record a
   stable Flint thread id, an owning workspace identity, a worktree label, or a
   parent thread.
4. The active `AgentThreadsPanel` asks for threads matching only the active
   project's visible roots and renders each live row as a status dot and title.
5. `AgentThreadStore::focus_thread` activates the terminal item inside its
   owning `Workspace`, but it does not first make that workspace active in
   `MultiWorkspace`.

The title bar correctly identifies the foreground worktree and branch. The
Agent Threads panel correctly identifies a terminal. Neither surface explains
that a visible child thread belongs to a different background worktree, and
selecting the thread does not make all three surfaces converge on the same
location.

An agent also has no Flint-owned command for this workflow. It can run
`git worktree add`, but that creates an external checkout rather than a loaded
Flint workspace or a registered Agent Thread. Flint cannot safely infer that a
shell command means the existing agent moved: the agent process remains in its
original working directory after the child shell exits.

## Goals

- Let a parent Agent Thread request an isolated child thread in a new linked
  worktree through a Flint-owned control interface.
- Keep the parent thread and the user's foreground workspace unchanged while
  the child is created.
- Make the owning worktree and branch visible for every live thread.
- Show sibling worktrees from the same project group in one Agent Threads
  panel, including background workspaces created by an agent.
- Make selecting a thread activate its owning workspace and then focus its
  terminal as one user action.
- Keep the worktree picker, title bar, Agent Threads panel, and terminal focus
  consistent.
- Preserve all existing per-agent launch options, credentials, plan usage,
  history, resume behavior, and Direct/Tunneled routing rules.
- Support local and SSH-remote projects without transferring host paths or
  executing the wrong agent binary across the route boundary.
- Degrade explicitly when an agent does not support the control skill or an
  initial prompt.

## Non-Goals

- Moving an already-running agent process from one working directory to
  another. Delegation starts a new child process; the parent remains where it
  started.
- Automatically merging, rebasing, committing, pushing, or deleting a child
  worktree.
- Automatically inventing or checking out a feature branch. Worktree creation
  retains Flint's detached-HEAD safety; the child follows the repository's
  branch instructions before editing.
- Treating arbitrary `git worktree add` invocations as Flint-managed Agent
  Threads.
- Displaying threads from another platform window in the current window's
  panel.
- Replacing the manual title-bar worktree picker.
- General-purpose multi-agent orchestration, task graphs, worker monitoring, or
  result synthesis.

## Product Invariants

### Thread location is explicit

Flint records a thread's owning `Workspace` when the terminal is registered.
The panel never decides location by polling process working directories,
parsing terminal output, matching a title string, or guessing from the most
recently created worktree.

### Selection changes context before focus

A thread click first activates the owning workspace in the same
`MultiWorkspace`, then activates and focuses the terminal item. After the
action completes, the title bar, worktree picker, Agent Threads panel, editor,
and terminal all describe the same worktree.

If workspace activation fails, Flint does not focus a terminal hidden in a
background workspace. It leaves the current context unchanged and surfaces the
error.

### Creation is background-first

An agent-created worktree uses the existing background path in
`create_worktree_workspace` and `MultiWorkspace::add_background_workspace`.
Creating a child must not change the user's active workspace, active item, or
keyboard focus.

### The route boundary remains authoritative

For remote projects, Direct continues to launch only the configured ambient
agent executable on the remote host. Tunneled continues to launch only the
pinned Flint-managed executable on the remote host with traffic routed through
local Flint. A worktree-control request never causes either route to fall back
to the other.

### Raw Git remains external

A checkout created with raw Git is not assigned to a thread automatically.
Flint may show it as an external worktree in the normal worktree picker, but it
does not claim that an existing Agent Thread owns it.

## User Experience

### Parent-to-child flow

From a Codex thread in `main`, the user can ask:

> Work on CSV billing export in a new worktree.

When the installed Flint worktree skill matches that request, the parent calls
the Flint control command with a worktree name, agent kind, and initial task.
Flint then:

1. validates that the caller is a live Agent Thread;
2. resolves the caller's owning workspace and project group;
3. creates the linked worktree through the existing worktree service;
4. retains the new workspace in the same window without activating it;
5. launches a child Agent Thread in the new workspace with the task as its
   initial prompt;
6. records the parent-child relationship and worktree identity; and
7. returns the child and worktree identifiers to the parent.

The original agent does not continue the feature work in `main`. It remains the
parent/coordinator. The new Agent Thread performs the isolated work.

### Panel hierarchy

The panel becomes worktree-first for the current project group:

```text
Agent Threads                                      [New thread ▾]
Codex 5H:64% W:22% · Claude · Pi
Viewing  flint / main · main · Direct

▾ main                                      MAIN  VIEWING
    ◉ Plan worktree-aware threads
      Codex · Needs attention

▾ billing-export                 WORKTREE  feature/billing-export
    ● Implement CSV billing export
      Codex · Active · started 4m ago
      ↳ Created from "Plan worktree-aware threads"

▾ terminal-focus                         WORKTREE  fix/terminal-focus
    ● Fix terminal focus
      Claude · Active · started 11m ago

▸ Recent history
```

The visual hierarchy has three levels only:

1. one project-group context for the panel;
2. one group per loaded worktree workspace; and
3. thread rows inside that worktree.

Agent kind moves from the top-level grouping into each row. This avoids
repeating the same worktree under Codex, Claude, and Pi sections and makes
location the primary navigation axis.

### Panel header

The header contains:

- a **New thread** split button;
- the existing plan-usage summaries when `show_plan_usage` is enabled; and
- a persistent **Viewing** strip containing project name, worktree label,
  branch or detached commit, and remote agent route when applicable.

The New thread menu groups the existing actions by agent kind. Every existing
per-kind launch variant, managed-agent availability rule, credential action,
and hidden setting remains reachable with the same route policy. The main
button repeats the most recently selected visible agent and launch variant for
the currently viewed worktree; until one exists, it opens the menu rather than
guessing.

Plan usage remains visible rather than being hidden inside the menu. On narrow
panels it wraps to its own compact line.

### Worktree group header

Each loaded workspace in the active `ProjectGroupKey` gets one group header:

- `main` or the linked-worktree short name;
- a `MAIN` or `WORKTREE` text label;
- the current branch, or a short commit when detached;
- a `VIEWING` marker only for the foreground workspace; and
- an attention indicator when any child row needs attention.

The full host path appears only in a tooltip. Paths are not the primary label.

For a multi-root project, Flint shows one branch only when every Git-backed root
has the same branch label. Otherwise the header says **Mixed branches** and the
tooltip lists each root and branch. Non-Git roots do not create separate
groups.

Clicking a group header activates that workspace while preserving the currently
focused editor or panel according to existing workspace-switch behavior. It
does not select an arbitrary thread.

### Thread row

A live row shows:

- a status icon and text;
- the thread title;
- the agent label;
- elapsed time since launch; and
- an optional parent relationship for agent-created children.

Status never relies on color alone:

- **Active** means the process and terminal are live and no attention signal is
  pending. It does not claim that the model is currently computing.
- **Needs attention** means the terminal emitted a bell after launch. Focusing
  the thread clears the state.
- Historical rows use their existing timestamp and do not pretend to be live.

Right-click handoff and existing row actions remain available. The selected row
uses the normal selected-element background in addition to its status icon.

### New child feedback

As soon as the child terminal is registered, its worktree group appears in the
currently visible panel. A non-blocking toast says:

```text
Codex started in billing-export                         [Open]
```

**Open** performs the same activate-then-focus operation as clicking the row.
The toast is supplementary; closing or missing it does not hide the child.

### Worktree picker

The existing title-bar picker remains the manual worktree-management surface.
Its rows gain a trailing occupancy label such as **1 agent** or **2 agents**,
plus an attention marker when applicable. A worktree containing a live Agent
Thread cannot be deleted from the picker; the disabled action explains which
thread still owns it.

No occupancy is inferred from processes outside `AgentThreadStore`.

### History and missing worktrees

Recent history is collapsed below live worktree groups. Expanding it groups
historical rows by their recorded project root. If that root belongs to a
currently loaded workspace, the normal worktree label is used. Otherwise the
group is labeled with the last path component and **Not open**.

Selecting history for an open worktree activates that workspace before resume.
Selecting history for a missing worktree follows the existing resume behavior;
this change does not recreate a removed linked worktree automatically.

## Identity Model

### Flint thread identity

Add a Flint-owned `AgentThreadId` generated when a terminal thread is first
registered. `terminal_item_id` remains the live GPUI item handle, and the
provider session id remains the resume/history identity. They are not used as
substitutes for one another:

| Identity                 | Lifetime                          | Purpose                                              |
| ------------------------ | --------------------------------- | ---------------------------------------------------- |
| `AgentThreadId`          | Flint thread/restoration lifetime | Parent links, control authorization, panel selection |
| terminal item `EntityId` | One live UI item                  | Pane lookup and focus                                |
| provider session id      | Provider history lifetime         | Resume and transcript lookup                         |

Session-restore records persist `AgentThreadId`. Older records without it are
upgraded by assigning an id on load.

### Worktree context

Each live `ThreadEntry` already owns a weak `Workspace`. Extend the metadata
exposed to the panel with an `AgentThreadLocation` derived from that owner:

```text
AgentThreadLocation
  window_id
  workspace_entity_id
  workspace_database_id (when persisted)
  project_group_key
  project_roots
  primary_project_root
```

Worktree display name, branch, detached commit, host, and main/linked state are
read from the live workspace and repositories at render time so branch changes
appear without rewriting thread metadata. A small last-known display snapshot
is retained only for history or a workspace that closes between query and
render.

`project_root` remains in `AgentThreadMetadata` for provider-history filtering
and compatibility. It stops being the authority for live workspace ownership.

### Origin

Agent-created children record:

```text
AgentThreadOrigin::Child {
    parent_thread_id,
    requested_worktree_name,
}
```

Manual launches use `AgentThreadOrigin::User`. Restored legacy threads use
`AgentThreadOrigin::Unknown`. The panel omits the relationship line when the
parent title is no longer available rather than rendering a stale or guessed
title.

## Panel Data and Ownership

`AgentThreadsPanel` remains a workspace panel; it does not move into
`MultiWorkspace`. During render it resolves:

1. its owning window's active workspace;
2. that workspace's `ProjectGroupKey`;
3. `MultiWorkspace::workspaces_for_project_group`; and
4. live threads whose owning workspace entity is in that set.

Filtering by owning workspace prevents a local and remote checkout with equal
textual paths from colliding. Filtering by window prevents selecting a terminal
owned by another platform window.

Every workspace in a project group must render the same group-level panel state
after a switch. Selected thread, group expansion, recent-history expansion, and
the remembered New thread choice are therefore stored in shared presentation
state keyed by `(WindowId, ProjectGroupKey)`, not only in one
`AgentThreadsPanel` instance.

Provider history refresh remains provider-specific internally. The panel
collects roots from every retained workspace in the group, performs the
existing per-kind scans, then presents the merged rows under worktrees. Existing
visible caps apply to the whole provider result before grouping so changing the
layout does not silently increase history work.

`AgentThreadStoreEvent` gains a general thread-updated event for location,
title, attention, and parent changes. Active panels rerender from store state;
events do not carry duplicate display models.

## Activate-Then-Focus Semantics

`AgentThreadStore::focus_thread` uses the `WindowHandle<MultiWorkspace>` already
stored in `ThreadEntry`.

1. Resolve the thread, owner workspace, terminal view, and owning window.
2. Reject the request if the caller is in a different platform window from the
   thread exposed in its panel.
3. Call `MultiWorkspace::activate` for the owner workspace when it is not
   foreground.
4. After activation publishes `ActiveWorkspaceChanged`, resolve the pane and
   terminal item again.
5. Activate and focus the terminal item.
6. Clear the thread's attention state and publish a thread-updated event.

The second lookup matters because workspace activation can replace panel and
pane state. Flint does not hold borrowed pane indexes across the activation.

If the workspace or terminal disappears at any step, the action returns a
user-visible error and prompts a store refresh. It never panics or focuses a
different item by stale index.

## Flint Agent Control

### Command surface

Ship a small `flint-agent-control` executable with the local app and remote
server. A dedicated executable avoids making `flint agent ...` ambiguous with
the existing `flint <path>` CLI contract.

The initial command is intentionally narrow:

```sh
"$FLINT_AGENT_CONTROL" create-thread \
  --worktree new \
  --name billing-export \
  --agent codex \
  --prompt "Implement CSV billing export" \
  --json
```

Supported worktree values are:

- `current`: start the child in the caller's owning workspace; and
- `new`: create a linked worktree from the caller's current commit and start
  the child there.

The first release requires an explicit `--name` for `new`. It validates the
same naming and collision rules as the worktree picker. `--agent` defaults to
the caller's kind only when that kind supports seeded launch. `--prompt` is
required for agent-originated creation; Flint refuses to launch an unattended
child with no task.

Successful JSON contains stable identifiers and display information, not an
internal socket or capability token:

```json
{
  "thread_id": "0198e2cc-58cf-7f20-a896-0becc6fbc042",
  "worktree": "billing-export",
  "branch": null,
  "path": "/projects/worktrees/billing-export",
  "agent": "codex"
}
```

Human-readable output is used without `--json`.

### Session context and authorization

Every Agent Thread launch receives:

- `FLINT_AGENT_CONTROL`: an absolute path to the host-local control executable;
- `FLINT_AGENT_THREAD_ID`: the caller's Flint thread id;
- `FLINT_AGENT_KIND`: the registered agent kind; and
- a short-lived, thread-scoped control token delivered through a separate
  environment variable.

The control token authorizes only child creation inside the caller's current
project group and window. It cannot open arbitrary host paths, select another
remote host, delete worktrees, change route settings, or control unrelated
terminals. The broker validates the live thread and its owner again for every
request.

The token is rotated when a session is restored and invalidated when the
terminal closes. Requests are concurrency-limited per parent so repeated or
prompt-injected calls cannot start an unbounded creation storm.

### Local and remote transport

Locally, `flint-agent-control` sends a structured request to the running Flint
application over its existing authenticated local IPC boundary.

On an SSH project, the executable lives beside `flint-remote-server` and sends
the request to that server. The remote server validates host-local paths and
forwards only the typed create-thread request to the owning client. The client
invokes the existing remote-aware worktree service. A local filesystem path is
never sent to the remote agent, and a remote path is never interpreted by the
local filesystem.

The agent process route is selected only after the new remote workspace is
open. Direct/Tunneled policy is read again from that workspace immediately
before launch so a concurrent route change fails instead of launching through
stale policy.

### Seeded launch

Child creation reuses each agent kind's explicit `InitialPromptStrategy`. The
command builder first applies the normal launch option, session id, route,
managed executable, proxy, and self-update policy. It appends the child prompt
last using the registered strategy.

The existing handoff-only seeded launch path is not sufficient because it
currently excludes managed/Tunneled launch. The child path must share the
ordinary route-aware launcher and represent unsupported seeded prompts as an
explicit agent capability. There is no fallback that starts an idle child and
claims the task was delivered.

## How the Agent Learns the Command

Flint bundles a provider-neutral `flint-worktrees` Agent Skill. Its catalog
description includes concrete trigger phrases such as:

- work in a new or another worktree;
- delegate this to another agent;
- start a child agent in an isolated checkout; and
- continue this feature without changing the current worktree.

The skill tells the agent to use `FLINT_AGENT_CONTROL`, explains that the
parent remains in its current worktree, and documents the small JSON command
contract. It explicitly warns against using raw `git worktree add` when the
requested result is a Flint-visible child thread.

Terminal-backed agents own their native skill discovery. Flint therefore does
not silently write into global agent configuration. The Settings Editor offers
**Install Flint worktree skill** for each agent with a tested installation
capability. Installation shows the target path and requires confirmation.
Managed and ambient executables use the same installed skill on the host where
they run.

The capability is explicit in `AgentKindDefinition`; registry membership alone
does not render the install action or promise autonomous discovery. The first
implementation may enable it for Codex only. Claude, Pi, and future agents stay
disabled until their native discovery path, remote behavior, upgrade, and
uninstall flow are tested end-to-end.

Without the skill, the control command remains available for a user to paste or
for project instructions to reference. Flint does not claim that the agent will
discover it autonomously.

## Creation Transaction and Failure Handling

Creation is a staged operation, not an all-or-nothing filesystem transaction:

1. Validate caller, agent capability, name, route, repository, and connection.
2. Reserve one creation slot for the parent workspace.
3. Create and open the linked worktree through
   `create_worktree_workspace`.
4. Run the existing worktree trust propagation and `create_worktree` task-hook
   behavior.
5. Launch and register the child terminal in the new workspace.
6. Release the reservation and return the result.

Failures before step 3 leave no new worktree. If worktree creation succeeds but
agent launch fails, Flint keeps the new workspace and reports a partial result:

```text
Created billing-export, but Codex did not start: <reason>       [Open worktree]
```

The control command exits non-zero and includes the retained worktree path in
its structured error. Flint does not delete a checkout that may contain setup
hook output or user files.

Existing worktree-service errors remain user-visible: no repository,
disconnected remote, name collision, fetch failure, trust failure, and
concurrent creation. Agent provisioning and launch errors use their existing
notifications. No error is discarded after showing only in the parent
terminal.

## External Worktrees

If an agent ignores the skill and runs `git worktree add`:

- the parent thread stays grouped under its original workspace;
- the external checkout can appear in the normal worktree picker after Git
  refresh;
- Flint labels it as not open until the user opens it; and
- no child Agent Thread is registered automatically.

A future **Attach external worktree** flow may open the checkout and launch a
new thread, but automatic import is excluded here. Process-tree scanning and
terminal-title parsing are not reliable enough to prove ownership.

## Persistence and Lifecycle

- Agent-created workspaces use the existing retained-background-workspace
  persistence.
- `AgentThreadId`, origin, owner workspace database id, and last-known worktree
  snapshot are added to session-restore records with a versioned migration.
- Closing a child terminal removes live occupancy but does not close or delete
  its workspace.
- Closing a background workspace with a live thread requires confirmation and
  names the affected thread.
- Deleting a linked worktree with any live occupant is blocked.
- A restored thread whose workspace no longer exists appears in Recent history
  as **Not open**; this design does not reconstruct the worktree.

## Considered Alternatives

### Keep agent-kind sections and add a worktree subtitle

This is the smallest UI change, but worktree identity remains secondary and
the same checkout is repeated under every provider. It also makes switching
between all agents in one worktree harder. Rejected in favor of worktree-first
navigation.

### Infer the worktree from current working directory

An agent can run commands with another directory without changing its own
process working directory. Shells, wrappers, remote launchers, and resumed
sessions also make polling ambiguous. Rejected because incorrect inference is
worse than missing metadata.

### Let the parent run `git worktree add` and start a nested CLI

The nested process would not be registered in `AgentThreadStore`, would bypass
route-aware launch and session restoration, and could outlive the terminal
without Flint ownership. Rejected.

### Activate the new worktree immediately

This makes location obvious, but steals focus whenever an agent delegates in
the background. It prevents the user from continuing to talk to the parent.
Rejected; the new group and Open action provide visibility without interruption.

### Move `AgentThreadsPanel` into `MultiWorkspace`

A window-owned panel would make cross-workspace aggregation direct, but it
would require moving dock state, settings, panel activation, and every existing
workspace integration. Keeping the panel workspace-owned and sharing its
project-group presentation state achieves the required behavior with a smaller
ownership change.

### Add subcommands to the existing `flint` CLI

`flint` currently treats positional arguments as paths. Reserving a word such
as `agent` or `worktree` would change the meaning of an existing command like
`flint agent`. A dedicated control executable keeps the user-facing open-path
contract intact.

### Install the skill silently

Writing agent instructions into global provider configuration without consent
crosses a trust boundary and is difficult to undo. Rejected in favor of an
explicit, capability-gated install action.

## Testing

### Identity and grouping

- Live threads are grouped by their owner workspace, not textual path alone.
- Only workspaces in the active project group and platform window are shown.
- Main, linked, detached, mixed-branch, multi-root, local, and remote display
  labels are correct.
- Parent relationships use `AgentThreadId` and disappear safely when the
  parent is unavailable.
- Legacy restoration records receive a new Flint thread id.

### Navigation

- Clicking a thread in the foreground workspace focuses its terminal without a
  workspace switch.
- Clicking a background thread activates its workspace before focusing the
  terminal.
- Activation failure leaves the original workspace and focus unchanged.
- A terminal or pane disappearing between activation and focus returns an
  error without indexing or update panics.
- The title bar and `VIEWING` marker update in the same interaction.
- Bell events set **Needs attention** independently of desktop-notification
  settings; focusing the row clears it.

### Background creation

- A parent in `main` creates a retained child workspace without changing the
  active workspace or focus.
- The child command's working directory is the new worktree root.
- The initial prompt is applied after every normal launch argument.
- A child group appears in the already-visible parent panel after registration.
- Launch failure retains the created worktree and returns a structured partial
  error.
- Concurrent requests from one parent are bounded and do not create duplicate
  names.

### Route and agent capability

- Local creation uses the local control broker.
- Remote creation validates paths and creates the worktree on the remote host.
- Direct uses only the configured ambient remote executable.
- Tunneled uses only the pinned managed remote executable and Flint egress.
- A route change during creation aborts launch without fallback.
- Unsupported initial-prompt and skill-install capabilities are explicit and
  hide or reject the corresponding action.
- Every agent-enabled install control uses the exact tested provider path and
  works for install, upgrade, uninstall, and remote setup.

### Control security

- Missing, expired, closed-thread, cross-window, cross-project, and cross-host
  tokens are rejected.
- A caller cannot choose an arbitrary worktree path or delete a worktree.
- JSON output never includes the control token or internal socket address.
- The control process propagates broker and launch errors to stderr and a
  non-zero exit code.

### Existing behavior

- Manual worktree creation still foregrounds the new workspace.
- Manual worktree switching and opening in a new window are unchanged.
- Existing per-agent launch menus, hidden settings, plan usage, credentials,
  handoff, history, and resume remain available after the layout change.
- Worktree deletion remains available when no live Agent Thread occupies it.

Focused crate tests, GPUI interaction tests, `cargo fmt --all -- --check`, and
`./script/clippy` are required by the implementation plan. Remote-route tests
must cover Direct and Tunneled without contacting an agent provider.

## Rollout

Implement behind an `agent-thread-worktree-control` feature flag in three
reviewable stages:

1. explicit thread location, worktree-first panel grouping, occupancy, and
   activate-then-focus;
2. authenticated local/remote control command and background child launch; and
3. capability-gated skill installation, starting with agents whose native skill
   discovery is verified.

The first stage is useful without autonomous creation and can ship on its own.
The control command does not become discoverable to agents until its
authorization, error propagation, and route tests pass.

## Acceptance Criteria

- While viewing `main`, the panel shows a background child under its named
  worktree with agent and status labels.
- The user can always tell which worktree is foreground from the persistent
  Viewing strip and group marker.
- Selecting the child switches Flint to its workspace and focuses its terminal;
  selecting the parent returns to `main` and focuses the parent terminal.
- Agent-created worktrees do not steal focus when created.
- The worktree picker shows live agent occupancy and blocks unsafe deletion.
- A supported agent asked to work in a new worktree uses the Flint control
  command through its installed skill and receives the created child id.
- Raw Git worktrees remain visibly external and are never assigned by guess.
- Local, Direct remote, and Tunneled remote creation preserve their established
  executable and transport boundaries.
- Unsupported agents and partial failures are stated explicitly rather than
  appearing to succeed.
