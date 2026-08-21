# `flintctl` and Terminal Control

## Status

This document is a design proposal. No implementation work has started.

## Goal

Replace the long `flint-agent-control` command name with `flintctl` and make
the command a stable local control surface for Flint. Keep the existing Agent
Thread operations and add a small set of commands that let a process in one
Flint terminal inspect and operate another terminal in the same workspace.

The first version has these user-visible command groups:

```text
flintctl thread retie --worktree <path>
flintctl thread create --worktree <current|new> --agent <agent> --prompt <prompt>

flintctl terminal current
flintctl terminal list
flintctl terminal read <terminal-id> [--source visible|recent] [--lines <count>]
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

## Command name and compatibility

The installed executable becomes `flintctl`. Command names use noun-first
groups so that later additions do not create another flat list:

```text
flint-agent-control retie-thread ...  -> flintctl thread retie ...
flint-agent-control create-thread ... -> flintctl thread create ...
```

The application bundle and Linux packages keep a `flint-agent-control`
compatibility executable for two stable releases. It accepts the two old
commands, prints one deprecation message to standard error, and sends the same
protocol requests as `flintctl`. Scripts that request `--json` still get only
JSON on standard output.

The executable-location marker keeps its current file name during the
compatibility period, but its JSON `executable` value points to `flintctl`.
This avoids an atomic migration problem: existing instructions can find the
marker, while new Agent Threads immediately use the new executable. After the
compatibility period, a separate cleanup change can rename the marker and
remove the old executable.

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
Workspace entity
root PTY process ID
creation sequence
```

`TerminalControlId` is an opaque value assigned by the registry. A suggested
display form is `t1`, `t2`, and so on. IDs are not pane indexes, tab indexes,
process IDs, or GPUI entity IDs. They are not reused during one Flint process.
They do not survive an application restart.

Register a terminal after its PTY and `TerminalView` exist. Remove it when the
terminal entity or view is released. When a terminal moves to another pane or
workspace, update its registry location without changing its ID.

Display-only terminals are not controllable and do not appear in `terminal
list`.

## Caller resolution and access boundary

Terminal commands must also work from an ordinary Flint terminal. Therefore,
the server cannot use only the Agent Thread store to resolve callers.

For every request, the server gets the peer process ID from the operating
system and walks up to the existing bounded ancestry depth. It matches that
ancestry against the root PTY process IDs in `TerminalControlRegistry`. The
first matching live terminal is the calling terminal. Thread commands then do
one additional lookup to require that this terminal is a registered Agent
Thread.

The first version uses this access policy:

- A terminal command is accepted only from a live local Flint PTY terminal.
- The target must belong to the same `Workspace` as the caller.
- The caller can read and write itself, although commands default to excluding
  itself from `terminal list` unless `--all` is given.
- A process outside a Flint terminal is rejected, even when it runs as the
  same operating-system user.
- Thread commands keep their current Agent Thread-only restriction.

The socket or named pipe remains local and user-scoped. Unix socket permissions
remain `0600`. No token supplied by the client is treated as caller identity.

This boundary prevents an Agent Thread in one project from operating terminals
in another open project. Cross-workspace access and access from an external
automation process are not part of the first version.

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

### `terminal list`

List live, controllable terminals in the caller's workspace. Results are
sorted by creation sequence, not by the current visual layout. Each item uses
the same shape as `terminal current`.

The first version does not expose pane positions. Pane layout can change when
the user drags a terminal, and terminal control does not need layout identity.

### `terminal read`

Return a bounded plain-text snapshot. The supported sources are:

- `visible`: the current visible terminal grid.
- `recent`: the current grid plus available primary-screen scrollback.

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
target exits, the target is released, or the timeout expires.

The default read source is `recent`. A timeout is required at the protocol
layer; the CLI supplies a conservative default when the user omits it. Cancel
the server task when the client disconnects.

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
terminal-read
terminal-send-text
terminal-send-key
terminal-run
terminal-wait-output
```

During compatibility, decode the existing `retie-thread` and `create-thread`
request names as aliases for the new thread variants. Responses use specific
success types for thread and terminal operations. Expected failures return
machine-readable error codes plus a message, for example:

```text
caller-not-recognized
caller-not-agent-thread
terminal-not-found
terminal-outside-workspace
terminal-exited
invalid-key
invalid-pattern
timeout
response-too-large
```

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

The first version controls only terminals whose PTY process runs on the same
machine as the Flint control server.

For a remote project:

- A local terminal created with Flint's local route can use local `flintctl`.
- A terminal whose shell runs through the remote server is not exposed unless
  the local server can verify its caller process identity and route input to
  that exact terminal without weakening the workspace boundary.

No executable is copied to a remote host for this feature. Direct and tunneled
Agent Thread routes keep their existing launch and credential boundaries.
Remote terminal control needs a separate design and tests before it is enabled.

## Packaging and discovery

Build and package `flintctl` beside the current helper location on every
supported platform. Update Agent Thread instructions to discover the executable
through the existing marker. Do not require `flintctl` to be in `PATH`.

The compatibility executable can be a small wrapper or a second binary target
that shares the same client library. It must not locate and start Flint. If no
matching control endpoint exists, it reports that Flint is not running or that
the release channel does not match.

Release-channel scoping remains part of the endpoint and marker names. A Stable
client must not connect to a Nightly or development Flint process by accident.

## Verification

Protocol tests cover serialization, old request aliases, response limits, and
all error codes. CLI tests cover the new command hierarchy, human output, JSON
output, and compatibility commands.

Server tests cover:

- caller resolution from ordinary terminals and Agent Thread terminals;
- denial for an external process and for a target in another workspace;
- registration, release, move, and non-reuse of terminal IDs;
- visible and recent reads, line limits, UTF-8 truncation, and alternate-screen
  reporting;
- text input, validated keys, atomic run input, exited terminals, and
  display-only terminals;
- immediate and delayed output matches, invalid regular expressions, timeout,
  target release, and client cancellation;
- Unix socket permissions and Windows peer-process verification;
- unchanged `thread retie` and `thread create` behavior;
- local, Direct remote, and Tunneled remote route boundaries.

Package tests verify that `flintctl` and the compatibility executable are in
the macOS application bundle and Linux and Windows packages. Instruction tests
verify that newly started Agent Threads receive the `flintctl` discovery text.

## Non-goals

The first version does not:

- keep terminals alive after Flint exits;
- create, split, move, focus, close, or resize terminal panes;
- expose raw PTY file descriptors or arbitrary byte input;
- infer whether a terminal is idle, blocked, or at a shell prompt;
- recognize or name the coding agent in an ordinary terminal;
- allow cross-workspace or remote-host terminal control;
- provide a public network API or accept client-provided authentication tokens;
- guarantee terminal IDs across Flint restarts.

Pane creation and Agent Thread state detection can be later additions. They
must use explicit capabilities and must not expand the first version's access
boundary implicitly.

## Implementation order

1. Add the `flintctl` command hierarchy, protocol aliases, packaging, and the
   compatibility executable without changing server behavior.
2. Add `TerminalControlRegistry`, terminal lifecycle registration, caller
   resolution, and `terminal current` and `terminal list`.
3. Add bounded terminal reads and their alternate-screen metadata.
4. Add validated text and key input plus `terminal run`.
5. Add cancellable `terminal wait-output`.
6. Update Agent Thread instructions and remove the old command name only after
   the compatibility period.

Each stage keeps the current thread commands working and can be tested before
the next stage changes the control surface.
