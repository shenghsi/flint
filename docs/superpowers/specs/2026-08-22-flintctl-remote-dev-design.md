# `flintctl` for Remote Development

## Status

This document is a design proposal. Local `flintctl` terminal control is
implemented. Remote terminal control is not implemented.

## Goal

Let a process in a Flint remote-development terminal use the same `flintctl`
commands as a process in a local terminal. Keep terminal state and control in
the local Flint application. Use the existing authenticated connection to
`flint-remote-server` as the bridge. Do not open a public network port.

The remote command surface is the current local command surface:

```text
flintctl status [--json]

flintctl thread retie --worktree <path> [--json]
flintctl thread create --worktree <current|new> [--name <name>] --agent <agent> --prompt <prompt> [--json]

flintctl terminal current [--json]
flintctl terminal list [--all] [--json]
flintctl terminal read <terminal-id> [--source visible|recent|recent-unwrapped|detection] [--lines <count>] [--since <cursor>] [--json]
flintctl terminal send-text <terminal-id> <text> [--json]
flintctl terminal send-key <terminal-id> <key>... [--json]
flintctl terminal run <terminal-id> <command> [--json]
flintctl terminal wait-output <terminal-id> (--match <text>|--regex <pattern>) [--source visible|recent|recent-unwrapped|detection] [--lines <count>] [--timeout <duration>] [--json]
```

`terminal read --since` is valid only with the default `recent` source. The
cursor is opaque. Remote control preserves the current defaults, limits,
results, and error codes. It does not create a second command implementation.

## Current state

Flint displays a remote terminal through a local `Terminal` entity. The remote
shell and coding agent run in a PTY managed by `flint-remote-server`. The local
`Terminal` owns the emulated screen, scrollback, input path, and Flint terminal
ID.

Local `flintctl` connects to a release-channel-scoped Unix socket or Windows
named pipe. Flint gets the peer process ID from the operating system and first
walks local process ancestry. It also has a constrained Agent Thread fallback
for delegated command processes whose ancestry does not reach the terminal
root. Flint rejects remote terminals because the remote process cannot connect
to the local endpoint and its process ancestry exists only on the remote host.

The local protocol is versioned and length-prefixed. Its current request and
response limits are 1 MiB. The remote host already runs the matching
`flint-remote-server` binary.

Local executable discovery uses a marker instead of a custom environment
variable. Some coding-agent command processes remove custom environment
variables. Remote identity and discovery must not depend on an inherited
custom environment variable for the same reason.

## Design summary

Use `flint-remote-server` as a remote `flintctl` bridge. Install a sibling
command named `flintctl` that starts the same binary in client mode. The client
connects to user-scoped control endpoints on the remote host. The selected
remote server gets the client process ID from the endpoint, resolves it to a
PTY that the server owns, and forwards the current `ControlRequest` with a
server-created caller identity to local Flint. Local Flint validates the
connection and terminal registration, then uses the existing dispatcher.

```text
Remote shell or agent
        |
        | current length-prefixed ControlRequest
        v
Remote user-scoped control endpoint
        |
        | peer process ID -> remote PTY registration
        v
flint-remote-server
        |
        | authenticated connection
        | + server-created caller registration ID
        v
Local Flint control dispatcher
        |
        | connection + registration ID -> terminal and workspace
        v
Local terminal registry and terminal model
```

The CLI does not present a capability, terminal ID, process ID, working
directory, or other identity claim. Caller identity comes from the operating
system and the remote server that owns the PTY.

Terminal content does not move to the remote server for control. Reads use the
local emulated screen and scrollback. Input uses the existing remote terminal
input path.

## Remote command installation

Do not add a separate remote release artifact. Extend the installed
`flint-remote-server` binary with a `flintctl` client mode. Dispatch this mode
from the executable file name so the same binary supports both command parsers.

After a remote server version is installed:

- On Unix, create a sibling link named `flintctl` to the versioned server.
- On Windows, create a sibling `flintctl.exe` launcher or copy.
- Write a release-channel- and version-scoped executable marker under Flint's
  managed remote data directory.
- Replace each supported Flint-managed remote agent instruction block with the
  instructions from this Flint version.

The command must not depend on `PATH`. Managed instructions read the marker
and run its exact path. A stable shell link can be a separate convenience
feature.

The parser and control protocol come from the same Flint installation. A new
remote server installation replaces its command link, marker, and instructions
as one versioned operation. New sessions must not use old command text.

## Remote terminal identity

The local terminal ID is an identifier, not a credential. It can appear in
command output and logs.

When `flint-remote-server` creates a PTY, it creates an opaque remote terminal
registration ID and records the PTY root process identity. The ID is unique
for the life of that server process. The server sends it to local Flint in the
remote terminal creation flow. Local Flint stores this caller data with the
local terminal record:

```text
RemoteTerminalCaller {
    remote_connection_id,
    remote_terminal_registration_id,
}
```

The record also contains the local terminal ID, the exact `Terminal` and
`TerminalView` registration generation, the owning workspace, and Agent Thread
state.

When remote `flintctl` connects, the server gets its peer process ID from the
operating system. It walks ancestry from that process to a registered PTY root.
It does not accept a process ID or registration ID from the client. If it finds
a match, it forwards the server-side registration ID. Local Flint accepts that
ID only on the remote connection that created it.

Registration can finish after the shell starts. During this race, the bridge
returns `not-ready`, and `flintctl` uses the current bounded retry behavior.

`terminal current` needs no caller-supplied terminal ID. `terminal list`
returns other live terminal IDs in the same workspace by default and includes
the caller when `--all` is present.

## Delegated Agent Thread caller fallback

Some agent tools run commands through a delegated process whose ancestry does
not reach the terminal root. The remote design must preserve the reason for
the current constrained local Agent Thread fallback.

The normal remote rule is process ancestry. If it fails, the remote server can
apply an Agent Thread-only fallback equivalent to the local behavior. It can
use operating-system facts that the server reads for the peer process, such as
its working directory and executable. It can match only live Agent Thread
registrations owned by that server and must reject an ambiguous match. It must
not make an ordinary remote terminal controllable by working directory alone.

Local Flint still verifies that the registration is live, belongs to the
forwarding connection, and has Agent Thread state before a `thread` command.

## Caller and workspace boundary

A verified caller authorizes only one live caller terminal and its local
workspace. It does not authorize a host, user account, repository, connection,
or all terminals owned by one server.

Local Flint rejects a remote request when:

- the forwarding connection did not register the caller ID;
- the caller terminal was released or its generation changed;
- the server restarted and lost the PTY registration;
- the target terminal belongs to another workspace;
- the target terminal ID is stale;
- the command is not supported by the protocol version.

Recreating a terminal creates new local and remote IDs. Releasing either side
removes the mapping. A later terminal must not reuse either ID during the
owning process lifetime.

The endpoint is user-scoped. Use mode `0600` for a Unix socket and a
current-user access rule for a Windows named pipe. The server must get the peer
process ID from the operating system before dispatch. This boundary does not
protect against a fully compromised process that runs as the same remote user.

## Endpoint discovery and multiple Flint instances

One remote host can have multiple server processes for different Flint
versions, release channels, local instances, or projects. A single well-known
endpoint cannot select one without a client identity claim.

Each server creates an instance-scoped endpoint and a bounded discovery record
in a release-channel- and version-scoped control directory. The record contains
only the endpoint name, server process identity, and protocol version. It
contains no credential.

Remote `flintctl` reads the matching directory and tries its bounded set of
live endpoints. Each server checks the peer process against only its own PTY
registrations. Exactly one server can claim a normal caller. The client accepts
the first verified response and does not fall back after a server claims the
caller. Servers remove their records during normal shutdown. Discovery ignores
records whose server process no longer exists.

Thus, operating-system ancestry selects the matching Flint instance without
an environment variable. A request must never run through another instance
only because its endpoint was tried first.

## Request routing

The client sends the current bounded, length-prefixed `ControlRequest` to each
candidate endpoint. It keeps the selected connection open until it receives
the response, which permits disconnect detection for `wait-output`.

After caller resolution, the server adds a transport-only envelope:

```text
RemoteControlEnvelope {
    remote_terminal_registration_id,
    control_request,
}
```

The authenticated remote connection supplies the remote connection identity.
Local Flint must not accept an envelope from a connection that did not create
the registration.

The server does not resolve local terminal IDs, read terminal content,
interpret operations, or write terminal input. Local Flint uses the current
request, response, dispatcher, byte limits, and protocol rules.

The response returns through the same path. Client disconnect, server
disconnect, project disconnect, Flint shutdown, and terminal release cancel an
active wait. The bridge enforces the current 1 MiB limits before allocation and
at each framing boundary.

## Terminal registration

Register local and remote terminals in one local registry. Both use the same
terminal ID, workspace check, metadata, snapshot, cursor, input, and wait
implementation. Only caller resolution differs.

```text
TerminalControlCaller
    Local { root_process_id }
    Remote { remote_connection_id, remote_terminal_registration_id }
```

The server owns its PTY process-to-registration map. Local Flint owns the
connection-and-registration-to-terminal map. Neither side infers a caller from
a host name, SSH destination, agent kind, repository, or client-supplied path.

## Direct and Tunneled Agent Thread routes

Remote control must not change agent launch or credential behavior.

- Direct uses only the configured ambient agent executable on the remote host.
- Tunneled uses only the pinned Flint-managed executable on the remote host and
  routes its network traffic through local Flint.
- Both routes use the same process-based bridge.
- The bridge does not expose managed binaries, credentials, or Tunneled proxy
  capabilities to a Direct session.

`thread create` preserves the workspace's route rules. Request data cannot
change a Direct caller to Tunneled or let a Tunneled caller bypass its pinned
executable and credential boundary.

## Ordinary remote terminals

Every supported remote PTY gets a registration. A process in an ordinary
remote terminal can use `status` and the `terminal` group through process
ancestry. Agent Thread operations keep their current checks:

- `thread retie` requires a registered Agent Thread caller.
- `thread create` requires a registered Agent Thread caller and enabled Agent
  Thread control.

## Command behavior

Remote commands use local semantics:

- `status --json` reports the local Flint version, protocol version, release
  channel, and supported capabilities.
- `terminal current` returns the caller's local terminal metadata.
- `terminal list` excludes the caller by default and includes it with `--all`.
- `terminal read` reads the local screen and scrollback, returns an opaque
  cursor, and preserves `--since` and `cursor-expired` behavior.
- `send-text`, `send-key`, and `run` use the current terminal input path.
- `wait-output` preserves source, line, timeout, cancellation, and matching
  behavior for the pinned terminal registration.
- `thread retie` and `thread create` update local Agent Thread state.

`terminal run` sends input followed by Enter as one non-interleavable control
operation. It does not start a separate shell process.

## Connection and lifecycle behavior

If no server can verify the caller, `flintctl` reports that the process is not
in a controllable Flint remote terminal. If a server verifies the caller but
has no local Flint connection, it reports that the matching session is
unavailable. It does not start Flint or another server.

A server disconnect invalidates all of its local caller mappings and cancels
active waits. A reconnect uses new connection identity. Existing terminals
must register again with fresh IDs or become explicitly unavailable. They must
not attach silently to a different Flint instance.

After an upgrade, existing terminals continue with their current server and
endpoint until they close, or control becomes explicitly unavailable. New
terminals use the new server, marker, and instructions. An upgrade must not
change a live terminal's route in place.

## Errors

Preserve current control errors, including `caller-not-recognized`,
`caller-not-agent-thread`, `terminal-not-found`,
`terminal-outside-workspace`, `terminal-exited`, `invalid-key`,
`invalid-pattern`, `invalid-request`, `cursor-expired`, `timeout`,
`response-too-large`, and `unsupported-protocol`.

Add remote transport errors only where current errors are not sufficient:

- `remote-control-unavailable` when the matching bridge has no local Flint
  connection;
- `remote-session-stale` when the server resolves a caller but local Flint no
  longer has its connection-bound registration;
- `remote-version-mismatch` when all three components cannot use one protocol
  version.

Failure to match the peer uses `not-ready` during the bounded registration
race. After retries, the CLI reports `caller-not-recognized`. Human output
explains the failure boundary. JSON output preserves the error code.

## Verification

Protocol and local control tests cover:

- remote envelope serialization and current byte limits;
- rejection when the forwarding connection does not own the registration;
- protocol negotiation, additive minor fields, and typed transport errors;
- exact `terminal current` resolution without a caller-supplied terminal ID;
- list exclusion and `--all` inclusion;
- same-workspace access and cross-workspace denial;
- invalidation on release, replacement, and disconnect;
- no local terminal ID or remote registration ID reuse;
- cursor reads and `cursor-expired` through the remote route;
- disconnect cancellation for output waits;
- unchanged local caller resolution.

Remote server tests cover:

- endpoint permissions and peer identity on Unix and Windows;
- ancestry resolution to the exact owned PTY;
- constrained Agent Thread fallback and ambiguous-match rejection;
- no working-directory fallback for ordinary terminals;
- concurrent instances, stale discovery records, and version handover;
- forwarding without interpreting terminal operations;
- current byte limits and cancellation in both directions.

End-to-end tests cover ordinary terminals and Agent Threads on Direct and
Tunneled routes. For each applicable route, verify status, current, list with
and without `--all`, snapshot and cursor reads, input, run, wait, retie, and
create with an optional name. Verify human and JSON output. Verify the ambient
Direct executable and pinned Tunneled executable boundaries.

Instruction and package tests verify the matching remote command mode,
executable marker, and all supported managed remote agent instruction blocks.

## Non-goals

This design does not:

- move terminal rendering or scrollback ownership to the remote host;
- expose a public TCP or HTTP control API;
- allow control across local workspaces or Flint instances;
- preserve terminal IDs or registrations across terminal recreation;
- use a terminal ID, environment variable, working directory, or client PID
  claim as a credential;
- change Direct or Tunneled executable and credential rules;
- add pane, split, focus, resize, or terminal creation commands;
- keep terminals alive after local Flint exits.

## Implementation order

1. Add connection-bound remote registration IDs to the local registry and add
   lifecycle and authorization tests.
2. Add remote PTY registration and peer-process ancestry resolution, including
   the constrained Agent Thread fallback.
3. Add instance endpoints, bounded discovery records, and concurrent-instance
   tests.
4. Add the remote envelope and bidirectional forwarding with current limits,
   protocol rules, and cancellation.
5. Add executable-name dispatch and remote discovery to the installed server.
6. Synchronize versioned managed instructions on the remote host.
7. Route verified requests through the existing local dispatcher.
8. Add Direct, Tunneled, ordinary-terminal, cursor, reconnect, upgrade, Unix,
   and Windows verification.
