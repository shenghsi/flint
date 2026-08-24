# Caller Disambiguation and Agent-Initiated Terminal Creation

## Status

This document is a design proposal. No implementation work has started.

## Goal

Give the agent control server (`crates/agent_threads/src/control.rs`) the best
available way to answer one question: which live Agent Thread, if any, does
this connecting process belong to? The answer must be exact when the
available signals identify one thread. The server must return
`caller-not-recognized` when the signals are missing or ambiguous. This rule
must also apply to a CLI that routes tool commands through a shared,
long-lived daemon instead of forking its own child process.

After Flint identifies the caller, let the caller create a plain terminal,
create a split terminal, or create a sibling Agent Thread. Terminal creation
must use the caller's workspace and current placement as its scope. It must
not depend on whichever pane the user has focused.

## Background

`crates/agent_threads/src/control.rs` and
`docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md`
already describe a two-step caller resolution:

1. **Process ancestry.** The server reads the true peer process ID of the
   connecting socket from the operating system (`LOCAL_PEERPID` on macOS,
   `SO_PEERCRED` on Linux, the named-pipe client PID on Windows), then walks
   its parent chain looking for a PID the server already tracks as a live
   terminal. This step needs no cooperation from the agent CLI and is the
   strong signal.
2. **Working-directory fallback.** Some CLIs, Codex among them, run tool
   commands through an already-running daemon (`codex app-server`) instead
   of forking a child of the interactive session. No ancestor PID is ever
   one Flint tracks, so step 1 finds nothing. The server then matches the
   connecting process's own current working directory against each tracked
   thread's tied worktree root. When more than one thread ties to that root,
   the server tries to break the tie using the connecting process's own
   ancestry process names against each candidate's `kind_id` (for example,
   `"codex"`).

This design is already agent-neutral by construction. Unit tests in
`control.rs` already model the Codex daemon topology
(`resolve_caller_thread_falls_back_to_cwd_when_ancestry_has_no_match`) and
pass.

## The problem

Step 2's tie-break by process name only works when the ambiguous candidates
are of *different* kinds. When two Agent Threads of the *same* kind are tied
to the same worktree at the same time, the connecting process's name (for
example, `"codex"`) matches every candidate equally, and the server correctly
refuses to guess. This is exercised today by
`resolve_caller_thread_refuses_an_ambiguous_cwd_match`.

This is not a rare case. It is the normal case for a user who runs more than
one Codex Agent Thread in the same worktree at once. Live evidence from this
machine's own Flint log (`~/.local/share/flint/logs/Flint.log`) confirms it:

```text
agent control caller 891556 could not be resolved:
ancestry [(891556, "flintctl"), (2946707, "codex"), (2946588, "node"), (1, "systemd")]
matched none of the tracked pids [...]
tracked worktrees:
  (codex, /mnt/storage/xis/dev/flint)
  (codex, /mnt/storage/xis/dev/flint)
  (claude, /mnt/storage/xis/dev/flint)
  ...
```

Two `codex`-kind threads are tied to the same worktree. Working-directory
matching finds both. Process-name matching cannot separate them: both
candidates' `kind_id` is `"codex"`, and the caller's own ancestry contains
`"codex"` regardless of which of the two threads it belongs to. The server
reports `caller-not-recognized`, correctly, given the information it has
today.

### A separate, smaller problem

`crates/agent_control_skill/skills/flintctl/SKILL.md` gates all use of
`flintctl` on `FLINT_AGENT_THREAD=1` being set in the calling process's own
environment. Flint sets this variable once, at launch time, for every local
Agent Thread process
(`apply_control_skill_environment` in `crates/agent_threads/src/store.rs`).
For a CLI that forks its own tool subprocess (Claude Code, for example), the
variable is inherited and current. For Codex, the variable's value comes
from whenever its shared `app-server` daemon last started, which can predate
the current Agent Thread entirely, or be missing if the daemon started
outside any Flint Agent Thread.

This gate sits in front of the server-side resolution above and can hide a
real Agent Thread from the skill even when the server-side resolution would
have found it. It is a real defect, but fixing it alone does not fix the
same-kind ambiguity problem, which is a caller-identity gap in the server,
not a skill-side gate problem.

## Proposed method

**Rule:** never treat a value a local process can freely claim about itself
(an environment variable, a declared ID) as sufficient identity on its own.
Use it only to break a tie after the operating system has supplied the peer
PID and Flint has narrowed the candidates by current working directory and
agent kind. The session ID is a disambiguation signal. It is not an
authorization token.

The daemon fallback is weaker than terminal-PID ancestry. A same-user process
can choose a working directory, process name, and environment. The local
socket remains user-scoped and the server requires a matching live local
Agent Thread, but the fallback cannot prove that the caller descended from a
Flint PTY. This is an accepted limit for daemon-routed agents. The server
must not describe this fallback as equivalent to the strong ancestry match.

### New step: session-ID tie-break

Add one step, used only when the working-directory step still has more than
one candidate of the *same* kind:

3. **Session-ID tie-break.** Some agent CLIs carry a stable, per-run session
   ID in their own process environment, even when routed through a shared
   daemon. Codex sets `CODEX_THREAD_ID` in the environment of the tool-call
   process, so a `flintctl` run from that process inherits it. Flint already
   learns and stores a Codex session ID per Agent Thread today, for history
   resume (`AgentThreadMetadata::resumed_session_id`, populated by
   `attach_discovered_session_id` in `store.rs`). The server keeps only the
   candidate whose stored session ID equals the caller's session ID. Exactly
   one match resolves the caller. Zero or more than one match stays
   unresolved, the same fail-closed behavior the server already uses
   everywhere else.

#### Required assumption: the two IDs are the same identifier

The tie-break works only if `CODEX_THREAD_ID` and Flint's stored
`resumed_session_id` name the same thing. Flint does not read
`CODEX_THREAD_ID` today. It takes the stored ID from the Codex rollout file
field `session_meta.payload.id` (`parse_summary` and `summary.id` in
`crates/agent_history/src/codex.rs`). Sample rollout files show that
`payload.id` and `payload.session_id` hold the same UUID, which supports the
assumption but does not prove it.

State this as a named assumption, and verify it before implementation step 2:
capture `CODEX_THREAD_ID` from a live Codex tool-call process and compare it
with the `session_meta.payload.id` of that session's rollout file. If the two
values name different things, the tie-break never matches. It then fails
closed and silently, so nothing reports the defect. Add an assertion or a log
line at the comparison point so a future divergence is visible.

#### Decided: the server reads the peer's environment

Two sources were possible: the server reads the connecting process's own
environment through `sysinfo`'s `Process::environ()` and
`ProcessRefreshKind::with_environ`, or `flintctl` sends its own value as an
optional request field. This design uses the first.

The deciding reason is which side of the connection knows what to look for.
By the point this tie-break runs, the server has already narrowed the
candidates to one kind (Codex, say) and knows that kind's
`caller_session_env_var`. A generic `flintctl` binary has no equivalent
knowledge at request-construction time: it is not told in advance which
Agent Kind, if any, it is about to be matched against, so it cannot know
which single variable name to read and send. It would have to send every
known kind's session-variable value speculatively, which grows the request
surface every time a new kind is registered and leaks environment values the
server may not even need for this particular request. Reading the value
server-side, only for the one kind resolution has already narrowed to, avoids
both problems and keeps the existing sentence in
`docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md`
literally true: "No token supplied by the client is treated as caller
identity."

The cost is real and the design accepts it: process-environment access can
fail on every supported platform because of permissions, process exit, or
operating-system restrictions, and the remote server must repeat the read on
the remote host rather than reuse a value the local bridge already had. An
unavailable, missing, non-Unicode, or empty value is no match. The server
must not fall back to guessing after a failure.

### Session association readiness

The tie-break can work only after Flint has attached a session ID to the live
terminal. A fresh Codex thread starts with
`AgentThreadMetadata::resumed_session_id == None`; the existing background
history discovery runs every 30 seconds and can also remain ambiguous when
several new sessions appear in the same project.

The current discovery candidate filter excludes remote projects. Extend it to
use the existing remote history index through the authenticated project
connection. Apply the same launch-time, project, kind, and already-bound rules
as local discovery. Do not scan a remote user's files from the local
filesystem. A connection failure leaves the session unassociated and retries
on the next bounded discovery interval.

This design does not claim that `CODEX_THREAD_ID` solves that earlier
association problem. The server follows these rules:

1. If exactly one cwd-and-kind candidate exists, resolve it as today. The
   session ID is not needed.
2. If several same-kind candidates exist, every one of them must already have
   an attached session ID. If one or more of them is still unassociated,
   return `caller-not-recognized`. Do not compare only the associated
   candidates. `attach_discovered_session_id` infers the ID from history
   files instead of receiving it from Codex, so an unassociated candidate can
   still be the true caller.
3. If exactly one attached ID equals the caller's session ID, resolve that
   candidate.
4. If no attached ID matches, or more than one attached ID matches, return
   `caller-not-recognized`.

The skill can retry a not-ready response through the existing CLI retry
deadline, but it must not wait without a bound. If concurrent fresh Codex
threads cannot be associated by the existing history discovery, control from
those daemon-routed threads remains unavailable. A later design can add a
supported Codex-to-Flint session registration channel if Codex exposes one.
The current official Codex documentation does not establish a launch option
that lets Flint assign a session ID to a fresh session, so this design does
not invent one.

### Remote caller disambiguation

Remote development uses the same resolution order on the remote host. The
remote `flintctl` bridge gets the peer PID, and `flint-remote-server` reads
the remote process ancestry, cwd, process names, and -- per "Decided: the
server reads the peer's environment" -- the remote peer process's own
configured session environment variable. Local Flint must send the live
Agent Thread kind, the kind's `caller_session_env_var`, the attached session
ID, and the tied worktree root as connection-bound metadata for the matching
remote PTY registration. The remote server needs the environment-variable
name because `AgentKindDefinition` is local application state, not remote
server state. It needs the tied worktree root because the terminal working
directory captured when the remote PTY registers does not follow a later
retie and is not enough to match a caller working in another linked worktree
of the same repository.

All paths in this metadata are remote-host paths. The remote server compares
the peer cwd with the tied worktree roots and, when needed, computes git common
directories from those remote paths. It must not scan local paths or infer
identity metadata from a client claim.

Local Flint sends the complete identity metadata when it binds the remote PTY
registration. It sends an update when a retie changes the tied worktree root
or when session discovery attaches or changes the session ID. Each update is
bound to the authenticated project connection and the current registration
generation. The remote server rejects a stale generation and discards the
metadata when the PTY, connection, or registration generation ends. Until the
required metadata arrives, ambiguous same-kind remote callers remain
unresolved. Direct and Tunneled routes use the same identity exchange; the
exchange does not change how either agent executable starts or reaches its
provider.

### Generalizing beyond Codex

To keep this usable for a future daemon-routed agent kind without special
casing, add one optional field to `AgentKindDefinition`
(`crates/agent_threads/src/agent_threads.rs`):

```rust
/// Environment variable this kind's own CLI sets on every tool-call
/// process, carrying a session ID stable for that CLI run. Used only to
/// break a tie between live threads of this kind tied to the same worktree,
/// after ancestry, cwd, and process name have already narrowed the
/// candidates to that kind. This value is not an authorization token.
/// `None` for kinds that do not provide a stable session ID in each
/// tool-call process.
pub caller_session_env_var: Option<&'static str>,
```

Only Codex sets this today (`Some("CODEX_THREAD_ID")`). A kind whose CLI
forks its own tool subprocess directly never needs it, because step 1
already gives a strong, unambiguous answer for it.

### Full resolution order

```text
peer PID -> tracked terminal PID (ancestry walk)
  |  no match
  v
peer's own cwd -> tracked worktree root
  |  0 candidates -> unresolved
  |  1 candidate  -> resolved
  |  2+ candidates
  v
peer's own ancestry process names -> candidate kind_id
  |  narrows to 1 -> resolved
  |  2+ candidates of the SAME kind remain
  v
any remaining candidate has no attached session ID?   [NEW]
  |  yes -> unresolved
  |  no
  v
caller's session ID -> candidate's stored session ID   [NEW]
  |  exactly 1 match -> resolved
  |  0 or 2+ matches
  v
caller-not-recognized (unchanged, fail closed)
```

The caller's session ID is the value of the kind's `caller_session_env_var`
read from the caller's own environment by the server -- see "Decided: the
server reads the peer's environment".

### `SKILL.md` probe

Remove `FLINT_AGENT_THREAD` completely. Put no environment variable in its
place. Flint also sets `TERM_PROGRAM=flint`, `TERM_PROGRAM_VERSION`, and
`ZED_TERM=true` in every terminal it creates
(`insert_flint_terminal_env` in `crates/terminal/src/terminal.rs`), but those
variables carry the same defect and must not become the new gate. A
daemon-routed tool process inherits the daemon's environment, so the variables
fail in both directions: they are absent when the daemon started outside
Flint although the caller really is in an Agent Thread, and they persist after
the terminal closed or Flint quit although the caller is not.

The skill uses a two-stage gate.

**Stage 1: a cheap negative check.** Test that the release-matched marker
exists and that the control endpoint exists — the socket on Unix, the named
pipe on Windows. Neither test reads the caller's environment, so no stale
daemon can affect the answer. The marker proves Flint is installed. The
endpoint proves Flint is running, because the control server creates it at
start and removes it on quit. If either is absent, stop and continue the task
without Flint.

Stage 1 earns its place on cost. When the server cannot resolve the caller,
the CLI sleeps through `RETRY_BACKOFFS` of 250 ms, 500 ms, and 1000 ms before
it reports `caller-not-recognized`
(`crates/agent_control_cli/src/lib.rs`). An agent that never runs in Flint
would pay about two seconds on every skill activation. The endpoint test
removes almost all of those cases for the cost of one filesystem check.

**Stage 2: the probe.** Run `flintctl terminal current --json`. It is the only
authoritative answer, and one call answers two separate questions:

- The call succeeds -> the caller is in a live Flint terminal. It can use
  `terminal open`, `terminal split`, and the other terminal commands.
- The response also has `is_agent_thread: true` -> the caller is an Agent
  Thread. It can additionally use `thread retie` and `thread create`.

A connection failure, a protocol mismatch, or `caller-not-recognized` means
the skill must continue without any Flint control. `is_agent_thread: false`
is not such a case: it withdraws only the thread commands, and the terminal
commands stay available. This matches "Access and remote boundaries".

Update the skill's own frontmatter description as well. It currently reads
"Outside a Flint Agent Thread, continue without Flint control commands",
which understates what an ordinary Flint terminal caller may do.

Remove `apply_control_skill_environment`, its launch-path call, and its unit
test from `store.rs`. Update the active OpenSpec requirement and scenarios so
they require the two-stage gate instead of the environment variable.

## Terminology: `terminal split` matches tmux and Herdr; `terminal open` does not

An agent that knows tmux or Herdr already has a working mental model for a
side-by-side split: a new rectangular region appears next to the current one,
holding one new shell, and a divider between them can be dragged to resize
both. `terminal split` is exactly that model, not a different one. It creates
a new `Pane` holding exactly one new terminal with its own PTY
(`new_pane_with_active_terminal`, `SplitMode::EmptyPane` /
`SplitMode::ClonePane` in `TerminalPanel`), placed beside the source. The
divider is the same `HANDLE_HITBOX_SIZE` resize handle the user drags by hand;
dragging it calls `compute_resize`, which adjusts `flexes[ix]` in the owning
`PaneAxis` (`crates/workspace/src/pane_group.rs`) -- the same mechanism
whether a person drags the handle or `flintctl` created the pane. An agent
that already knows tmux or Herdr needs no new vocabulary for `terminal split`.

`terminal open` is the one place that mental model does not carry over. It
adds the new terminal as another **tab** inside an existing pane, not as a new
resizable region, and without `--focus` that tab is meant to stay behind the
one already showing -- see "Reuse Flint's own terminal creation path" for the
`activate`-versus-`focus_item` fix this default depends on. Tmux has no
equivalent: a tmux pane cannot hold a second shell hidden behind the active
one the way a Flint pane's tab bar can. State this plainly to an agent using
the skill, because assuming `terminal open` behaves like a small split would
put the new terminal in the wrong place.

Two consequences follow:

1. **Most panes hold exactly one terminal, but not all of them.**
   `terminal split` keeps that 1:1 relation at creation time, matching tmux
   and Herdr. `terminal open` breaks it by adding a tab to a pane that may
   already hold one. A pane that has only ever been split, never opened into,
   is safe to treat as one terminal. Direction-based addressing still resolves
   to the pane's **active** tab, because `terminal open` makes that the
   general case. See "Spatial addressing".
2. **Pane identity stays private regardless.** `TerminalControlId` remains
   the only public address. Do not add a pane ID to the protocol. An agent
   that thinks in tmux terms would otherwise treat a pane ID as a terminal,
   address the wrong thing, and get no error.

## Agent-initiated creation commands

The current `terminal` command group can inspect and operate only terminals
that already exist. Add these commands:

```text
flintctl terminal open [--cwd <path>] [--focus] [--json]
flintctl terminal split (--current|--terminal <terminal-id>) \
  --direction <left|right|up|down> [--cwd <path>] [--focus] [--json]

flintctl thread create --worktree <current|new> [--name <name>] \
  --agent <agent> --prompt <prompt> \
  [--split <left|right|up|down>] [--focus] [--json]
```

`thread create` already exists. This change keeps it in the `thread` group,
preserves its existing `--name` and `--json` options, adds optional placement
for a current-worktree thread, returns the created terminal identity, and
teaches the installed skill when to use it. Do not add a second terminal
command that starts an agent.

### Reuse Flint's own terminal creation path

Every command in this group must create its terminal through the same
functions the normal UI actions call. Do not build a parallel creation path.
A terminal that Flint creates through its own path is a Flint-managed
terminal: `TerminalPanel` owns it, it gets a tab, `Pane::add_item` records it
for workspace serialization and restore, and `TerminalControlRegistry`
registers it. Registration is automatic, because `terminal_control::init`
observes every new `TerminalView` (`cx.observe_new` in
`crates/agent_threads/src/terminal_control.rs`). A hand-built terminal that
skips `Pane` and `TerminalPanel` is not a managed terminal, even though the
observer still registers it, so `flintctl` must never create one.

There is one narrow exception, and it is about dispatch, not about the code
path. Do not call `window.dispatch_action(SplitRight …)` or the equivalent
for the other directions. Action dispatch routes to whichever pane holds
focus, so the target would be the user's focused pane instead of the
caller's. Call the same method the action handler calls, on the caller's own
pane entity: `Pane::split` takes `&mut self`, so
`caller_pane.update(cx, |pane, cx| pane.split(direction, SplitMode::EmptyPane,
window, cx))` targets an exact pane. The resulting `pane::Event::Split`
carries that same pane to `TerminalPanel`, which already calls
`center.split(&source_pane, &new_pane, direction, cx)` with the source pane
from the event, so placement itself is already deterministic and does not
read `active_pane`.

That event handler is not, on its own, something control code can use.
`Pane::split` returns nothing, and `TerminalPanel`'s
`pane::Event::Split` handler runs the actual pane and terminal creation
inside `cx.spawn_in(window, async move |panel, cx| { … }).detach()`
(`terminal_panel.rs`): it returns before creation finishes, and if
`new_pane.await` comes back `None` the handler silently returns. A control
request that just dispatched `Pane::split` would have no way to learn
whether creation succeeded, when it finished, or what the new terminal's ID
is -- exactly what "Creation completion and errors" below requires it to
report.

Four gaps remain in the existing path. Close them by adding parameters and
one shared function, not by writing a second creation path:

1. Extract the body of that `SplitMode::ClonePane | SplitMode::EmptyPane` arm
   into a method on `TerminalPanel`, for example
   `create_adjacent_terminal(source_pane: Entity<Pane>, clone: bool,
   direction: SplitDirection, window: &mut Window, cx: &mut Context<Self>)
   -> Task<Result<WeakEntity<Terminal>>>`. The `pane::Event::Split` handler
   calls it and detaches the task, same as today, except it should log a
   failure instead of discarding it silently
   (`task.detach_and_log_err(cx)`), consistent with this repository's error
   handling rule. Control code calls the same method and **awaits** the
   task, mapping `Ok` to the created terminal's metadata and `Err` to
   `terminal-create-failed` or `terminal-placement-failed`. This is the one
   piece of new plumbing this design needs; everything else below is a
   parameter change to code that already exists.
2. Inside that method, `window.focus(&new_pane.focus_handle(cx), cx)` must
   become conditional, so a control-created split can keep focus on the
   caller.
3. `add_terminal_shell_internal` always calls `Pane::add_item`, and
   `add_item` hardcodes its internal `activate: bool` to `true`
   (`add_item_inner`'s third parameter), so `activate_item` always makes the
   new terminal the pane's visible tab regardless of the existing
   `focus_item` argument -- `focus_item` only calls
   `focus_active_item` for keyboard focus afterward. `RevealStrategy` alone
   cannot fix this: it gates the panel-level reveal and the keyboard-focus
   step, not which tab a pane shows. Call the already-`pub`
   `Pane::add_item_inner` directly instead of the `add_item` wrapper, with
   an explicit `activate` argument distinct from `focus_item`. Default
   `terminal open` (no `--focus`) must pass `activate: false`, so the
   caller's current tab keeps showing; `--focus` passes `activate: true`
   together with `focus_item: true`. Also add the explicit-pane variant of
   `add_terminal_shell_internal` that reads a passed-in pane instead of
   `terminal_panel.active_pane`.
4. `new_pane_with_active_terminal` reads `self.active_pane`, and with
   `clone = false` it takes the working directory from
   `default_working_directory` instead of the source terminal. Let the
   caller pass the source terminal or an explicit working directory.

### `terminal open`

Create a new plain shell terminal as another item in the caller's current
terminal pane. The new terminal has a new `Terminal`, PTY, shell process,
`TerminalView`, and `TerminalControlId`. It does not copy the caller's running
shell state or start an Agent Thread.

The default working directory is the caller terminal's last known working
directory. If that value is unavailable, use the workspace terminal default.
An explicit `--cwd` must be an absolute existing directory on the machine
that will own the PTY. The caller can already change to any accessible local
directory, so the path does not have to be inside a project root.

Without `--focus`, the new terminal is created but stays behind the caller's
current tab: neither the pane's visible tab nor keyboard focus changes. This
requires the `activate`-versus-`focus_item` fix in "Reuse Flint's own
terminal creation path" -- calling today's `add_item` wrapper as-is would
make the new terminal the visible tab regardless of `--focus`, because that
wrapper hardcodes tab activation independently of focus. `--focus` makes the
new terminal both the visible tab and the focused one. The response returns
the same terminal metadata as `terminal current`.

### `terminal split`

Create a new plain shell terminal in a new pane adjacent to the selected
terminal. `--current` selects the caller. `--terminal` selects an existing
terminal in the caller's workspace. The two forms are mutually exclusive and
one is required. The direction is required so the server never guesses from
window geometry or focus state.

The default working directory is the selected terminal's last known working
directory, with the workspace terminal default as the fallback. `--cwd` and
`--focus` follow `terminal open` semantics. A split creates an empty terminal;
it does not use Flint's clone mode and does not copy the selected shell state.

The server splits the exact pane that owns the selected `TerminalView`,
through `Pane::split` on that pane, as "Reuse Flint's own terminal creation
path" describes. It must work for a terminal in the terminal panel and for a
terminal in the workspace center.

Do not copy pane ownership into `TerminalControlRegistry`. `Workspace` already
owns the current item-to-pane map: `ItemHandle::added_to_pane` updates
`Workspace::panes_by_item` when an item is added or moved, including items in
the terminal panel. At dispatch time, resolve the selected view through
`Workspace::pane_for_item_id`. Read `Pane::in_center_group` to select the
workspace-center or terminal-panel placement surface. Add a shared helper that
returns the exact pane and its owning `PaneGroup` without reading active-pane
or focus state. A missing pane or group is `invalid-placement`.

Pane identity remains an internal placement detail; it does not become a
public control ID.

### Spatial addressing

The user says "the terminal on the right" or "the one below". `terminal list`
answers only with identity today, so an agent cannot map that phrase to a
terminal. Extend the `terminal list` result. Add no new command.

Flint already has the primitive, and it is the one the user's own pane
navigation uses: `PaneGroup::find_pane_in_direction`
(`crates/workspace/src/pane_group.rs`), called today by
`TerminalPanel::activate_pane_in_direction`. Its answer therefore agrees with
what the user sees. It works on pixel bounding boxes rather than on the split
tree, which stays correct for uneven and nested layouts where a tree walk
would not.

Add two optional blocks to each entry of the `terminal list` result:

- **`neighbors`**: the navigation neighbor in each direction -- the same pane
  `activate_pane_in_direction` would jump to -- as
  `{"left": <id|null>, "right": <id|null>, "up": <id|null>, "down": <id|null>}`.
  Call it a navigation neighbor, not the nearest terminal:
  `find_pane_in_direction` samples one point just past the pane's edge and
  returns whichever pane sits there, rather than searching for the closest
  pane in that direction. A pane offset from that sampled point can therefore
  be missed even though it is the closest pane in that direction, and
  `null` then means "no neighbor found at that point," not "provably no pane
  exists in that direction." A direction resolves to the neighboring pane's
  **active** terminal tab, because a pane can hold several.
- **`placement`**: the surface, either `panel` or `center`; an opaque
  per-response key that groups the terminals sharing one pane; and the
  terminal's tab index inside that pane. The grouping key lets an agent say
  "the two terminals in the right-hand pane" without a public pane ID, which
  "Terminology: `terminal split` matches tmux and Herdr; `terminal open` does not" forbids.

Four rules keep the answer honest:

1. **Report, do not guess, when geometry is unknown.** Bounding boxes are
   filled during `prepaint` (`pane_group.rs`). A terminal panel that is closed,
   or a pane group that never drew, has no bounds. Omit both blocks for those
   terminals rather than sending nulls, so an agent can tell "no neighbor" from
   "position unknown".
2. **A single pane is not an error.** `PaneGroup::bounding_box_for_pane`
   returns `None` when the root is a lone `Member::Pane`. Such a layout has no
   neighbors, so report `null` in every direction, not an error.
3. **Stay inside one surface.** The terminal panel owns its own
   `center: PaneGroup`, and the workspace center owns another. A terminal in
   the panel has no spatial relation to a terminal in the workspace center.
   `Pane::in_center_group` already records the surface. Never return a
   neighbor from the other surface.
4. **Stay inside the caller's workspace.** Spatial data obeys the same
   workspace boundary as every other terminal command.

This needs no new registry state. For each response, resolve the current pane
from `Workspace::pane_for_item_id`, read `Pane::in_center_group`, and use the
same exact-pane helper as `terminal split` to get the owning `PaneGroup`.

The skill should prefer an explicit `--terminal <id>` from a previous
`terminal list --all` over a direction word. It uses the caller entry's
`neighbors` to resolve the user's phrase into an ID first, then addresses that
ID. It must tell the user when position is unknown instead of choosing a
terminal.

### `thread create`

Only a resolved Agent Thread can use `thread create`, as today. The command
continues to validate the agent kind, prompt support, worktree mode, and
remote route before it changes UI state.

For `--worktree current`:

- Without `--split`, create the Agent Thread as another item in the caller's
  current terminal pane.
- With `--split`, create it in a new pane adjacent to the caller in the
  requested direction.
- Do not move focus unless `--focus` is present.

For `--worktree new`, Flint creates or opens the destination worktree
workspace and launches the Agent Thread there. `--split` is invalid because
the caller's pane belongs to another workspace and there is no target pane in
the destination workspace. `--focus` activates the destination only when the
caller asks for it. The default remains a background, non-activating
workspace.

`launch_seeded_thread`'s own doc comment states its current limit: "Only
used on the configured (non-managed) route -- handoff does not support the
managed/tunneled route yet." `launch_seeded_and_respond` in `control.rs`
calls it directly, with no route dispatch, unlike `launch_new_thread`, which
already dispatches between `launch_configured_thread` and
`launch_managed_thread_for_route` through `new_thread_launch_route`. As
written today, a seeded `thread create` on a project using the Tunneled
route does not reach the managed launch path at all, contradicting "Access
and remote boundaries"'s promise of Direct and Tunneled parity for `thread
create`.

Give `launch_seeded_thread` the same route dispatch `launch_new_thread`
already has: a route-aware seeded launcher that calls
`launch_managed_thread_for_route` for the Tunneled route and the existing
configured path otherwise. `launch_managed_thread_for_route` is already
`Task`-based (`cx.spawn_in`), so extend it to return the created terminal
result rather than leaving it fire-and-forget, matching the awaitable
contract "Reuse Flint's own terminal creation path" requires for
`terminal open` and `terminal split`.

Refactor `launch_seeded_thread` so the control handler receives the created
terminal view. Keep its existing Boolean meaning as well: the Boolean reports
whether the kind accepts a seeded initial prompt, and
`launch_seeded_and_respond` in `control.rs` turns `false` into a specific
error. Return both values, for example as a struct or an enum. Do not replace
the Boolean with the terminal view. The success response includes:

```json
{
  "worktree": "/path/to/worktree",
  "terminal": {
    "id": "terminal-18-f8653f52-6d0e-498c-b5ff-06d53fd01df1",
    "title": "Codex",
    "working_directory": "/path/to/worktree",
    "is_agent_thread": true,
    "has_exited": false
  }
}
```

This lets the caller immediately use `terminal read`, `terminal wait-output`,
or other terminal commands without racing `terminal list`.

### Creation completion and errors

A creation request succeeds only after the PTY, `TerminalView`, pane
placement, Agent Thread registration when applicable, and
`TerminalControlRegistry` entry exist. Return the created terminal metadata
in the same response. Do not leave an unplaced terminal or an unregistered
Agent Thread when a later creation step fails.

Add these typed errors:

```text
invalid-working-directory
invalid-split-direction
invalid-placement
terminal-route-mismatch
terminal-create-failed
terminal-placement-failed
remote-terminal-create-failed
```

Represent `direction` and the optional `thread create` split value as raw
protocol strings. Parse them in the control handler after request decoding.
Only `left`, `right`, `up`, and `down` are valid; any other decoded string
returns `invalid-split-direction`. The `flintctl` CLI still uses a value enum
and rejects an invalid command-line value before it sends a request. A raw or
newer protocol client gets the typed server error. Malformed JSON remains
`invalid-request` because the server cannot decode a command from it.

`flintctl status --json` reports `terminal-open`, `terminal-split`,
`terminal-placement`, and the current `thread-create` capability separately. A
client must not infer support from the protocol minor version alone.
`terminal-placement` covers the `neighbors` and `placement` blocks in the
`terminal list` result, so a client can tell an older server from a server
whose panel simply has not drawn yet.

### Access and remote boundaries

Any caller that resolves to a live controllable terminal can use
`terminal open` and `terminal split`. Only a caller that also resolves to a
registered Agent Thread can use `thread create`. Every explicit target must
belong to the caller's workspace.

Local and remote callers get the same command forms, defaults, response
shapes, focus behavior, and workspace checks. The owning workspace controls
the new PTY host:

- A local workspace creates only local terminals.
- A remote workspace creates only remote terminals through its authenticated
  remote connection.
- `terminal open` uses the caller terminal only to select the workspace and
  exact pane.
- `terminal split` uses the selected terminal only to select the workspace,
  exact pane, and split position.
- A remote `--cwd` is a path on the remote host and uses that host's path
  style and directory validation.
- Local Flint owns pane placement and terminal metadata for both workspace
  routes. The remote server owns only the remote PTY process and its
  registration.

There is no per-terminal local or remote route choice. A terminal registry
entry must match its owning workspace route. Registration and creation return
`terminal-route-mismatch` if the PTY host does not match the workspace. Flint
does not keep a compatibility path for a local terminal in a remote workspace.

The remote bridge forwards the same control request to local Flint. Local
Flint creates the terminal through the existing project terminal route and
waits for the new remote PTY registration before it returns success. Two
failure directions need cleanup, not one: a remote creation failure returns a
typed error and cleans up local partial placement, and -- the direction the
first version of this design left unspecified -- a remote PTY that starts
successfully followed by a local placement, registration, or cancellation
failure must terminate that remote PTY and remove its registration. Neither
side of a failed creation may outlive the other.

`thread create` uses the destination workspace route for its terminal PTY.
For `--worktree current`, this is the caller's workspace. A worktree created
from a remote workspace also gets a remote workspace and remote PTY. Placement
options must not change executable, credential, or traffic routing. Direct
uses the configured ambient remote executable. Tunneled uses the pinned
Flint-managed remote executable and its existing local traffic tunnel.

### Skill behavior

After the probe succeeds, the skill uses the installed executable's help as
the syntax authority. Local and remote managed skills use their respective
release-matched markers and the same command policy.

#### Choosing between `terminal open` and `terminal split`

The two commands look similar in a request ("give me a terminal", "open
another terminal") but land the new terminal in visually different places, as
"Terminology" describes: `terminal open` adds a terminal behind the caller's
current tab, and `terminal split` draws a second, visible box beside it. (The
"behind the current tab" default is a requirement on the implementation, not
yet the existing behavior -- see "Reuse Flint's own terminal creation path".)
The decision that matters is not "does the user want a new terminal" -- both
commands give one -- it is **whether the user needs to see the new terminal
and the current one on screen at the same time.**

- **`--focus` does not answer this question.** `terminal open --focus`
  switches which tab is showing; it still shows exactly one terminal at a
  time, the same as clicking a different browser tab. Only `terminal split`
  puts two terminals on screen together. An agent that reaches for `--focus`
  when the user wanted simultaneous visibility gives the wrong result even
  though a terminal did appear.
- **Use `terminal split`** when the request names a direction ("open one on
  the right", "below this"), names "split" or "pane" directly, or states or
  implies watching both at once ("so I can watch the build while I keep
  working here", "run the tests beside it").
- **Use `terminal open`** for a plain, undirected request for another
  terminal ("give me a terminal", "open a shell", "start a terminal to run
  this in the background"), and for any case where the user does not care
  whether the current terminal stays visible.
- **When the request is genuinely ambiguous, prefer `terminal open`.** It is
  the less disruptive action once the default-behind-current-tab fix lands:
  it does not rearrange the caller's visible layout, and a background tab is
  trivial for the user to bring forward, while an unwanted split pane is a
  layout change the user has to undo by hand. This matches the general rule
  below against doing more than the request needs.

`thread create` is a separate decision, not a third option in this choice: use
it when the user asks for another coding agent, or when the requested work
needs a delegated Agent Thread rather than a plain shell.

The skill preserves the caller's working directory unless the user asks for
another directory. It does not request focus unless the user asks to focus the
new terminal. It must not create a worktree or Agent Thread only because a
plain terminal is sufficient.

When the user names a terminal by position, the skill resolves the phrase
through `neighbors` in a `terminal list --all` result and then addresses the
resulting `TerminalControlId`. `--all` is required because the default list
excludes the caller, and the caller's own entry supplies the neighbor relation
for phrases such as "the terminal on the right". It must not send a direction
word as an address. If the blocks are absent, position is unknown: tell the
user and ask which terminal, rather than choosing one. Use Flint's meaning of
"pane" in anything the user reads, because a tmux or Herdr meaning describes a
different layout.

## Non-goals

- Giving a daemon-routed caller the same strong ancestry proof as a direct
  child of a Flint PTY. The cwd, kind, and session fallback has the weaker
  same-user boundary described above.
- Solving ambiguity for a daemon-routed kind that has no per-session
  environment variable available on its tool-call processes. Such a kind
  stays unresolved when two of its threads share a worktree, same as today.
- Guaranteeing immediate control for concurrent fresh Codex threads before
  Flint has attached their session IDs.
- Inventing a Codex session-registration channel that Codex does not support.
  This design extends the existing history discovery to remote projects but
  keeps its conservative association rules.
- Moving, closing, resizing, or reordering existing panes through
  `flintctl`.

## Open questions

- Does `CODEX_THREAD_ID` hold the same identifier as the rollout field
  `session_meta.payload.id` that Flint stores? See "Required assumption: the
  two IDs are the same identifier". Verify this before implementation step 2.
  The whole tie-break depends on it.
- Does every supported Codex version put `CODEX_THREAD_ID` in every tool-call
  process? The implementation must fail closed when it is absent, regardless
  of the answer.
- Do the macOS and Windows production builds have permission to read the peer
  process environment? See "Decided: the server reads the peer's
  environment". The implementation must treat an empty result as no match,
  and the release validation must exercise both platforms.
- Can a supported future Codex hook or app-server protocol report the session
  ID together with a Flint-provided terminal identity? If so, a later design
  can remove the fresh-session readiness limit.

## Verification

- Extend the existing `control.rs` test suite with:
  - Two same-kind candidates tied to the same worktree, disambiguated by a
    matching `caller_session_env_var` value on the connecting test process.
  - The same setup with no matching stored session ID (stays unresolved).
  - The same setup with the stored session ID matching more than one
    candidate (stays unresolved).
  - A kind with no `caller_session_env_var` set still refuses to
    disambiguate same-kind candidates (regression coverage for
    `resolve_caller_thread_refuses_an_ambiguous_cwd_match`).
  - A matching peer session ID with candidates whose stored session IDs are
    all missing stays unresolved.
  - A caller session ID that matches one candidate while another same-kind
    candidate is still unassociated stays unresolved (rule 2 of "Session
    association readiness").
  - A caller session ID that is absent, empty, or an invalid operating-system
    string stays unresolved.
  - Environment access that returns no data stays unresolved, and a peer
    process that exits during inspection stays unresolved.
- Add an automated end-to-end control test with a detached process, two live
  Codex-kind terminals in one worktree, and distinct attached session IDs.
  Verify that `terminal current --json` selects the matching terminal.
- Add a test that `terminal current --json` stays unresolved for two
  concurrent fresh Codex-kind terminals whose session IDs are not attached.
- Add skill tests for a successful Agent Thread probe, an ordinary-terminal
  response that keeps the terminal commands and withdraws only the thread
  commands, a missing marker, a missing endpoint, a connection failure, a
  protocol mismatch, and `caller-not-recognized`.
- Add a test that stage 1 stops before it runs `flintctl` when the endpoint is
  absent, so a caller outside Flint never pays the retry backoff.
- Add a test that a stale `TERM_PROGRAM=flint` or `ZED_TERM=true` in the
  caller's environment changes no decision, in either direction.
- Add skill tests for the `terminal open` versus `terminal split` choice: a
  directional request chooses split, a "so I can watch both" request chooses
  split, a plain undirected request chooses open, and an ambiguous request
  chooses open. Add a test that `--focus` is never substituted for
  `terminal split` when the request asks to see both terminals at once.
- Add tests for spatial addressing: `neighbors` in all four directions across
  an uneven and a nested split; a lone pane reporting `null` in every
  direction; both blocks omitted when the pane group has not drawn; no
  neighbor returned across the panel and workspace-center surfaces; no
  neighbor returned outside the caller's workspace; a direction resolving to
  the neighboring pane's active tab while `placement` still lists that pane's
  other tabs; the caller entry in `terminal list --all` resolving a position
  while the default caller-excluding list cannot; and the `placement` grouping
  key staying stable within one response and absent from every other result.
- Add protocol and CLI tests for `terminal open`, `terminal split`, the four
  split directions, mutually exclusive targets, cwd validation, focus flags,
  preserved `--name` and `--json` options, typed errors, capability reporting,
  and the expanded `thread create` result. Send an invalid direction through
  a raw protocol request and verify `invalid-split-direction`; verify that the
  CLI rejects the same value before transport.
- Add GPUI tests that create and register a terminal in the caller's pane,
  split beside the exact selected terminal in both the terminal panel and
  workspace center, keep focus by default, focus only when requested, and
  clean up all partial state after failure.
- Add a test that a control-created terminal is a Flint-managed terminal: it
  is an item of a `TerminalPanel` pane, it appears in `TerminalControlRegistry`,
  and it survives workspace serialization and restore the same as a
  user-created terminal.
- Add a test that a control-created split keeps focus on the caller while the
  same split through the normal UI action still focuses the new pane, so the
  new conditional focus step does not change user-facing behavior.
- Add a test that `create_adjacent_terminal`'s returned `Task` resolves only
  after the new pane, terminal, and registry entry all exist, and that a
  placement failure inside it surfaces as `Err` to an awaiting caller rather
  than a silent no-op, covering both the control caller (awaits) and the UI
  event handler (detaches and logs).
- Add a test that a control-created `terminal open` without `--focus` leaves
  the caller's pane showing its previous active tab, and a test that
  `--focus` makes the new terminal both the active tab and the focused one.
- Add Agent Thread tests for current-worktree tab placement, each split
  direction, returned terminal identity, new-worktree background behavior,
  rejection of `--split` with `--worktree new`, and exact error propagation.
- Test that an ordinary local terminal can open and split terminals but
  cannot create an Agent Thread.
- Test local, Direct remote, and Tunneled remote boundaries for all three
  creation operations. For Direct and Tunneled, verify matching command forms,
  cwd behavior, placement, focus, returned identity, cleanup, and exact
  executable and traffic boundaries. Verify that a remote workspace rejects a
  local terminal registration and that terminal and Agent Thread creation
  never fall back to a local PTY.
- Add a test that a seeded `thread create` on a project using the Tunneled
  route reaches `launch_managed_thread_for_route`, not the configured-route
  path, and returns the created terminal's metadata the same as the Direct
  route does.
- Add a test that a remote PTY created successfully, followed by a local
  placement or registration failure, terminates that remote PTY and removes
  its registration -- the failure direction opposite the existing
  remote-creation-failure cleanup test.
- Add remote bridge tests for connection-bound kind, session-environment
  variable name, session ID, and tied-worktree metadata; update after session
  discovery and retie; linked-worktree repository matching; same-kind
  disambiguation; stale-generation rejection; disconnect cleanup; and
  missing-metadata fail-closed behavior.
- Add remote session-discovery tests for fresh Codex threads, reconnect,
  history lookup failure, already-bound IDs, and ambiguous concurrent
  sessions.
- Verify on Linux, macOS, and Windows that an environment-read failure counts
  as no match. A manual platform check can supplement the automated tests but
  cannot replace the detached-process regression test.
- Remove the production and test references to the exact variable
  `FLINT_AGENT_THREAD`, which appear only in `store.rs` and `SKILL.md`. Do
  not remove `FLINT_AGENT_THREAD_ID` in
  `crates/agent_threads/src/remote_process.rs`. That is a different variable
  and it carries the remote lifecycle guard that stops orphaned remote agent
  processes. A substring search matches it too.

## Implementation order

1. Add `caller_session_env_var` to `AgentKindDefinition`; set it for Codex
   only.
2. Add the session-ID tie-break step to `resolve_caller_thread` in
   `control.rs`, gated on there being more than one same-kind candidate
   after the existing steps.
3. Add the new tests listed under Verification.
4. Update `SKILL.md` to use the two-stage gate: the marker-and-endpoint
   negative check, then the `flintctl terminal current --json` probe. Correct
   its frontmatter description so it does not restrict terminal commands to
   Agent Threads.
5. Remove `FLINT_AGENT_THREAD`, `apply_control_skill_environment`, and their
   tests.
6. Add the session-ID tie-break step to
   `docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md`'s
   "Caller resolution and access boundary" section, so the two documents stay
   consistent. That section already carries the weaker-fallback caveat; only
   the new step is missing.
7. Update
   `openspec/changes/add-flintctl-terminal-control/specs/terminal-agent-threads/spec.md`
   and its related tasks and design text to replace the environment gate with
   the control-server probe.
8. Keep `Workspace::panes_by_item` as the pane-ownership authority. Add a
   shared helper that resolves an item to its current exact pane, placement
   surface, and owning `PaneGroup`, and enforce that each terminal PTY host
   matches its owning workspace. Then make the four changes "Reuse Flint's
   own terminal creation path" lists:
   extract `TerminalPanel`'s split-creation body into an awaitable
   `create_adjacent_terminal` method that both the UI event handler and
   control code call; make its focus step conditional; replace
   `add_terminal_shell_internal`'s use of `Pane::add_item` with
   `Pane::add_item_inner` and an explicit `activate` argument distinct from
   `focus_item`, plus an explicit-pane variant of that function; and an
   explicit source terminal or working directory for
   `new_pane_with_active_terminal`. Do not add a second creation path.
9. Extend conservative session discovery to remote projects and synchronize
   the kind, `caller_session_env_var`, attached session ID, and tied worktree
   root to the connection-bound remote PTY registration. Update the
   registration after session discovery and retie, with generation checks.
10. Add `terminal open` and `terminal split` to the protocol, CLI, local and
   remote dispatchers, capability result, and installed skills.
11. Give `launch_seeded_thread` the same route dispatch `launch_new_thread`
    already has, and make `launch_managed_thread_for_route` return the
    created terminal result instead of remaining fire-and-forget. Then
    extend `thread create` with current-worktree split placement, focus
    control, and the created terminal metadata result. Keep new-worktree
    creation non-activating by default.
12. Add the `neighbors` and `placement` blocks to the `terminal list` result
    through `PaneGroup::find_pane_in_direction`, report the
    `terminal-placement` capability, and teach the installed skills to use
    `terminal list --all` to resolve a position word into an ID before they
    address a terminal.
13. Reconcile
    `docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md`,
    `docs/superpowers/specs/2026-08-22-flintctl-remote-dev-design.md`, and the
    active OpenSpec with what the implementation actually landed. Both design
    documents already carry the creation commands, access rules, remote
    behavior, response shapes, errors, and capabilities. Correct any
    difference instead of adding the sections again, and add the same content
    to the OpenSpec, which does not have it yet.
