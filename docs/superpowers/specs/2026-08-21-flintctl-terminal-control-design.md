# `flintctl` and Terminal Control

## Status

This document is a design proposal. No implementation work has started.

## Goal

Replace the long `flint-agent-control` command name with `flintctl` and make
the command a stable local control surface for Flint. Keep the existing Agent
Thread operations and add a small set of commands that let a process in one
Flint terminal inspect, operate, and create another terminal in the same
workspace. Let an Agent Thread create a sibling Agent Thread with explicit
placement and without changing user focus by default.

The first version has these user-visible command groups:

```text
flintctl status [--json]

flintctl thread retie --worktree <path>
flintctl thread create --worktree <current|new> --agent <agent> --prompt <prompt> [--split <left|right|up|down>] [--focus]

flintctl terminal current
flintctl terminal list
flintctl terminal open [--cwd <path>] [--focus]
flintctl terminal split (--current|--terminal <terminal-id>) --direction <left|right|up|down> [--cwd <path>] [--focus]
flintctl terminal read <terminal-id> [--source visible|recent|recent-unwrapped] [--lines <count>]
flintctl terminal send-text <terminal-id> <text>
flintctl terminal send-key <terminal-id> <key>...
flintctl terminal run <terminal-id> <command>
flintctl terminal wait-output <terminal-id> (--match <text>|--regex <pattern>) [--timeout <duration>]
```

`flintctl` controls a running Flint application. It is not a general terminal
multiplexer and does not keep terminal processes alive after Flint exits.

## Current state

`flint-agent-control` currently has two flat commands:

- `retie-thread` moves the calling Agent Thread to an existing worktree.
- `create-thread` starts a sibling Agent Thread in the current worktree or in
  a new linked worktree.

The client sends one JSON request to the Agent Control server for each command.
On Unix, the transport is a local socket. On Windows, it is a named pipe. The
server gets the peer process ID from the operating system and walks its process
ancestry. A request is accepted only when that ancestry contains a terminal
process that Flint tracks as a live Agent Thread.

Flint already has the terminal primitives that the new commands need:

- Every newly created or cloned split terminal has its own `Terminal`, PTY,
  and shell process. Moving a terminal to a split moves the existing terminal
  and does not create a process.
- `Terminal::input` and `Terminal::paste` write to the PTY.
- `Terminal::get_content` and `Terminal::last_n_non_empty_lines` read the
  emulated terminal grid and scrollback.
- `TerminalPanel` and `Workspace` own the terminal views and split layout.

The missing parts are an identity for each live terminal, a registry that can
resolve the calling terminal, and protocol operations for terminal access.

## Ideas adapted from Herdr

This design borrows selected control-surface ideas from the
[Herdr documentation](https://herdr.dev/docs/). Herdr is a persistent terminal
multiplexer, while Flint is an editor that owns terminal entities for the life
of the application. The shared ideas do not make the process models equal.

The adapted ideas are:

- Make the CLI the normal automation surface. Keep the socket protocol as an
  implementation interface until Flint has a reason to support third-party
  protocol clients.
- Keep public terminal identity separate from visual pane and layout identity.
  Retain internal placement state for exact open and split operations.
- Provide explicit `current`, `list`, `read`, validated-key input, command
  input, and output-wait operations.
- Provide `visible`, `recent`, and `recent-unwrapped` read sources.
- Search existing output before an event-driven wait begins.
- Pin the resolved target for the life of a wait so a replacement terminal
  cannot satisfy it.
- Return typed success results and machine-readable error codes.
- Version the protocol and require clients to ignore unknown response fields.

The first version does not copy Herdr's general workspace, tab, and pane
management, agent-state detection, event-subscription API, plugin API, or
session restore. It adds only terminal creation and one adjacent split
operation through Flint's existing pane model.

## Command name and instruction upgrades

The installed executable becomes `flintctl`. Command names use noun-first
groups so that later additions do not create another flat list:

```text
flint-agent-control retie-thread ...  -> flintctl thread retie ...
flint-agent-control create-thread ... -> flintctl thread create ...
```

The application bundle and packages contain only `flintctl`. The command does
not accept the old flat forms, and the server does not decode the old request
names.

The executable-location marker keeps its current release-channel-scoped file
name, but every Flint launch rewrites its JSON value to the running version's
`flintctl`. The same launch synchronizes a versioned Flint-managed block in
each supported installed agent's global instruction file. This replaces exact
known old blocks, removes their flat commands, and preserves all user content
outside the managed block.

The Rust crates can keep their current names during the first change. Renaming
the crates does not change user behavior and would make the functional diff
larger. Module and help text use the new `flintctl` name where they describe
the installed command.

## Terminal process model

A split is a layout operation. Its process behavior depends on the split mode:

- An empty split creates a new terminal with a new PTY and shell process.
- A cloned split creates a new terminal with a new PTY and shell process. It
  copies launch properties such as the working directory; it does not clone
  the running shell state.
- A moved split keeps the existing terminal, PTY, and process.

Because separate terminals have separate PTYs, terminal control must target a
specific terminal. Writing to a pane or to the focused UI item is not a stable
addressing rule.

## Terminal identity and registry

Add a process-local `TerminalControlRegistry` owned by the Flint application.
It records each controllable PTY terminal while that terminal is live:

```text
TerminalControlId
Terminal entity
TerminalView entity
owning Pane entity
placement surface (terminal panel or workspace center)
Workspace entity
root PTY process ID
last known working directory
creation sequence
```

`TerminalControlId` is an opaque value assigned by the registry. A suggested
display form is `t1`, `t2`, and so on. IDs are not pane indexes, tab indexes,
process IDs, or GPUI entity IDs. They are not reused during one Flint process.
They do not survive an application restart.

Register a terminal after its PTY and `TerminalView` exist. Remove it when the
terminal entity or view is released. When a terminal moves to another pane or
workspace, update its pane, placement surface, and workspace without changing
its ID.

Display-only terminals are not controllable and do not appear in `terminal
list`.

## Caller resolution and access boundary

Terminal commands must also work from an ordinary Flint terminal. Therefore,
the server cannot use only the Agent Thread store to resolve callers.

For every request, the server gets the peer process ID from the operating
system and walks up to the existing bounded ancestry depth. It first matches
that ancestry against the root PTY process IDs in `TerminalControlRegistry`.
This is the strong signal and resolves an ordinary shell command directly.

If ancestry does not match, use the existing Agent Thread cwd and agent-kind
fallback first. This supports CLIs such as Codex that delegate tool commands to
an `app-server` process outside the terminal process tree. Map the resolved
Agent Thread item back to its registered terminal ID.

For an ordinary terminal that is not a registered Agent Thread, do not use cwd
as identity. Any unrelated local process can select the same directory, so a
unique cwd match would weaken the rule that only a process in a Flint terminal
can control terminals. Ordinary terminal callers must have a matching PTY
process in their ancestry. A coding agent that uses an unrelated command daemon
must be a registered Agent Thread to use the existing constrained cwd and
agent-kind fallback. This is an explicit first-version capability limit.
Thread commands then do one additional lookup to require that the resolved
terminal is a registered Agent Thread.

The first version uses this access policy:

- A terminal command is accepted through a strong live local Flint PTY
  ancestry match or through the constrained Agent Thread daemon fallback.
- The target must belong to the same `Workspace` as the caller.
- The caller can read and write itself, although commands default to excluding
  itself from `terminal list` unless `--all` is given.
- An ordinary process outside a Flint terminal is rejected. The Agent Thread
  daemon fallback uses cwd, kind, and session signals that a same-user process
  can claim, so it is weaker than PTY ancestry and is not proof of descent
  from a Flint terminal.
- Thread commands keep their current Agent Thread-only restriction.

The socket or named pipe remains local and user-scoped. Unix socket permissions
remain `0600`. No token supplied by the client is treated as caller identity.

This boundary prevents a resolved Agent Thread in one project from operating
terminals in another open project. Cross-workspace access and general external
automation are not part of the first version.

## Agent Thread creation placement

`thread create` remains available only to a resolved Agent Thread. For
`--worktree current`, it creates the new Agent Thread as another item in the
caller's exact terminal pane. `--split <direction>` instead creates it in a
new pane adjacent to the caller. It does not move focus unless `--focus` is
present.

For `--worktree new`, Flint launches the Agent Thread in the destination
worktree workspace. `--split` is invalid because the caller's pane belongs to
another workspace. The new workspace remains in the background unless
`--focus` is present.

The success response returns the worktree and the created terminal metadata.
This lets the caller read or wait for the new Agent Thread without listing
terminals and guessing which result is new.

## Terminal commands

### `terminal current`

Return the calling terminal and its current location. This command is useful
for scripts that must avoid operating on themselves.

The JSON result includes:

```json
{
  "id": "t1",
  "title": "codex",
  "working_directory": "/path/to/project",
  "is_agent_thread": true,
  "has_exited": false
}
```

`working_directory` is a nullable string. It is `null` when
`Terminal::working_directory()` returns `None`, including before a local shell
reports its directory. Remote PTY terminals are outside the first version, but
their directory must also remain nullable if they become controllable later.

### `terminal list`

List live, controllable terminals in the caller's workspace. Results are
sorted by creation sequence, not by the current visual layout. Each item uses
the same shape as `terminal current`.

The first version does not expose pane positions. Pane layout can change when
the user drags a terminal, and terminal control does not need layout identity.

### `terminal open`

Create a plain shell terminal as another item in the caller's exact terminal
pane. Use the caller's last known working directory by default, then the
workspace terminal default when it is unavailable. An explicit `--cwd` must
be an absolute existing directory on the PTY host.

The command creates a new PTY and shell. It does not copy running shell state
and does not create an Agent Thread. It keeps focus on the caller unless
`--focus` is present. The response returns the created terminal metadata.

### `terminal split`

Create a plain shell terminal in a new pane adjacent to the selected terminal.
Exactly one of `--current` and `--terminal <terminal-id>` is required. The
direction is also required. The selected terminal must belong to the caller's
workspace.

Use the selected terminal's last known working directory by default, then the
workspace terminal default when it is unavailable. `--cwd` and `--focus`
follow `terminal open` semantics. The split is empty rather than cloned: it
does not copy the selected shell state.

The server uses the selected terminal's registered owning pane and placement
surface. It must not dispatch an active-pane UI action. The same operation
must work in the terminal panel and in the workspace center. Pane identity is
internal and is not a public control ID.

Both creation commands return only after the PTY, terminal view, placement,
and registry entry exist. A failed later step must clean up partial creation.

### `terminal read`

Return a bounded plain-text snapshot. The supported sources are:

- `visible`: the current visible terminal grid.
- `recent`: the current grid plus available primary-screen scrollback.
- `recent-unwrapped`: the same available history with soft-wrapped display
  rows joined into logical lines. This source is useful for logs and command
  output that the terminal width wrapped.

`recent` is the default. `--lines` defaults to 120 and has a configured hard
maximum. The response also reports whether the terminal is on the alternate
screen and whether output was truncated.

Rows that have left an alternate screen are not available from normal host
scrollback. The command must report this limit; it must not imply that a larger
line count can recover those rows.

The protocol caps every response at `MAX_RESPONSE_BYTES`. Text is truncated at
a UTF-8 boundary, and the JSON result sets `truncated: true`.

### `terminal send-text`

Write the supplied text as terminal input. It does not add Enter and does not
use bracketed-paste framing. This makes its behavior suitable for prompts and
interactive applications, but callers must request Enter explicitly when they
need it.

Reject NUL bytes and input larger than the protocol request limit. Return an
error when the target exited or stopped being a PTY terminal.

### `terminal send-key`

Accept a documented set of key names and modifiers, for example `enter`,
`escape`, `tab`, `backspace`, `up`, `ctrl-c`, and `alt-left`. Parse and validate
the full key list before writing any input. If one key is invalid, write
nothing.

Use the existing terminal key mapping so application cursor mode, application
keypad mode, and platform behavior match keyboard input sent through the UI.
Raw byte input is not exposed in the first version.

### `terminal run`

Atomically validate the command, write its text, and write Enter while holding
one foreground update of the target terminal. It does not invoke a separate
shell process and does not claim that the target is at a shell prompt. The
caller is responsible for selecting an appropriate target.

### `terminal wait-output`

Wait until a literal string or Rust regular expression appears in the selected
read snapshot. Search once before subscribing so output that already exists can
match. Then observe terminal content changes until the pattern matches, the
target exits, the target is released, or the timeout expires. Pin the resolved
terminal entity and its registry generation when the wait starts. A terminal
that later replaces the item in the same pane or tab cannot satisfy the wait.

The default read source is `recent`. A timeout is required at the protocol
layer; the CLI supplies a conservative default when the user omits it.

Long-lived waits require a transport change. The current Unix connection reads
one request to EOF, which requires the client to close its write half. After
that EOF, the server cannot use another read to distinguish a connected client
from a disconnected client. New-protocol clients therefore send a bounded,
length-prefixed request and keep the connection open. While a wait is pending,
`handle_connection` races the terminal observation against a read that
completes only when the client disconnects. A disconnect cancels the wait and
its observation task. Windows uses the same message-length rule over its named
pipe.

The server accepts only the current length-prefixed framing. Framing detection
and request-size checks happen before JSON decoding.

The result includes the final bounded snapshot so a caller does not need an
immediate second request.

## Protocol shape

Keep the one-request-per-connection model. Extend `ControlRequest` with grouped
wire variants. The serialized names remain explicit and do not depend on Clap
types:

```text
thread-retie
thread-create
terminal-current
terminal-list
terminal-open
terminal-split
terminal-read
terminal-send-text
terminal-send-key
terminal-run
terminal-wait-output
```

Add a protocol version to every request and response. The server rejects a
client whose required major version is not supported. Minor-version additions
are additive, and clients ignore fields they do not understand. A lightweight
`flintctl status --json` command reports the running Flint version, protocol
version, release channel, and supported command capabilities. This lets an
agent check support before it depends on a new operation.

Report `terminal-open`, `terminal-split`, and `thread-create` separately. A
client must not infer operation support from the protocol minor version.

Decode only the current noun-first thread request names. Responses use
specific success types for thread and terminal operations. Expected failures return
machine-readable error codes plus a message, for example:

```text
caller-not-recognized
caller-not-agent-thread
terminal-not-found
terminal-outside-workspace
terminal-exited
invalid-working-directory
invalid-split-direction
invalid-placement
terminal-create-failed
terminal-placement-failed
remote-terminal-create-failed
invalid-key
invalid-pattern
timeout
response-too-large
```

Keep `ControlResponse::NotReady` as a response state separate from typed hard
errors. Return it when the caller cannot yet be distinguished from a terminal
whose registry entry has not completed. `flintctl` applies the existing
bounded client-side retry backoff to thread and terminal commands. If retries
end without a match, the CLI reports that the caller is not recognized.

An explicit target ID that was returned by `terminal list` but is no longer in
the registry is a hard `terminal-not-found` error and is not retried. A newly
created terminal has no public ID until registration completes, so scripts
must obtain its ID from a successful create result or a retried list request.

The CLI maps these failures to a nonzero exit status. `--json` prints the full
response. Human-readable output stays concise and does not change the response
written by the server.

## Foreground and async work

The socket accept loop and transport IO remain asynchronous. GPUI entities can
only be read or updated on the foreground thread. Each request therefore uses
`AsyncApp` to resolve registry entries and to perform a short entity read or
update.

`terminal wait-output` must not hold an entity update across an await point. It
registers an observation, returns to the async task, and re-reads the terminal
after a notification. Dropping the target entity or the connection cancels the
observation and completes the request with a defined error.

Input operations validate all data before the foreground terminal update. A
multi-key request and `terminal run` perform one update so another control
request cannot interleave bytes within that operation.

## Remote behavior

Remote development provides the same terminal command forms, defaults,
response shapes, focus behavior, and workspace checks through the authenticated
`flint-remote-server` bridge described in
`docs/superpowers/specs/2026-08-22-flintctl-remote-dev-design.md`.

Creation follows the source terminal route. `terminal open` follows the caller
terminal. `terminal split` follows the selected terminal. Local Flint owns the
new terminal view and pane placement. The remote server owns a remote PTY and
its connection-bound registration when the selected route is remote. A remote
`--cwd` is interpreted and validated on the remote host.

Direct and Tunneled Agent Thread routes, including `thread create`, keep their
existing launch and credential boundaries. Direct uses only the configured
ambient executable. Tunneled uses only the pinned Flint-managed executable and
its existing local traffic tunnel.

## Packaging and discovery

Build and package `flintctl` beside the current helper location on every
supported platform. Update Agent Thread instructions to discover the executable
through the existing marker. Do not require `flintctl` to be in `PATH`.

`flintctl` does not locate and start Flint. If no matching control endpoint
exists, it reports that Flint is not running or that the release channel does
not match.

Release-channel scoping remains part of the endpoint and marker names. A Stable
client must not connect to a Nightly or development Flint process by accident.

## Verification

Protocol tests cover serialization, rejection of old request names, response
limits, and all error codes. CLI tests cover the current command hierarchy,
human output, JSON output, and rejection of old flat commands.

Server tests cover:

- caller resolution from ordinary terminals and Agent Thread terminals;
- denial for an external process and for a target in another workspace;
- registration, release, move, and non-reuse of terminal IDs;
- terminal open in the caller's exact pane, default and explicit cwd, returned
  identity, no-focus default, explicit focus, and partial-failure cleanup;
- terminal split in all four directions beside an exact selected terminal in
  both the terminal panel and workspace center;
- visible, recent, and recent-unwrapped reads, line limits, UTF-8 truncation,
  and alternate-screen reporting;
- `recent-unwrapped` reconstruction across soft-wrapped rows;
- text input, validated keys, atomic run input, exited terminals, and
  display-only terminals;
- immediate and delayed output matches, invalid regular expressions, timeout,
  pinned-target replacement, target release, and client cancellation;
- Unix socket permissions and Windows peer-process verification;
- unchanged `thread retie` behavior and `thread create` tab, split, focus,
  returned-identity, and new-worktree placement behavior;
- denial of Agent Thread creation from an ordinary terminal;
- local, Direct remote, and Tunneled remote command parity for terminal open,
  terminal split, and Agent Thread creation, including cwd, placement, focus,
  returned identity, cleanup, executable, credential, and traffic boundaries.

Package tests verify that `flintctl` is in the macOS application bundle and
Linux and Windows packages and that `flint-agent-control` is absent.
Instruction tests verify that every new Flint launch installs the current
`flintctl` discovery text.

## Non-goals

The first version does not:

- keep terminals alive after Flint exits;
- provide a persistent terminal host or terminal multiplexer;
- move, close, resize, or reorder existing terminal panes;
- expose raw PTY file descriptors or arbitrary byte input;
- infer whether a terminal is idle, blocked, or at a shell prompt;
- recognize or name the coding agent in an ordinary terminal;
- allow cross-workspace or remote-host terminal control;
- provide a public network API or accept client-provided authentication tokens;
- guarantee terminal IDs across Flint restarts.

General pane management and Agent Thread state detection can be later
additions. They must use explicit capabilities and must not expand the first
version's access boundary implicitly.

## Implementation order

1. Add the `flintctl` command hierarchy, protocol version and status result,
   launch-time Agent Thread instruction synchronization, and packaging. Remove
   the old executable and reject old flat commands and request names.
2. Add `TerminalControlRegistry`, terminal lifecycle registration, caller
   resolution, and `terminal current` and `terminal list`.
3. Add bounded terminal reads and their alternate-screen metadata.
4. Add validated text and key input plus `terminal run`.
5. Add current-only length-prefixed framing and cancellable
   `terminal wait-output`.
6. Retain exact internal pane and placement-surface state in the terminal
   registry and add shared placement helpers that do not depend on UI focus.
7. Add `terminal open` and `terminal split`, their typed errors, capability
   reporting, returned identity, and local-only route checks.
8. Extend `thread create` with current-worktree split placement, focus
   control, and the created terminal metadata result.

Each stage keeps the current thread commands working and can be tested before
the next stage changes the control surface.
