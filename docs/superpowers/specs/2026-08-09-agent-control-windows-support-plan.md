# Agent-Initiated Worktree Control: Windows Support

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

This document specifies the Windows implementation. The earlier peer-credential
work also exposed Windows `dead_code` failures:
the control-server task, live-terminal PID/worktree helpers, and their supporting
type were reachable only from `#[cfg(unix)]` call sites but were not all gated the
same way. Those items are now `#[cfg(unix)]`, keeping current Windows CI clean. The
Windows implementation must broaden those gates as part of enabling their Windows
callers; simply adding a transport is not sufficient.

The behavioral baseline is commit
`e1b1db2271c1122d1247837a668847be8a7faa65`. Windows support extends that
implementation rather than defining a second control model. In particular, it must
preserve the existing request and response JSON, server-side caller resolution,
ancestry-before-cwd precedence, ambiguity rejection, local-thread boundary, live
`agent_threads.agent_control` setting check, retie/create handlers, and executable
marker discovery. Platform-specific code supplies only endpoint ownership, framing,
peer-PID acquisition, and the Windows validation needed to make the same resolver
safe. Existing Unix behavior and lifecycle are regression constraints.

## Implementation status

The branch implementation now covers the endpoint protocol, named-pipe client and
server, caller resolution, capability-gated instructions, helper delivery, updater
rollback, and installer metadata described below. Windows-native tests and the
repository clippy gate pass. Do not declare the platform supported until the
remaining release-acceptance work is recorded in the PR: build and inspect a
signed installer, exercise the DACL with genuinely separate logon identities, and
smoke-test the installed helper on the pinned Windows 10 1903 VM. Those checks
require release signing credentials and managed Windows identities/VMs; unit tests
or a developer-machine build do not substitute for them.

## Baseline platform boundary

Already cross-platform:

- Worktree ties, persistence, retie, panel filtering, history attribution, and
  restore routing (`store.rs`, `panel.rs`, `history.rs`).
- The JSON request/response types in `agent_control_protocol`.
- `agent_control_cli`'s argument parsing and response rendering. Its actual I/O is
  still Unix-only, and its non-Unix `run()` returns an unsupported-platform error.
- Most of the caller resolver after a trustworthy peer PID is available. The
  algorithm uses `sysinfo`, but its Windows process and path behavior still needs
  native tests before it can be called unchanged.

Unix-only at the baseline commit:

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
2. Named pipes provide `GetNamedPipeClientProcessId`, the identity primitive this
   feature needs.

Flint already uses `net::async_net::UnixListener` for askpass on Windows, so this
choice does not imply that AF_UNIX is generally unavailable or unsupported there.
It is specifically unsuitable for agent control because it cannot report the peer
PID required by this authorization model.

The JSON request and response types remain shared. The transport endpoint and
message framing become platform-specific.

## Endpoint naming and access control

`agent_control_protocol::socket_path()` is not a cross-platform endpoint once
Windows uses named pipes. Make the distinction explicit:

- Keep `socket_path() -> PathBuf` under `#[cfg(unix)]`.
- Add a Windows pipe-name function under `#[cfg(windows)]`, returning a name in the
  `\\.\pipe\...` namespace. Include the release channel and Terminal Services
  session ID in the name so Stable/Nightly/Dev and simultaneous user sessions do
  not collide.
- Keep the existing per-channel executable-marker path on Unix. On Windows, add the
  same Terminal Services session ID to the marker filename so each independently
  owned pipe has one independently owned marker. Derive the pipe name and marker
  path from one shared Windows control scope rather than computing their identities
  in separate functions.
- Keep endpoint derivation lightweight and identical in the server and CLI. A
  Windows-only dependency in `agent_control_protocol` is acceptable; it must not
  pull GPUI into `agent_control_cli`.
- Replace the CLI's transport-neutral assumption that every override is a
  filesystem `PathBuf`. Keep `--socket` for Unix tests and add a Windows `--pipe`
  override, or introduce one internal endpoint enum with platform-specific parsing.

Do not rely on
[`CreateNamedPipeW`'s default security descriptor](https://learn.microsoft.com/windows/win32/ipc/named-pipe-security-and-access-rights).
The Terminal Services session ID is only a naming/isolation value; it is not a
securable principal. Independently obtain the current logon SID (`S-1-5-5-X-Y`)
from Flint's process token and use it as the client principal in the pipe DACL.
Unlike a user SID, the logon SID scopes access to that logon session, including for
the same account connected through another Terminal Services session. Grant the
individual client read/write rights required by the protocol without granting
`FILE_CREATE_PIPE_INSTANCE`, and include only explicitly intended system
principals. Reject remote clients with `PIPE_REJECT_REMOTE_CLIENTS` and make handles
non-inheritable.

Create the initial owning instance with `FILE_FLAG_FIRST_PIPE_INSTANCE` before
creating the rest of the pool without that flag. This prevents a second Flint from
silently joining or stealing an existing endpoint while still allowing the owner
to create concurrent instances. A same-name live owner should disable the new
server instance with a clear log, matching the Unix behavior.

Keep that initial handle open as one persistent serving slot for the server's entire
lifetime. Every pool slot disconnects and reconnects the same handle rather than
closing and recreating instances, so the pool never passes through a state with no
open owner handle. A separate non-serving anchor is unsuitable because Windows may
select that available instance for a client connection and strand the client with no
worker. Remove the owned marker before signalling the workers to stop, while the
first-instance handle is still open; this prevents a new owner from publishing a
marker in the gap between endpoint release and the old owner's removal. Tests should
force every serving slot to recycle concurrently and prove that a second server still
cannot acquire the endpoint.

The executable-location marker remains a normal file under `paths::data_dir()`.
Its Windows filename must contain the same Terminal Services session ID as the pipe
name.
Write it only after the named-pipe server owns its endpoint, and remove it on clean
shutdown only if that server instance wrote that session-specific marker. This
preserves the Unix invariant that one endpoint owner owns one marker: two Flint
instances for the same Windows user in different Terminal Services sessions must
never overwrite or remove each other's marker.

Accept stale markers for ended sessions rather than adding cross-session cleanup.
They contain no secret, discovery reads only the current session's marker, and a
CLI using a stale current-session marker fails to connect and exits nonzero. When a
new server later acquires the pipe for that session, it atomically replaces the
marker with its own executable location. Markers for other ended sessions may
remain as harmless files under the data directory.

## Server architecture and framing

Use the workspace-standard `windows` crate, whose configured features already
include `Win32_System_Pipes` and `Win32_Security`, rather than introducing
`windows-sys`. Add the currently missing `Win32_Security_Authorization` feature
for constructing the explicit pipe DACL. Reuse the repository's named-pipe
patterns in `crates/flint/src/flint/windows_only_instance.rs` where helpful, but do
not copy its single small inbound-message assumptions: agent control is duplex and
its JSON is variable-sized.

Add narrowly featured, Windows-only `windows` dependencies to
`agent_control_protocol` for session-scoped endpoint derivation and to
`agent_control_cli` for the named-pipe client. Both crates must remain independent
of GPUI and the terminal stack; the platform dependency does not change the small,
standalone nature of the CLI binary.

The current Unix framing is request bytes followed by a write-half shutdown; the
server calls `read_to_end` and then writes the response. Named pipes have no
equivalent half-close, so define Windows framing explicitly:

- Create a duplex, message-mode pipe (`PIPE_TYPE_MESSAGE` and
  `PIPE_READMODE_MESSAGE`).
- Send one complete JSON request message and one complete JSON response message.
- Read `ERROR_MORE_DATA` chunks until the message is complete, with a fixed maximum
  request/response size. Reject an oversized or malformed request with a bounded
  error response rather than growing a `Vec` without limit.
- When all server instances are occupied, make the Windows client wait for an
  available instance only up to a fixed timeout and report a clear busy error. Put
  bounded timeouts around client reads and writes as well so pool exhaustion or a
  failed server worker cannot hang the CLI indefinitely.
- Implement those read/write deadlines with overlapped operations and
  `CancelIoEx`; `WaitNamedPipeW` bounds only the wait for an available instance and
  does not bound a subsequent synchronous `ReadFile` or `WriteFile`. After a
  cancellation, wait for the overlapped operation to reach a terminal state before
  closing or reusing its handle.
- Disconnect and reuse or recreate the pipe instance after the response. The next
  accept must be ready even if the previous request failed to decode or dispatch.

Synchronous `ConnectNamedPipe`/`ReadFile`/`WriteFile` must never run on GPUI's
foreground executor. Use a bounded pool of concurrent pipe instances, each driven
by cancellable overlapped I/O on dedicated Windows server threads (or an
equivalently cancellable background implementation). The pool must contain more
than one instance so a slow `create-thread` request cannot block all other clients,
and its size must be an explicit constant that tests can override.

1. Each available instance accepts independently, reads its bounded message, and
   obtains the client PID with `GetNamedPipeClientProcessId` from that connected
   pipe instance.
2. It sends `(peer_pid, request, response_sender)` through a channel consumed by a
   foreground GPUI dispatcher. The dispatcher spawns each request independently so
   one long-running command does not serialize other connected instances.
3. The foreground request task calls the existing async `dispatch` path and returns
   the `ControlResponse` through the response sender.
4. The owning server worker writes the response, disconnects the instance, and
   immediately recreates or reuses that pool slot for another accept, including
   after decode or dispatch failure.
5. Waiting for the foreground response is itself bounded and observes shutdown. If
   the dispatcher task exits, the response sender is dropped, dispatch exceeds its
   deadline, or shutdown begins, the worker returns a bounded error when possible
   and recycles or closes the instance instead of waiting forever.
6. App shutdown first removes the marker it owns while the first-instance serving
   handle still owns the endpoint, then signals every worker, cancels pending
   accept/read/write I/O, and joins all server threads off the GPUI foreground
   thread. Tests must be able to stop the pool deterministically; process exit is
   not the cleanup mechanism.

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
- Validate every ancestry hop with process creation times from
  [`GetProcessTimes`](https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes):
  the child must not predate its reported parent. Reject the hop if either process
  cannot be opened for query, either creation time cannot be read, or the ordering
  is invalid. Do not use `sysinfo::Process::start_time()` for this authorization
  check because its one-second granularity cannot distinguish PID reuse within the
  same second.
- Normalize `.exe` suffixes when comparing process names with agent kind IDs.
- Make cwd/root containment honor Windows path semantics, including
  case-insensitive components, drive-letter case, mixed separators, and canonical
  paths. Do not use raw `Path::starts_with` as the only Windows containment check.
  Canonicalize both paths before comparison and fail authorization closed if either
  path cannot be canonicalized; a lexical fallback can accept aliases, junctions,
  or traversal components that do not identify the tracked worktree.
- Preserve ancestry as the authoritative signal and continue rejecting ambiguous
  cwd/kind matches.
- Continue excluding remote threads from the local PID/worktree candidate sets.

Windows does not reparent orphaned processes, and its reported parent PID may have
been recycled after the original parent exited. The creation-time validation above
is therefore part of authorization, not only a correctness check. Add a native test
whose reported stale/recycled parent PID now belongs to a tracked terminal and
verify that the creation-time ordering prevents the false match.

Also validate the cwd fallback before treating any delegating-daemon CLI as
supported. For those CLIs, ancestry never reaches the tracked terminal and cwd/kind
matching is the only authorization path. Native x86_64 and aarch64 tests must prove
that the chosen process inspection can read the real daemon child's cwd across the
architectures Flint supports, including relevant WOW64 combinations. Record this
as a per-kind Windows authorization capability alongside the instruction
capability. If cwd inspection is unavailable or unreliable for a kind, do not offer
the Windows instruction nudge for it and show the unsupported authorization reason
in the Settings UI rather than allowing calls to fail later as unexplained
`NotReady` responses.

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
  platform transport accurately. In particular, update `control.rs`'s Unix-only
  module doc and `write_executable_location`'s comment that names `--socket` as the
  only discovery override.

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

The current fail-closed capability decision is:

- **Codex: enabled.** Official OpenAI documentation confirms native PowerShell
  execution on Windows and the global `CODEX_HOME/AGENTS.md` convention. Native
  Windows process/path tests cover the cwd inspection used by its fallback.
- **Claude Code: disabled.** Its user instructions path is known, but Anthropic's
  native Windows setup uses Git Bash rather than the PowerShell block below; its
  native cwd authorization path still needs a kind-specific test.
- **OpenCode: disabled.** Its global instructions path is known, but its Windows
  documentation recommends WSL and its native shell/cwd authorization path is not
  established for this local Windows transport.
- **Pi: disabled.** Its global instructions path is known, but its `shellPath` is
  configurable on Windows, so one unconditional shell block would be unsafe and
  its cwd authorization path remains unverified.

Keep an explicit unsupported reason for each disabled kind and surface the current
limitations with the Agent Control setting. Enabling another kind requires both an
exact shell-specific block test and a native cwd/ancestry authorization test; a
known global instructions path alone is insufficient.

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
- Update `crates/auto_update_helper`'s explicit job list to move the old
  `flint-agent-control.exe` into `old\`, move the replacement from `install\`
  into the application root, and restore the old executable during rollback if a
  later update job fails. Add apply and rollback tests covering the new file.
- Verify the installed executable location is the same path written to the marker,
  and that the signed installer contains both the executable and its expected
  version metadata.

## Testing and verification

Keep the shared dispatch tests, then add Windows-native coverage for the pieces Unix
tests cannot validate:

- Pipe-name and executable-marker derivation use the same release/session scope,
  are isolated across Terminal Services sessions, and are overridable in tests.
- Starting and stopping one of two simulated Terminal Services session servers
  neither overwrites nor removes the other session's marker.
- The pipe DACL rejects a client running as another user and the same user from a
  different logon session, and remote clients are rejected. Separate tests prove
  that the numeric Terminal Services session ID isolates names and markers while
  the logon SID is the securable DACL principal.
- A real named-pipe connection reports the actual client PID.
- A real Windows child process resolves through tracked ancestry.
- An ancestry hop rejects a recycled parent PID whose current process has a later
  creation time than the child.
- The cwd fallback handles differently cased drive letters/components and mixed
  separators, disambiguates by normalized process kind, and rejects ambiguous
  matches.
- Message-mode request/response round trips cover payloads larger than the first
  read buffer, malformed JSON, oversize rejection, retry after a failed request,
  concurrent requests while one dispatch is slow, pool exhaustion behavior, and
  deterministic shutdown while accept/read/write or foreground dispatch is
  pending. Force all serving slots to recycle at once and verify the persistent
  first-instance serving handle prevents a second server from acquiring the name.
- The Windows CLI preserves the Unix client's retry/backoff and exit-code behavior.
- Toggling `agent_threads.agent_control` affects already-running Windows threads.
- Windows startup publishes the marker; the test discovery layer reads it, launches
  the executable path recorded in it, and verifies that executable reaches the
  running server through its independently derived pipe name.
- Instruction paths, exact blocks, idempotent append behavior, and unsupported-agent
  gating are covered for every enabled Windows agent.

Treat `clippy_windows` as a required gate. Also run the Windows `agent_threads`,
`agent_control_cli`, `agent_control_protocol`, and `net` tests, build the Windows
installer, and inspect its file list. Resolve the repository's existing minimum-OS
mismatch as part of this work: `docs/src/installation.md` declares Windows 10 1903
and later supported, while `flint.iss` still permits 1709. Raise the installer
`MinVersion` to Windows 10 1903 (`10.0.18362`) so the documented and enforced floors
agree. Before merging, smoke-test the installed executable on a locally managed,
pinned Windows 10 1903 VM and record the OS build, installer artifact, and commands
and results in the PR. Hosted CI and cross-compilation do not replace that runtime
acceptance check.

## Explicit non-goals

- Remote-SSH agent-control transport on either platform. Direct and tunneled remote
  agent routes remain outside this local control surface.
- Changing the worktree-tie model, retie persistence, history attribution, or
  command semantics.
- Reintroducing client-presented bearer tokens or trusting a PID supplied by the
  client.

## Implementation order

1. Add shared Windows release/session scoping for pipe names and executable-marker
   paths, plus security-descriptor helpers and focused ownership tests. Accept
   injected session IDs and data directories in the derivation layer so pipe names
   and marker paths are test-overridable from the start.
2. Implement the cancellable named-pipe transport, message framing, and peer-PID
   acquisition behind injected test endpoints, including the concurrent instance
   pool.
3. Make caller resolution Windows-correct, add creation-time and native process/path
   tests, and establish each delegating agent's cwd-based authorization capability.
4. Broaden all module, store, startup, utility, and nudge gates; add the startup
   integration check.
5. Implement the Windows CLI transport and end-to-end request tests.
6. Verify all four agents' Windows instruction conventions and enable only the
   confirmed per-kind blocks.
7. Add build, installer-manifest, auto-update/rollback, signing, debug-symbol, and
   installed-bundle verification; align the installer minimum with Windows 10 1903.
8. Run Windows clippy/tests and the pinned Windows 10 1903 smoke test before
   declaring the platform supported.
