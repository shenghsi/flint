# Agent-Initiated Worktree Control: Windows Support (plan only, not implemented)

## Context

`docs/superpowers/specs/2026-08-07-worktree-tied-agent-threads-panel-design.md`
("Stage 2") deliberately scoped agent-initiated worktree control
(`flint-agent-control retie-thread` / `create-thread`) to local Unix hosts.
The implementation later replaced per-thread bearer tokens with kernel-verified
peer identity (`LOCAL_PEERPID`/`SO_PEERCRED`, process-ancestry walking, and a
cwd/kind fallback for CLIs such as Codex that delegate shell execution to a
detached daemon). It also added an opt-in nudge that can append worktree discovery
instructions to a CLI's global instructions file
(`crates/agent_threads/src/instructions.rs`).

This document plans a later Windows implementation. It does not implement Windows
support. The earlier peer-credential work also exposed Windows `dead_code` failures:
the control-server task, live-terminal PID/worktree helpers, and their supporting
type were reachable only from `#[cfg(unix)]` call sites but were not all gated the
same way. Those items are now `#[cfg(unix)]`, keeping current Windows CI clean. The
Windows implementation must broaden those gates as part of enabling their Windows
callers; simply adding a transport is not sufficient.

## Current platform boundary

Already cross-platform:

- Worktree ties, persistence, retie, panel filtering, history attribution, and
  restore routing (`store.rs`, `panel.rs`, `history.rs`).
- The JSON request/response types in `agent_control_protocol`.
- `agent_control_cli`'s argument parsing and response rendering. Its actual I/O is
  still Unix-only, and its non-Unix `run()` returns an unsupported-platform error.
- Most of the caller resolver after a trustworthy peer PID is available. The
  algorithm uses `sysinfo`, but its Windows process and path behavior still needs
  native tests before it can be called unchanged.

Unix-only today:

- `mod control` and `mod instructions` in `agent_threads.rs`.
- `AgentThreadStore::_control_server_task`, `LiveTerminalWorktree`,
  `hold_control_server_task`, `live_terminal_pids`, and
  `live_terminal_worktree_roots` in `store.rs`.
- `init_control_server`'s real call to `control::init` and the instruction-nudge
  call in `spawn_thread_task_inner`.
- The control transport and peer-PID functions in `control.rs`.
- `agent_control_cli`'s `std::os::unix::net::UnixStream` client.
- `util::get_flint_agent_control_path`, including the function-level
  `#[cfg(unix)]`.
- Building, signing, and packaging `flint-agent-control.exe` in
  `script/bundle-windows.ps1` and `crates/flint/resources/windows/flint.iss`.

Windows already has AF_UNIX support in this repository:

- `crates/net/src/socket.rs`, `listener.rs`, and `stream.rs` implement Winsock
  `AF_UNIX`.
- `crates/net/src/async_net.rs` provides the async wrapper used by
  `net::async_net::{UnixListener, UnixStream}`.
- `cargo test -p net --lib` exercises both the synchronous and async paths on a
  Windows host.

That existing support answers only the transport question. It does not provide the
peer identity required by agent control.

## Chosen transport: Windows named pipes

Use Windows named pipes rather than AF_UNIX.

AF_UNIX is not suitable for this authorization model even though the repository's
transport works:

1. [Windows AF_UNIX does not expose credential ancillary data](https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/)
   equivalent to `LOCAL_PEERPID`/`SO_PEERCRED`. Without a kernel-reported peer PID,
   it cannot preserve the current no-client-secret authorization boundary.
2. AF_UNIX arrived in Windows build 17063, while Flint's installer currently allows
   build 16299 (`MinVersion=10.0.16299` in `flint.iss`). A compile-only check cannot
   establish runtime availability on every supported Windows version.
3. Named pipes support `GetNamedPipeClientProcessId` on older supported Windows
   versions and therefore provide the identity primitive this feature actually
   needs.

The JSON request and response types remain shared. The transport endpoint and
message framing become platform-specific.

## Endpoint naming and access control

`agent_control_protocol::socket_path()` is not a cross-platform endpoint once
Windows uses named pipes. Make the distinction explicit:

- Keep `socket_path() -> PathBuf` under `#[cfg(unix)]`.
- Add a Windows pipe-name function under `#[cfg(windows)]`, returning a name in the
  `\\.\pipe\...` namespace. Include the release channel and Windows logon session
  identity in the name so Stable/Nightly/Dev and simultaneous user sessions do not
  collide.
- Keep the existing per-channel executable-marker path on Unix. On Windows, add the
  same logon-session identity to the marker filename so each independently owned
  pipe has one independently owned marker. Derive the pipe name and marker path
  from one shared Windows control scope rather than computing their identities in
  separate functions.
- Keep endpoint derivation lightweight and identical in the server and CLI. A
  Windows-only dependency in `agent_control_protocol` is acceptable; it must not
  pull GPUI into `agent_control_cli`.
- Replace the CLI's transport-neutral assumption that every override is a
  filesystem `PathBuf`. Keep `--socket` for Unix tests and add a Windows `--pipe`
  override, or introduce one internal endpoint enum with platform-specific parsing.

Do not rely on
[`CreateNamedPipeW`'s default security descriptor](https://learn.microsoft.com/windows/win32/ipc/named-pipe-security-and-access-rights).
Create the pipe with a DACL granting the current logon SID the required read/write
access (and only the explicitly intended system principals), reject remote clients
with `PIPE_REJECT_REMOTE_CLIENTS`, and make handles non-inheritable. Use
`FILE_FLAG_FIRST_PIPE_INSTANCE` or an equivalent ownership check so a second Flint
does not silently join or steal an existing endpoint. A same-name live owner should
disable the new server instance with a clear log, matching the Unix behavior.

The executable-location marker remains a normal file under `paths::data_dir()`.
Its Windows filename must contain the same logon-session identity as the pipe name.
Write it only after the named-pipe server owns its endpoint, and remove it on clean
shutdown only if that server instance wrote that session-specific marker. This
preserves the Unix invariant that one endpoint owner owns one marker: two Flint
instances for the same Windows user in different logon sessions must never overwrite
or remove each other's marker.

## Server architecture and framing

Use the workspace-standard `windows` crate, whose configured features already
include `Win32_System_Pipes`, rather than introducing `windows-sys`. Reuse the
repository's named-pipe patterns in
`crates/flint/src/flint/windows_only_instance.rs` where helpful, but do not copy its
single small inbound-message assumptions: agent control is duplex and its JSON is
variable-sized.

The current Unix framing is request bytes followed by a write-half shutdown; the
server calls `read_to_end` and then writes the response. Named pipes have no
equivalent half-close, so define Windows framing explicitly:

- Create a duplex, message-mode pipe (`PIPE_TYPE_MESSAGE` and
  `PIPE_READMODE_MESSAGE`).
- Send one complete JSON request message and one complete JSON response message.
- Read `ERROR_MORE_DATA` chunks until the message is complete, with a fixed maximum
  request/response size. Reject an oversized or malformed request with a bounded
  error response rather than growing a `Vec` without limit.
- Disconnect and reuse or recreate the pipe instance after the response. The next
  accept must be ready even if the previous request failed to decode or dispatch.

Synchronous `ConnectNamedPipe`/`ReadFile`/`WriteFile` must never run on GPUI's
foreground executor. Use a dedicated Windows server thread with cancellable
overlapped I/O (or an equivalently cancellable background implementation):

1. The server thread accepts a connection, reads its bounded message, and obtains
   the client PID with `GetNamedPipeClientProcessId` from that connected pipe
   instance.
2. It sends `(peer_pid, request, response_sender)` through a channel consumed by a
   foreground GPUI task.
3. The foreground task calls the existing async `dispatch` path and returns the
   `ControlResponse` through the response sender.
4. The server thread writes the response and disconnects the instance.
5. App shutdown signals the server, cancels pending accept/read I/O, joins the
   thread, and then removes the marker it owns. Tests must be able to stop a server
   deterministically; process exit is not the cleanup mechanism.

Preserve the existing Unix server lifecycle: it remains one foreground
`Task<()>` held by `AgentThreadStore`, with the same cancellation and app-quit
behavior introduced in the Linux/macOS implementation. Generalize only the stored
type, for example with a cfg-specific `ControlServerHandle` whose Unix definition
wraps that existing task unchanged and whose Windows definition contains the
foreground dispatcher task plus its shutdown and thread-join state. Do not move the
Unix accept loop to a thread or change its transport lifecycle as part of the
Windows pass. Keep the field and setter gated consistently so neither platform
leaves control-server-only state dead.

## Peer identification and caller resolution

Call
[`GetNamedPipeClientProcessId`](https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-getnamedpipeclientprocessid)
only after `ConnectNamedPipe` has established the specific pipe instance. Treat
every API failure as an authorization failure and return an error response; never
fall back to a PID supplied in JSON.

After obtaining the PID, reuse the ancestry-first and cwd/kind-fallback policy from
`resolve_caller_thread`, subject to Windows validation:

- Confirm `sysinfo` returns the expected parent chain, process names, executable,
  and cwd for a real Windows child process on both x86_64 and aarch64 CI where
  available.
- Normalize `.exe` suffixes when comparing process names with agent kind IDs.
- Make cwd/root containment honor Windows path semantics, including
  case-insensitive components, drive-letter case, mixed separators, and canonical
  paths. Do not use raw `Path::starts_with` as the only Windows containment check.
- Preserve ancestry as the authoritative signal and continue rejecting ambiguous
  cwd/kind matches.
- Continue excluding remote threads from the local PID/worktree candidate sets.

Keep the authorization-independent command handlers shared. Only endpoint
ownership, framing, and peer-PID acquisition should differ by transport.

## Platform gates and integration points

Enabling Windows requires updating every existing Unix-only boundary:

- Compile `control` and `instructions` on `cfg(any(unix, windows))`, with
  transport-specific imports and functions gated inside them. Splitting the Windows
  transport into a descriptive sibling source file is acceptable if it keeps Win32
  handle and unsafe code out of the shared resolver/dispatch logic.
- Make `init_control_server` call `control::init` on Windows.
- Broaden the store's control-server handle and caller-candidate helpers to Windows.
- Offer the worktree-instructions nudge for local Windows threads after a supported
  Windows instruction block exists.
- Remove or broaden `util::get_flint_agent_control_path`'s function-level Unix gate
  before adding its Windows branch.
- Replace `agent_control_cli`'s non-Unix stub with the Windows client while retaining
  a stub for targets supporting neither implementation.
- Update Unix-specific module docs and CLI help so they describe the selected
  platform transport accurately.

Add a Windows startup test or another focused integration check proving that the
real Windows gate calls the server initializer and publishes a usable executable
marker. Unit tests of an otherwise unreachable Windows module are not sufficient.

## Instructions text and per-agent capability

The current `find ... -exec cat ... \;` discovery command is Unix-shell-specific.
Do not replace it with one Windows block shared speculatively by every agent.

Before enabling the Windows nudge, verify both the global-instructions path and the
actual Windows tool-execution shell for every supported kind:

- Codex (`~/.codex/AGENTS.md`)
- Claude Code (`~/.claude/CLAUDE.md`)
- OpenCode (`~/.config/opencode/AGENTS.md`)
- Pi (`~/.pi/agent/AGENTS.md`)

Represent the result as an explicit per-kind/platform instruction capability. If a
kind's Windows shell or global instructions convention is unverified, do not offer
to write a block for that kind.

For PowerShell-backed agents, the block should discover
the marker for the current PowerShell process's session under
`%LOCALAPPDATA%\Flint` rather than reading markers belonging to every session. For
example, derive the session with `(Get-Process -Id $PID).SessionId`, match only
`agent-control-*-$sessionId-executable.json`, and invoke the discovered quoted
executable with the call operator:

    & "<executable>" retie-thread --worktree "<path>"

Provide different text for `cmd.exe` or a POSIX-compatible shell when an agent uses
one. Preserve `DETECTION_MARKER` compatibility so an existing manually added block
is not duplicated. Add exact-content tests for each enabled platform/kind pair and
tests that unsupported pairs do not receive a nudge.

## Executable delivery

Place `flint-agent-control.exe` beside `Flint.exe` in installed and development
layouts. Then make every delivery step explicit:

- Broaden `util::get_flint_agent_control_path` to Windows and search
  `./flint-agent-control.exe` relative to the running Flint executable.
- Add `agent_control_cli` to the Windows release `cargo build` invocation.
- Copy `flint-agent-control.exe` to the Windows staging root.
- Add an explicit `[Files]` entry to
  `crates/flint/resources/windows/flint.iss`; that manifest does not wildcard other
  executables in the staging root.
- Add the executable to `SignFlintAndItsFriends`.
- Add `flint-agent-control.pdb` to `ZipFlintAndItsFriendsDebug`.
- Verify the installed executable location is the same path written to the marker,
  and that the signed installer contains both the executable and its expected
  version metadata.

## Testing and verification

Keep the shared dispatch tests, then add Windows-native coverage for the pieces Unix
tests cannot validate:

- Pipe-name and executable-marker derivation use the same release/session scope,
  are isolated across logon sessions, and are overridable in tests.
- Starting and stopping one of two simulated logon-session servers neither
  overwrites nor removes the other session's marker.
- The pipe DACL rejects a client outside the allowed logon session, and remote
  clients are rejected.
- A real named-pipe connection reports the actual client PID.
- A real Windows child process resolves through tracked ancestry.
- The cwd fallback handles differently cased drive letters/components and mixed
  separators, disambiguates by normalized process kind, and rejects ambiguous
  matches.
- Message-mode request/response round trips cover payloads larger than the first
  read buffer, malformed JSON, oversize rejection, retry after a failed request,
  and deterministic shutdown while accept/read is pending.
- The Windows CLI preserves the Unix client's retry/backoff and exit-code behavior.
- Toggling `agent_threads.agent_control` affects already-running Windows threads.
- Windows startup publishes the marker; the test discovery layer reads it, launches
  the executable path recorded in it, and verifies that executable reaches the
  running server through its independently derived pipe name.
- Instruction paths, exact blocks, idempotent append behavior, and unsupported-agent
  gating are covered for every enabled Windows agent.

Treat `clippy_windows` as a required gate. Also run the Windows `agent_threads`,
`agent_control_cli`, `agent_control_protocol`, and `net` tests, build the Windows
installer, inspect its file list, and smoke-test the installed executable on the
oldest Windows build Flint still supports. A cross-compile alone is not runtime
verification.

## Explicit non-goals

- Remote-SSH agent-control transport on either platform. Direct and tunneled remote
  agent routes remain outside this local control surface.
- Changing the worktree-tie model, retie persistence, history attribution, or
  command semantics.
- Reintroducing client-presented bearer tokens or trusting a PID supplied by the
  client.
- Implementing any of the Windows work in this documentation-only pass.

## Implementation order

1. Add shared Windows release/session scoping for pipe names and executable-marker
   paths, plus security-descriptor helpers and focused ownership tests.
2. Implement the cancellable named-pipe transport, message framing, and peer-PID
   acquisition behind injected test endpoints.
3. Make caller resolution Windows-correct and add native process/path tests.
4. Broaden all module, store, startup, utility, and nudge gates; add the startup
   integration check.
5. Implement the Windows CLI transport and end-to-end request tests.
6. Verify all four agents' Windows instruction conventions and enable only the
   confirmed per-kind blocks.
7. Add build, installer-manifest, signing, debug-symbol, and installed-bundle
   verification.
8. Run Windows clippy/tests and the minimum-supported-Windows smoke test before
   declaring the platform supported.
