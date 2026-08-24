# Agent-Initiated Terminal Creation

## Status

Design proposal. No implementation work has started.

**Scope authority is
`docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md`.** That
document's Goal already includes terminal creation in its first version, and its
implementation stages 7 and 8 cover these commands.
`docs/superpowers/specs/2026-08-22-flintctl-remote-dev-design.md` requires the
same commands on the remote route. This document is not a separate proposal and
does not defer that work; it holds the implementation detail for those stages,
split out of `2026-08-24-agent-control-caller-disambiguation-design.md`, which had
bundled it with an unrelated caller-identity fix.

Two deliberate differences from 08-21, both resolved in this document's favor:

- **Pane ownership.** This document uses `Workspace::panes_by_item` and does not
  store pane state in `TerminalControlRegistry`. See "Pane ownership".
- **Spatial addressing.** An earlier draft added `neighbors` and `placement`
  blocks to `terminal list`. This document rejects them. Neither 08-21 nor 08-22
  requires them, so nothing else changes. See "Rejected: spatial addressing".

## Defect: seeded `thread create` ignores the route

`launch_seeded_thread`'s own doc comment states its limit: "Only used on the
configured (non-managed) route -- handoff does not support the managed/tunneled
route yet." `launch_seeded_and_respond` in `control.rs` calls it directly, with
no route dispatch, unlike `launch_new_thread`, which already dispatches between
`launch_configured_thread` and `launch_managed_thread_for_route` through
`new_thread_launch_route`.

So a seeded `thread create` on a project using the Tunneled route never reaches
the managed launch path. That contradicts the Direct-and-Tunneled parity the
2026-08-21 design promises for `thread create`.

**Route dispatch alone is not the fix.** `launch_managed_thread_for_route`
(`store.rs`) cannot carry a seeded prompt as it stands:

- It builds its command with `build_new_thread_launch` *inside* the async block,
  after `prepare_managed_agent(...).await` resolves, and never calls
  `seed_launch_command_with_prompt`. Dispatching to it would launch an **unseeded**
  thread, losing the prompt silently — the exact failure the existing Boolean
  return was added to prevent.
- It returns `()` and ends in `.detach_and_log_err(cx)`, so a control caller
  cannot learn whether the launch succeeded or what terminal it produced.
- It reports failures with `workspace.show_error(...)`, a UI toast the calling
  agent never sees, and it treats
  `ManagedAgentPreparation::Cancelled | AlreadyInProgress` as a silent no-op.

So the fix is a managed **seeded** launch path that is awaitable end to end:

1. Seed the command built after preparation resolves, not the pre-preparation
   command, and keep the existing Boolean meaning — whether the kind accepts a
   seeded prompt — alongside the created terminal.
2. Return a `Task<Result<…>>` carrying the created terminal, rather than
   detaching.
3. Map preparation failure, launch failure, `Cancelled`, and `AlreadyInProgress`
   to typed control responses. Do not route them only to a UI toast.

Tests must prove behavior, not routing. A route-selection assertion alone would
pass against an unseeded launch. Verify that the launched command contains the
prompt, that it uses the pinned managed executable, that the created terminal
metadata comes back, and that preparation failure, launch failure, and
cancellation each reach the control response.

This is no longer a small drive-by fix, and it should not be scheduled as one.

## Proposed commands

```text
flintctl terminal open [--cwd <path>] [--focus] [--json]
flintctl terminal split (--current|--terminal <terminal-id>) \
  --direction <left|right|up|down> [--cwd <path>] [--focus] [--json]

flintctl thread create --worktree <current|new> [--name <name>] \
  --agent <agent> --prompt <prompt> \
  [--split <left|right|up|down>] [--focus] [--json]
```

`thread create` already exists. This change keeps it in the `thread` group,
preserves `--name` and `--json`, adds optional placement for a current-worktree
thread, and returns the created terminal identity. Do not add a second terminal
command that starts an agent.

## Terminology: `split` matches tmux; `open` does not

An agent that knows tmux already has a working model for `terminal split`: a new
rectangular region appears beside the current one, holding one new shell, with a
draggable divider between them. Flint matches that model exactly. `Pane::split`
creates a new `Pane` holding one new terminal with its own PTY
(`new_pane_with_active_terminal`, `SplitMode::EmptyPane` in `TerminalPanel`).
The divider is the same resize handle a user drags by hand.

`terminal open` is where that model breaks. It adds the new terminal as another
**tab** inside an existing pane, not as a new region. Tmux has no equivalent: a
tmux pane cannot hold a second shell hidden behind the active one. State this
plainly in the skill, because an agent that assumes `open` behaves like a small
split puts the terminal in the wrong place.

Two consequences:

1. Most panes hold exactly one terminal, but not all. `terminal split` keeps the
   1:1 relation. `terminal open` breaks it. Direction-based addressing must
   therefore resolve to a pane's **active** tab.
2. Pane identity stays private. `TerminalControlId` remains the only public
   address. Do not add a pane ID to the protocol. An agent thinking in tmux terms
   would treat a pane ID as a terminal, address the wrong thing, and get no error.

## Reuse Flint's own creation path

Every command here must create its terminal through the same functions the normal
UI actions call. Do not build a parallel path. A terminal Flint creates through
its own path is a managed terminal: `TerminalPanel` owns it, it gets a tab,
`Pane::add_item` records it for workspace serialization, and
`TerminalControlRegistry` registers it. Registration is automatic, because
`terminal_control::init` observes every new `TerminalView` (`cx.observe_new` in
`crates/agent_threads/src/terminal_control.rs`). A hand-built terminal that skips
`Pane` and `TerminalPanel` is not managed, even though the observer still
registers it.

One narrow exception, about dispatch rather than the code path: do not call
`window.dispatch_action(SplitRight …)`. Action dispatch routes to whichever pane
holds focus, so the target would be the user's focused pane, not the caller's.
Call the method the action handler calls, on the caller's own pane entity:
`caller_pane.update(cx, |pane, cx| pane.split(direction, SplitMode::EmptyPane,
window, cx))`. The resulting `pane::Event::Split` carries that pane to
`TerminalPanel`, which already calls `center.split(&source_pane, &new_pane,
direction, cx)` with the source pane from the event, so placement is already
deterministic and does not read `active_pane`.

That event handler is not usable by control code as it stands. `Pane::split`
returns nothing, and `TerminalPanel`'s handler runs the actual creation inside
`cx.spawn_in(window, …).detach()` (`terminal_panel.rs`). It returns before
creation finishes, and if `new_pane.await` comes back `None` it silently returns.
A control request would have no way to learn whether creation succeeded, when it
finished, or what the new terminal's ID is.

### Four gaps to close

Close them with parameters and one shared function, not a second creation path.

1. Extract the body of the `SplitMode::ClonePane | SplitMode::EmptyPane` arm into
   a method on `TerminalPanel`, for example
   `create_adjacent_terminal(source_pane: Entity<Pane>, clone: bool, direction:
   SplitDirection, window: &mut Window, cx: &mut Context<Self>) ->
   Task<Result<WeakEntity<Terminal>>>`. The `pane::Event::Split` handler calls it
   and detaches, as today, except it should log a failure rather than discard it
   (`task.detach_and_log_err(cx)`), per this repository's error handling rule.
   Control code calls the same method and **awaits** the task. This is the only
   new plumbing this design needs.
2. Inside that method, `window.focus(&new_pane.focus_handle(cx), cx)` must become
   conditional, so a control-created split can keep focus on the caller.
3. `add_terminal_shell_internal` always calls `Pane::add_item`, and `add_item`
   hardcodes `add_item_inner`'s **fourth** parameter, `activate`, to `true`
   (`crates/workspace/src/pane.rs`; the signature is `item, activate_pane,
   focus_item, activate, destination_index, window, cx`). So `activate_item`
   always makes the new terminal the visible tab, regardless of `focus_item`,
   which only handles keyboard focus afterward. `RevealStrategy` cannot fix this:
   it gates the panel-level reveal and the focus step, not which tab a pane shows.
   Call the already-`pub` `Pane::add_item_inner` directly with an explicit
   `activate` argument distinct from `focus_item`. Default `terminal open` (no
   `--focus`) passes `activate: false`; `--focus` passes both as `true`. Also add
   an explicit-pane variant of `add_terminal_shell_internal` that reads a
   passed-in pane instead of `terminal_panel.active_pane`.
4. `new_pane_with_active_terminal` reads `self.active_pane`, and with
   `clone = false` it takes the working directory from `default_working_directory`
   instead of the source terminal. Let the caller pass the source terminal or an
   explicit working directory.

## Command behavior

**`terminal open`.** A new plain shell terminal as another item in the caller's
current pane, with its own `Terminal`, PTY, shell process, `TerminalView`, and
`TerminalControlId`. It does not copy shell state or start an Agent Thread. The
default working directory is the caller terminal's last known one, falling back to
the workspace default. `--cwd` must be an absolute existing directory on the
machine that will own the PTY; it does not have to be inside a project root.
Without `--focus`, the new terminal stays behind the caller's current tab.

**`terminal split`.** A new plain shell terminal in a new pane adjacent to the
selected terminal. `--current` selects the caller; `--terminal` selects another
terminal in the caller's workspace. The two are mutually exclusive and one is
required. The direction is required so the server never guesses from geometry or
focus. A split creates an empty terminal, not a clone. It must work for a terminal
in the terminal panel and one in the workspace center.

See "Pane ownership" for how the server resolves the selected terminal's pane.

### Pane ownership

08-21 previously required retaining pane and placement-surface state inside
`TerminalControlRegistry`. That was corrected in 08-21 itself, in both its
`terminal split` section and its implementation stage 6, to match this section.

Do not copy pane ownership into `TerminalControlRegistry`. `Workspace` already
owns the item-to-pane map: `ItemHandle::added_to_pane` updates
`Workspace::panes_by_item` when an item is added or moved, including items in the
terminal panel. Duplicating that in the registry creates a second copy that a
drag, a move, or a pane close can silently desynchronize.

The existing code already works this way. `focus_terminal` in
`crates/agent_threads/src/terminal_control.rs` resolves its pane with
`workspace.pane_for_item_id(terminal_item_id)`, and the registry holds no pane
state today. Adding it would be new divergence, not continuity.

Resolve the selected view through `Workspace::pane_for_item_id`, and read
`Pane::in_center_group` to select the placement surface. Add a shared helper that
returns the exact pane and its owning `PaneGroup` without reading active-pane or
focus state. A missing pane or group is `invalid-placement`.

**`thread create`.** Only a resolved Agent Thread may use it, as today. For
`--worktree current`: without `--split`, add it to the caller's current pane; with
`--split`, place it in a new adjacent pane. Do not move focus unless `--focus`. For
`--worktree new`, `--split` is invalid, because the caller's pane belongs to
another workspace. The default stays a background, non-activating workspace.

Refactor `launch_seeded_thread` so the handler receives the created terminal view,
and keep its existing Boolean meaning as well: the Boolean reports whether the
kind accepts a seeded prompt, and `launch_seeded_and_respond` turns `false` into a
specific error. Return both, as a struct or enum. The success response includes:

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

This lets the caller use `terminal read` or `terminal wait-output` at once,
without racing `terminal list`.

## Completion and errors

A creation request succeeds only after the PTY, `TerminalView`, pane placement,
Agent Thread registration when applicable, and registry entry all exist. Return
the created terminal metadata in the same response. Never leave an unplaced
terminal or an unregistered thread when a later step fails.

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

This list matches 08-21 and 08-22. `terminal-route-mismatch` covers a PTY host
that does not match its owning workspace, and `remote-terminal-create-failed`
covers a remote PTY or registration failure; both are required by "Access and
remote boundaries" below.

Represent `direction` as a raw protocol string and parse it in the control handler
after decoding. Only `left`, `right`, `up`, and `down` are valid. The CLI still
uses a value enum and rejects an invalid command-line value before sending.
Malformed JSON stays `invalid-request`.

`flintctl status --json` reports `terminal-open` and `terminal-split` separately.
A client must not infer support from the protocol minor version alone.

## Access and remote boundaries

Any caller that resolves to a live controllable terminal can use `terminal open`
and `terminal split`. Only a caller that also resolves to a registered Agent
Thread can use `thread create`. Every explicit target must belong to the caller's
workspace. Local and remote callers get the same command forms, defaults, response
shapes, focus behavior, and workspace checks.

The owning workspace controls the new PTY host. A local workspace creates only
local terminals; a remote workspace creates only remote terminals through its
authenticated connection. A remote `--cwd` is a remote path. Local Flint owns pane
placement and metadata for both routes; the remote server owns only the remote PTY
and its registration. There is no per-terminal route choice — a registry entry
must match its workspace route, and a mismatch returns `terminal-route-mismatch`.

Two failure directions need cleanup, not one. A remote creation failure returns a
typed error and cleans up local partial placement. A remote PTY that starts
successfully, followed by a local placement, registration, or cancellation
failure, must terminate that remote PTY and remove its registration. Neither side
of a failed creation may outlive the other.

Placement options must not change executable, credential, or traffic routing.
Direct uses the configured ambient remote executable. Tunneled uses the pinned
Flint-managed remote executable and its existing tunnel.

## Skill guidance

The decision that matters is **whether the user needs to see the new terminal and
the current one at the same time.**

- `--focus` does not answer this. `terminal open --focus` switches which tab
  shows; it still shows one terminal at a time. Only `terminal split` puts two on
  screen together.
- Use `terminal split` when the request names a direction, names "split" or
  "pane", or implies watching both at once.
- Use `terminal open` otherwise, and when the request is ambiguous. It does not
  rearrange the caller's layout, and a background tab is trivial to bring forward,
  while an unwanted split is a change the user must undo by hand.

`thread create` is a separate decision: use it when the user asks for another
coding agent, or when the work needs a delegated thread rather than a plain shell.
The skill preserves the caller's working directory unless asked otherwise, does
not request focus unless asked, and must not create a worktree or Agent Thread
when a plain terminal is enough. Use Flint's meaning of "pane" in anything the
user reads.

## Rejected: spatial addressing

An earlier draft added `neighbors` and `placement` blocks to `terminal list`, so
an agent could resolve "the terminal on the right" through
`PaneGroup::find_pane_in_direction`.

Rejected for now. `find_pane_in_direction` samples one point just past a pane's
edge and returns whatever sits there, rather than searching for the nearest pane.
A pane offset from that point is missed even when it is the closest one in that
direction, so `null` means "no neighbor at that point", not "no pane exists
there". An API whose own documentation must say its answer can be wrong is a poor
trade for a use case nobody has requested. It also needs bounding boxes that only
exist after `prepaint`, so the blocks would be absent for any closed panel.

Revisit only if users actually address terminals by position. The primitive is
already there if so, and it is the same one `activate_pane_in_direction` uses, so
its answer would agree with what the user sees.

## Non-goals

- Moving, closing, resizing, or reordering existing panes through `flintctl`.
- A public pane ID in the control protocol.

## Verification

Written against the scope above, if this is ever scheduled:

- GPUI tests that create and register a terminal in the caller's pane, split
  beside the exact selected terminal in both the terminal panel and workspace
  center, keep focus by default, focus only when requested, and clean up partial
  state after failure.
- A test that a control-created terminal is a managed terminal: an item of a
  `TerminalPanel` pane, present in `TerminalControlRegistry`, and surviving
  workspace serialization and restore like a user-created one.
- A test that a control-created split keeps focus on the caller while the same
  split through the normal UI action still focuses the new pane, so the new
  conditional focus step does not change user-facing behavior.
- A test that `create_adjacent_terminal`'s `Task` resolves only after the new
  pane, terminal, and registry entry exist, and that a placement failure surfaces
  as `Err` to an awaiting caller rather than a silent no-op — covering both the
  control caller (awaits) and the UI handler (detaches and logs).
- A test that `terminal open` without `--focus` leaves the caller's pane showing
  its previous active tab, and that `--focus` makes the new terminal both the
  active tab and the focused one.
- Protocol and CLI tests for both commands, the four directions, mutually
  exclusive targets, cwd validation, focus flags, typed errors, and capability
  reporting. Send an invalid direction through a raw protocol request and verify
  `invalid-split-direction`; verify the CLI rejects the same value before
  transport.
- Agent Thread tests for current-worktree tab placement, each split direction,
  returned identity, new-worktree background behavior, rejection of `--split` with
  `--worktree new`, and exact error propagation.
- A test that an ordinary local terminal can open and split but cannot create an
  Agent Thread.
- Local, Direct remote, and Tunneled remote boundary tests for all three creation
  operations, including that a remote workspace rejects a local terminal
  registration and that creation never falls back to a local PTY.
- A test that a remote PTY created successfully, followed by a local placement or
  registration failure, terminates that remote PTY and removes its registration.
- Skill tests for the open-versus-split choice: a directional request chooses
  split, a "so I can watch both" request chooses split, a plain undirected request
  chooses open, an ambiguous request chooses open, and `--focus` is never
  substituted for a split when the user asks to see both at once.
