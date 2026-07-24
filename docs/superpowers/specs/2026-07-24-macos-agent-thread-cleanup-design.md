# macOS Agent Thread Cleanup

## Problem

Remote Agent Thread cleanup verifies a POSIX process by searching the custom
`ps` command column for `FLINT_AGENT_THREAD_ID`. macOS does not include the
process environment in that column, so cleanup exits with status 67 and leaves
the agent process running.

## Design

Keep the existing lifecycle ID, process ID, process group, and process start
time checks on POSIX remotes. Remove only the environment-marker check from the
POSIX branch. Retain the stronger `/proc/<pid>/environ` check on Linux, where
the environment is directly available.

This preserves protection against PID reuse on macOS because a reused PID must
also match both the recorded process group and the recorded process start time
before Flint sends a signal.

## Testing

Add a macOS-only regression test that:

1. Launches the real wrapper script with a long-running child.
2. Waits for its lifecycle record.
3. Runs the real cleanup script.
4. Verifies cleanup succeeds, removes the record, and terminates the wrapper.

Existing unit tests continue to cover command construction and shutdown
selection.
