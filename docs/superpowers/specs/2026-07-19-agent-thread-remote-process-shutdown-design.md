# Agent Thread Remote Process Shutdown Design

**Date:** 2026-07-19
**Status:** Implemented and verified

## Problem

Closing an Agent Thread currently removes its terminal item and store entry but
does not explicitly stop the agent. Dropping the terminal eventually kills the
local SSH client, but that is not a remote-process lifecycle guarantee. On the
reported cluster, a root-owned `log-user-session` wrapper retains the remote
PTY after SSH closes, leaving Codex running without a corresponding Flint
thread.

Dropping the Agent Thread store entry also drops its `AgentEgressLease`. A
leaked Through-Flint process therefore loses its proxy capability and fails
closed, but still consumes remote resources and cannot be managed from Flint.

## Goals

- Closing a POSIX remote Agent Thread explicitly terminates its remote agent
  process.
- Give the agent a short opportunity to exit cleanly before forcing it.
- Target only the process created for the closed Agent Thread and its children.
- Keep Through-Flint egress alive while graceful shutdown is in progress.
- Use the same shutdown path for a direct tab close and route-change cleanup.
- Leave ordinary terminal-tab behavior unchanged.
- Surface shutdown failures instead of silently leaking a remote process.

## Non-goals

- Keeping an Agent Thread alive after its Flint terminal is closed.
- Managing processes that were not launched as Agent Threads.
- Guaranteeing remote termination while the SSH host is unreachable. Flint
  revokes local egress and reports the failure in that case.
- Changing the existing SSH-disconnect or tunnel-restoration design.
- Adding a Windows remote process supervisor. Windows keeps the graceful PTY
  interruption and existing terminal teardown until a separately reviewed
  platform-specific force-cleanup design is added.

## Considered Approaches

### Graceful interruption followed by targeted force termination

This is the selected approach. Flint sends an interrupt through the existing
PTY, waits for the task to complete, and uses a separately authenticated remote
cleanup command only if the grace period expires.

It preserves normal CLI shutdown when possible while still providing a bounded
close operation on hosts where SSH teardown does not kill the remote command.

### Immediate force termination

This is simpler and fast, but it can prevent the CLI from flushing session data
or restoring terminal state. It is reserved for the timeout fallback.

### Rely on SSH or terminal-object teardown

This is the current behavior. It is rejected because the reproduced cluster
keeps the remote PTY and Codex process alive after the local SSH process exits.

## Process Identity

Every remote Agent Thread receives a random lifecycle UUID. Flint adds it to
the agent environment as `FLINT_AGENT_THREAD_ID` and launches the agent through
a fixed Flint-generated POSIX supervisor. Before starting the agent as its
foreground child, the supervisor creates a user-private lifecycle directory
and atomically records:

- the lifecycle UUID;
- its process ID;
- its process-group ID; and
- the process start identity needed to reject PID reuse.

The recorded process is the supervisor, whose process group is scoped to that
interactive Agent Thread and contains the agent and commands it launches unless
they deliberately create a different session. The supervisor waits for the
agent and removes the lifecycle record on normal exit. Forced cleanup targets
the validated process group rather than relying on the supervisor to run its
exit handler.

The record is stored below a mode-`0700` Flint runtime directory and written
with mode `0600`. User-provided labels, paths, commands, and arguments are not
interpolated into the wrapper or cleanup program. The lifecycle UUID is parsed
and validated before it is used in a path.

On Linux, identity validation compares the lifecycle environment marker and
the process start-time field from `/proc/<pid>/stat`. A platform that cannot
validate both the marker and start identity must return a safe cleanup error
instead of signaling an unverified PID.

The wrapper applies to remote Agent Threads under both routing choices. Local
Agent Threads continue to use the terminal subsystem's local process control.

## Shutdown Coordinator

`AgentThreadStore` remains the owner of Agent Thread lifecycle policy. Each
remote `ThreadEntry` retains:

- its terminal entity;
- its remote connection identity;
- its lifecycle UUID; and
- its optional egress lease.

When the terminal item is removed, the store takes the entry and starts one
shutdown operation. Removing the entry makes the thread disappear from the UI
immediately, while the shutdown task owns the terminal and egress lease until
cleanup completes. Repeated close requests find no live entry and cannot start
a second cleanup.

The shutdown sequence is:

1. Obtain the terminal completion task.
2. Write the terminal interrupt byte (`Ctrl-C`) to the existing PTY.
3. Wait up to two seconds for normal task completion.
4. If the agent remains alive, run the fixed cleanup command through the
   project's shared SSH ControlPath so it reaches the same backend selected by
   a load-balanced SSH alias.
5. The cleanup command validates the lifecycle record against the live process
   and start identity, sends `SIGTERM` to its process group, waits up to 500 ms,
   then sends `SIGKILL` if the process is still present.
6. Remove the lifecycle record after confirmed exit or confirmed absence.
7. Release the egress lease after the shutdown result is known.

The force path must not fall back to an unvalidated PID or a process-name-wide
operation such as `pkill codex`.

## Close Sources

Direct terminal-tab removal starts cleanup from the Agent Thread release
observer. Workspace closure uses the same observer. The cleanup task retains
the state it needs after the UI entity disappears.

`close_threads_for_connection`, used when changing remote routing, explicitly
awaits every affected shutdown before reporting success. This prevents the new
route from being installed while old agents or egress leases are still alive.

Application shutdown makes the same bounded best-effort request while the
remote connection is available. An operating-system-forced application exit
cannot provide a remote cleanup guarantee.

## Failure Handling

If graceful completion succeeds, remote force cleanup is skipped. A missing
lifecycle record is successful only when Flint can also confirm that the
recorded process no longer exists.

If process identity validation fails, Flint refuses to signal the PID and
reports that the remote Agent Thread could not be safely terminated. If SSH is
unavailable, Flint reports that explicit remote cleanup could not run. In both
cases the egress lease is released so the leaked process loses its capability
immediately.

Direct tab close reports asynchronous cleanup failures through Flint's normal
error notification path. Route changes return the error to their existing UI
flow and do not silently claim successful cleanup.

## Testing

Tests follow red-green-refactor and cover:

- closing a registered Agent Thread starts exactly one shutdown;
- a graceful terminal exit does not invoke remote force cleanup;
- expiration of the two-second grace period invokes targeted cleanup;
- the egress lease remains owned until cleanup succeeds or fails;
- route-change cleanup waits for termination before returning;
- cleanup rejects malformed lifecycle IDs, PID reuse, and identity mismatch;
- generated launch and cleanup commands quote fixed arguments safely;
- ordinary terminal tabs retain their existing close behavior; and
- cleanup uses shared connection routing rather than a dedicated SSH
  connection.

The focused Agent Threads tests run first. The full `agent_threads`, `terminal`,
`project`, and `remote` test suites and Flint clippy checks run before delivery.

## Acceptance Criteria

- After closing a POSIX remote Agent Thread while SSH remains reachable, its
  recorded supervisor PID no longer exists after the bounded shutdown
  interval.
- A graceful Codex exit completes without the force cleanup path.
- A non-responsive test agent is force-terminated without affecting another
  concurrent Agent Thread.
- A Through-Flint process retains working egress during the grace period and
  loses its capability when cleanup completes or fails.
- Changing the connection route cannot finish while an affected Agent Thread
  shutdown is still pending.
- Closing a non-Agent terminal does not invoke Agent Thread cleanup.
