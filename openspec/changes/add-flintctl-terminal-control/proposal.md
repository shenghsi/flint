## Why

Flint's local control helper has a long, agent-specific command name and cannot inspect or operate ordinary terminals. Flint needs one stable, versioned local CLI that preserves Agent Thread automation and lets a process in one Flint terminal safely control another terminal in the same workspace.

## What Changes

- Install `flintctl` as the primary local control command, with noun-first `thread` and `terminal` command groups plus a capability-reporting `status` command.
- **BREAKING**: Stop building and packaging the `flint-agent-control` executable. Package only `flintctl` as the local control client.
- **BREAKING**: Remove the old flat command forms, legacy request names, and legacy request framing. The installed Flint version provides only its current commands and protocol.
- On every application launch, write the current release-channel-scoped executable marker and synchronize each supported, installed agent's Flint-managed global instruction block with the latest commands and instruction text from that Flint version.
- Add stable managed-block boundaries and a block version. Replace only Flint-managed text, preserve user content outside the block, and migrate the known unmarked block from earlier Flint versions.
- Add process-local terminal identities and lifecycle registration for live PTY-backed terminals.
- Resolve callers from operating-system process ancestry, with the existing constrained Agent Thread fallback, and restrict terminal access to the caller's workspace.
- Add commands to identify and list terminals, read bounded snapshots, send validated text or keys, run a command, and wait for matching output.
- Version requests and responses, add typed results and error codes, cap request and response sizes, and use length-prefixed framing for all commands and cancellable long-lived waits.
- Keep terminal control local to the machine that owns the Flint control server. Do not expose remote PTYs, cross-workspace terminals, pane management, persistent terminal hosting, or raw byte input.
- Package `flintctl` on macOS, Linux, and Windows, remove the old executable from package and updater manifests, and update new Agent Thread instructions to discover `flintctl` through the existing release-channel-scoped marker.

## Capabilities

### New Capabilities

- `local-terminal-control`: Defines the current-only `flintctl` command and protocol surface, terminal identity and caller boundary, terminal read/input/wait behavior, packaging, and local-only scope.

### Modified Capabilities

- `terminal-agent-threads`: Changes Agent Thread control commands to `flintctl thread` and makes each launched Flint version replace its managed global instruction blocks with that version's latest instructions.

## Impact

- Affects the Agent Control client, server, transport framing, Unix socket and Windows named-pipe handling, and release-channel endpoint discovery.
- Adds terminal lifecycle registration and workspace-aware lookup across terminal, terminal view, workspace, and application state.
- Extends terminal content access, key mapping reuse, foreground entity updates, and asynchronous output observation.
- Changes application bundles and Linux and Windows packages to include `flintctl` and remove `flint-agent-control`.
- Changes Agent Thread global instruction handling from an append-only user prompt to versioned, launch-time synchronization of Flint-managed blocks.
- Requires protocol, CLI, server, terminal behavior, route-boundary, instruction, and package tests on supported platforms.
