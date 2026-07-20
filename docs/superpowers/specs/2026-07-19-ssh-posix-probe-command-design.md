# Make the POSIX Target Probe Safe for SSH

## Status

Approved in conversation on 2026-07-19.

Design owner: Codex.

## Problem

Flint determines a POSIX remote's operating system, architecture, and Linux
libc before selecting a remote-server artifact. The shared target probe is a
multiline `sh -c` script used by SSH, Docker, and WSL transports.

The SSH transport cannot safely pass multiline arguments through every
supported login shell. Its command builder therefore has a debug assertion
that rejects arguments containing newlines. `SshSocket::platform_posix` passes
the shared probe through this builder, so opening an SSH remote panics before
platform detection completes.

## Design

Keep the existing SSH command-building invariant and its assertion. Rewrite
the shared POSIX target probe as a single-line POSIX shell command using
semicolons where command boundaries are required. Preserve its current
behavior:

- fail if `uname -s` or `uname -m` fails;
- report no libc for non-Linux targets;
- identify glibc with `getconf GNU_LIBC_VERSION`;
- otherwise inspect `ldd --version` for musl; and
- retain `unknown` when neither libc can be identified.

SSH, Docker, and WSL continue to call the same `posix_target_probe_command`
function. No transport-specific copy of the detection logic is introduced.
The tagged output format and `parse_platform` remain unchanged.

This is intentionally narrower than adding a general remote-script API.
Future multiline remote scripts may warrant an explicit stdin-backed API, but
the platform probe does not require that additional lifecycle and error-handling
surface.

## Error Handling

The existing command status checks and parsing errors continue to propagate to
the remote-project UI. The fix removes the debug panic without weakening the
SSH compatibility guard. A failed probe remains a recoverable connection error
rather than producing a guessed platform.

## Tests

Add a regression test that verifies every argument produced by
`posix_target_probe_command` is single-line, directly protecting the SSH
command-builder contract. Keep the existing execution test to verify that the
compact probe runs under a local POSIX `sh` and produces a parseable tagged
target.

Run the focused `remote` crate tests, formatting, and workspace clippy. Build
and install a fresh `/tmp/Flint-Local.app` for manual SSH validation.
