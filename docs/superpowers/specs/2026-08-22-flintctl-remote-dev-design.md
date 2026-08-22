# `flintctl` for Remote Development

## Status

This document is a design proposal. No implementation work has started.

## Goal

Let a process that runs in a Flint remote-development terminal use the same
`flintctl` commands as a process in a local terminal. Keep terminal state and
terminal control in the local Flint application. Use the existing connection
to `flint-remote-server` as the bridge. Do not open a public network port.

The remote command surface stays the same:

```text
flintctl status [--json]

flintctl thread retie --worktree <path>
flintctl thread create --worktree <current|new> --agent <agent> --prompt <prompt>

flintctl terminal current
flintctl terminal list
flintctl terminal read <terminal-id> [--source visible|recent|recent-unwrapped] [--lines <count>]
flintctl terminal send-text <terminal-id> <text>
flintctl terminal send-key <terminal-id> <key>...
flintctl terminal run <terminal-id> <command>
flintctl terminal wait-output <terminal-id> (--match <text>|--regex <pattern>) [--timeout <duration>]
```

## Current state

Flint displays a remote terminal through a local `Terminal` entity. The local
PTY runs an SSH, WSL, or container client. The shell and coding agent run on
the remote host. The local `Terminal` still owns the emulated screen,
scrollback, input path, and Flint terminal ID.

The first `flintctl` version rejects these terminals. Its local control server
gets the client process ID from a Unix socket or Windows named pipe and walks
local process ancestry. A `flintctl` process on the remote host has no local
process ancestry, cannot open the local control endpoint, and cannot prove
which local terminal displays its session.

The remote host already runs the matching `flint-remote-server` binary. Flint
updates that binary for the remote target when the local version changes.

## Design summary

Use `flint-remote-server` as a remote `flintctl` client bridge. Install a
sibling command named `flintctl` that starts the same binary in client mode.
The client connects to a user-scoped control endpoint on the remote host. The
remote server forwards the request through its existing connection to local
Flint. Local Flint validates a terminal-scoped capability and performs the
operation on the local terminal model.

```text
Remote shell or agent
        |
        | flintctl request + inherited capability
        v
Remote user-scoped control endpoint
        |
        v
flint-remote-server
        |
        | existing authenticated Flint connection
        v
Local Flint control dispatcher
        |
        | capability -> caller terminal and workspace
        v
Local terminal registry and terminal model
```

No terminal contents move to the remote server for control. They already move
from the remote shell to the local terminal through the terminal connection.
Reads use the local terminal model. Input uses the existing local PTY input
path.

## Remote command installation

Do not add a separate remote release artifact. Extend the installed
`flint-remote-server` binary with a `flintctl` client mode.

After the remote server version is installed:

- On Unix, create a sibling link named `flintctl` to the versioned remote
  server binary.
- On Windows, create a sibling `flintctl.exe` launcher or copy that starts the
  same binary in client mode.
- Write a release-channel- and version-scoped remote executable marker.
- Replace the Flint-managed instruction block in each supported remote agent
  instruction file with the instructions from the current Flint version.

The command must not depend on `PATH`. Managed instructions discover the
executable through the marker. A human can install a stable shell link as a
separate convenience feature, but this design does not require it.

The remote command parser and the local protocol version come from the same
Flint installation. A newly installed and launched Flint version replaces the
remote command link, marker, and managed instructions. Old commands and old
instructions must not remain active for a new remote session.

## Remote terminal identity

The local terminal ID is an identifier, not a credential. It can appear in
command output and logs. Remote caller authorization uses a separate random
capability.

Before Flint starts a remote terminal, it creates a remote control session:

```text
RemoteTerminalControlSession {
    capability: 256 random bits,
    release_channel,
    flint_instance,
}
```

Flint injects the capability into the remote terminal environment. It must not
put the capability in command-line arguments, terminal titles, logs, or
terminal output. Child processes, including coding agents, inherit it.

The local `Terminal` stores the same capability until it is released. When its
`TerminalView` registers with the terminal control registry, the registry maps
the capability to:

- the current local terminal ID;
- the exact terminal entity and registration generation;
- the owning local workspace;
- whether the terminal is an Agent Thread;
- the active Flint instance and release channel.

Registration can finish after the remote shell starts. During this race,
remote requests return `not-ready`, and `flintctl` uses the same bounded retry
behavior as local Agent Thread requests.

`flintctl terminal current` does not require the agent to know its local
terminal ID. The inherited capability resolves the caller, and Flint returns
the ID. `terminal list` returns other live terminal IDs in the same local
workspace. Commands that target another terminal use an ID from that list.

## Capability boundary

A capability authorizes only one live caller terminal and its local workspace.
It does not authorize a host, user account, repository, or all remote sessions.

Local Flint must reject a remote request when:

- the capability is missing, malformed, unknown, or expired;
- the capability belongs to another Flint instance or release channel;
- the caller terminal was released or replaced;
- the target terminal belongs to another workspace;
- the target ID is stale;
- the command is not supported by the negotiated protocol version.

Reconnecting or recreating a terminal creates a new terminal ID and a new
capability. Releasing the terminal removes the capability mapping. A later
terminal must never reuse either value.

The remote control endpoint is user-scoped. Use mode `0600` for a Unix socket
and a current-user access rule for a Windows named pipe. This endpoint limits
which remote user can submit a request. The capability limits which live
terminal and workspace that request can control.

The capability protects the Flint boundary from unrelated remote sessions. It
does not protect against a fully compromised process running as the same
remote operating-system user, which can already inspect or control that
user's processes on many supported systems.

## Request routing

The remote `flintctl` client reads its terminal capability from the inherited
environment and connects to the remote control endpoint. It sends the current
bounded, length-prefixed request plus a remote caller envelope. It keeps the
connection open for commands such as `terminal wait-output`.

`flint-remote-server` forwards the request through the existing bidirectional
protocol connection. It does not resolve terminal IDs, read terminal content,
or write terminal input. Local Flint performs all authorization and command
dispatch.

The response returns through the same path. Client disconnect, remote-server
disconnect, project disconnect, local Flint shutdown, and terminal release
cancel an active wait.

The remote bridge must apply the same request and response byte limits as the
local transport. It must not accept an unbounded message before forwarding.

## Terminal registration

Register local and remote terminals in one local registry. A local record uses
local process ancestry for caller resolution. A remote record uses its remote
capability. Both record types use the same terminal ID, workspace check,
metadata, snapshot, input, and wait implementation.

```text
TerminalControlCaller
    Local {
        root_process_id,
    }
    Remote {
        capability,
        remote_connection_id,
    }
```

Do not infer a remote caller from its working directory, remote host, agent
kind, or SSH destination. Several terminals can share all of those values.

## Direct and Tunneled Agent Thread routes

Remote `flintctl` support must not change agent launch or credential behavior.

- Direct continues to run only the configured ambient agent executable on the
  remote host.
- Tunneled continues to run only the pinned Flint-managed agent executable on
  the remote host and routes its network traffic through local Flint.
- Both routes inherit the terminal capability and use the same remote
  `flintctl` bridge.
- The bridge must not expose Flint-managed agent binaries, credentials, or
  Tunneled proxy capabilities to a Direct session.

`thread create` must preserve the workspace's current route rules. A Direct
caller cannot request a Tunneled launch by changing request data, and a
Tunneled caller cannot bypass its pinned executable or credential boundary.

## Ordinary remote terminals

The feature is not limited to Agent Threads. An ordinary remote terminal gets
a capability when Flint creates it. A process in that terminal can use
`status` and the `terminal` command group. Agent Thread-only operations keep
their existing checks:

- `thread retie` requires a registered Agent Thread caller.
- `thread create` requires a registered Agent Thread caller and enabled Agent
  Thread control.

## Command behavior

Remote commands use the local command semantics without a second
implementation:

- `terminal current` resolves the capability to the local terminal ID.
- `terminal list` lists only the caller's local Flint workspace.
- `terminal read` reads the local emulated screen and scrollback.
- `send-text`, `send-key`, and `run` use the local terminal input path.
- `wait-output` observes the pinned local terminal registration.
- `thread retie` and `thread create` update local Flint Agent Thread state.

A remote command does not start a shell command outside the displayed
terminal. `terminal run` still means terminal input followed by Enter.

## Connection and lifecycle behavior

If the remote server is not connected to local Flint, remote `flintctl`
reports that no matching Flint session is available. It must not start Flint,
start another remote server, or search other users' sessions.

If several local Flint applications connect to the same remote host, each one
uses an instance-scoped remote endpoint or routing record. The inherited
capability selects the correct Flint instance. A request must never fall back
to another live instance.

When local Flint upgrades its remote server, it must not change an already
running terminal's control route in place. Existing terminals either continue
with their matching server until they close or become explicitly unavailable.
New terminals use the new server, command marker, and instructions. The
implementation must define and test this handover before allowing two server
versions to share an endpoint.

## Errors

Add remote-specific machine-readable errors where the current errors are not
sufficient:

- `remote-control-unavailable` when the remote bridge is not connected;
- `remote-caller-unrecognized` when the capability cannot resolve a live
  terminal;
- `remote-session-stale` when the capability belongs to an ended or replaced
  terminal;
- `remote-version-mismatch` when the remote client and local Flint cannot use
  one protocol version.

Keep current target errors such as `terminal-not-found`, `terminal-exited`,
and `terminal-outside-workspace` after caller resolution succeeds.

Human output must explain whether the failure is discovery, connection,
caller identity, workspace authorization, or target lifecycle. JSON output
must preserve the error code.

## Verification

Protocol tests cover:

- remote caller-envelope serialization and byte limits;
- capability omission, malformed values, and unknown fields;
- version negotiation and typed remote errors;
- disconnect and cancellation propagation for output waits.

Local control tests cover:

- one remote capability resolving to its exact local terminal ID;
- `terminal current` without a caller-supplied terminal ID;
- same-workspace listing and access for local and remote targets;
- denial for another workspace, Flint instance, and release channel;
- immediate invalidation on terminal release or replacement;
- no capability or terminal ID reuse;
- unchanged local process-ancestry authorization.

Remote-server tests cover:

- user-scoped endpoint permissions on Unix and Windows;
- forwarding without inspecting or changing terminal operations;
- bounded messages in both directions;
- concurrent local Flint instances on one remote host;
- remote-server restart and version handover;
- cancellation when either side disconnects.

End-to-end remote tests cover ordinary terminals and Agent Threads on Direct
and Tunneled routes. For each route, verify `status`, `terminal current`,
list/read/input/run/wait, `thread retie`, and `thread create`. Verify that
Direct still uses the ambient agent executable and Tunneled still uses the
pinned managed executable.

Instruction and package tests verify that a new Flint version installs the
matching remote command mode, rewrites the remote executable marker, and
replaces every Flint-managed remote agent instruction block with the latest
commands.

## Non-goals

This design does not:

- move terminal rendering or scrollback ownership to the remote host;
- expose a public TCP or HTTP control API;
- allow control across local Flint workspaces or Flint instances;
- preserve terminal IDs or capabilities across terminal recreation;
- use a terminal ID as a credential;
- infer caller identity from a path, host, agent kind, or repository;
- change Direct or Tunneled agent executable and credential rules;
- add pane, split, focus, resize, or terminal creation commands;
- keep terminals alive after local Flint exits.

## Implementation order

1. Add the remote caller envelope, capability type, registry record variant,
   lifecycle invalidation, and local authorization tests.
2. Create a remote control session before remote terminal launch, inject the
   capability, and register the resulting local terminal entity.
3. Add the remote-server control endpoint and bidirectional request forwarding
   with limits, cancellation, and instance scoping.
4. Add `flintctl` client mode and remote command discovery to the installed
   remote-server binary.
5. Synchronize versioned Flint-managed instructions on the remote host.
6. Enable the existing command dispatcher for remote callers.
7. Add Direct, Tunneled, ordinary-terminal, reconnect, upgrade, Unix, and
   Windows verification.

