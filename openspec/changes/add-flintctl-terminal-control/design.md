## Context

See `proposal.md` for the motivation and the delta specs for observable behavior.

The current `flint-agent-control` client sends one JSON request per local connection. Unix uses a user-scoped socket, Windows uses a named pipe, and the server gets the peer process ID from the operating system. The server walks a bounded process ancestry and accepts a request only when it can map that ancestry, or the constrained working-directory and agent-kind fallback, to a live Agent Thread.

The current command has two flat operations: `retie-thread` and `create-thread`. Agent Thread instruction blocks discover the executable through a release-channel-scoped marker whose file name contains `agent-control`. Writing these commands into every agent's general global instruction file is broader than the task-specific control surface requires.

Flint already owns each PTY terminal, terminal view, workspace item, and split layout. A new or cloned split creates a new PTY and process. A moved split keeps the same terminal and process. Existing terminal APIs can read the emulated grid and scrollback, write input, paste text, and map UI key events. The missing controls are a stable identity for each live terminal, a registry that maps callers and targets, bounded wire results, and a wait operation that reacts to terminal output.

GPUI entities stay on the foreground thread. Socket and named-pipe input and output stay asynchronous. A long-lived output wait must not hold an entity update across an await point.

## Goals / Non-Goals

**Goals:**

- Keep the protocol as an internal local interface and make `flintctl` the stable automation surface.
- Give each live PTY terminal an identity that does not depend on its visual location.
- Resolve ordinary terminal callers without weakening the Agent Thread fallback.
- Keep access local, user-scoped, release-channel-scoped, and limited to one workspace.
- Reuse terminal rendering, content, and key semantics so control input behaves like UI input.
- Bound memory, request size, response size, line count, wait duration, and retry duration.
- Make the running Flint version the source of truth for dedicated control skills that the user chose to install.

**Non-Goals:**

- A persistent terminal server or a terminal multiplexer.
- Terminal creation, split, move, focus, close, or resize commands.
- Raw PTY file descriptors, arbitrary byte input, or shell-prompt detection.
- Cross-workspace control, public network access, or client-supplied authentication.
- Control of a PTY that runs on a remote host.
- Stable terminal IDs across application restarts.
- A crate rename in the first change.

## Decisions

### Make flintctl and its noun-first hierarchy the only control surface

Build `flintctl` beside the current helper location and move user-facing commands to noun-first groups:

```text
flintctl status
flintctl thread ...
flintctl terminal ...
```

Keep the Rust crate names in the first change. Crate names do not affect user behavior, and a rename would increase the review scope.

Stop building and packaging the `flint-agent-control` executable in this change. `flintctl` accepts only noun-first commands. Remove the old flat parser forms and old serialized request names. Keep the marker file name unchanged for endpoint discovery, but rewrite its value on each application launch so it points to the running version's `flintctl`.

This is a breaking change for scripts that invoke the old executable name or old flat commands. Do not provide a wrapper, adapter, second binary target, command alias, or protocol alias. Remove the old executable from package and updater manifests.

Update recorded Flint-owned skills during launch so current Agent Threads do not require old command aliases.

Alternative: Package a compatibility executable for the old name. Rejected because it keeps a second installed control surface that is not required for stored instructions.

Alternative: Keep old command and protocol aliases for stored skills. Rejected because the running Flint version can update skills it owns and must not keep obsolete commands as a permanent migration mechanism.

Alternative: Keep `flint-agent-control` as the primary command and add terminal commands to it. This would keep an agent-specific name for a general local control surface.

### Use an application-owned terminal control registry

Add one process-local `TerminalControlRegistry` to application state. For each controllable terminal, store:

- an opaque `TerminalControlId`;
- weak or otherwise non-owning handles to the terminal, terminal view, and workspace;
- the root PTY process ID;
- the last known working directory;
- a monotonic creation sequence; and
- a registry generation that can distinguish a pinned target from a later replacement.

Assign display IDs such as `t1`, `t2`, and continue the sequence for the life of the Flint process. Register only after both the PTY terminal and terminal view exist. Remove the entry when the terminal or view is released. Update location metadata when the terminal moves, but keep its ID.

Use non-owning handles so the registry cannot keep a closed terminal alive. Exclude display-only terminals because they cannot accept PTY input.

Alternative: Use pane or tab indexes. Layout changes make these indexes unstable.

Alternative: Use process IDs or GPUI entity IDs. These expose implementation identity, have unsuitable reuse rules, and do not give one stable public address.

### Resolve the caller from process ancestry before Agent Thread fallback

For each request, get the peer process ID through the local operating-system transport and walk the existing bounded ancestry depth.

First compare all ancestry process IDs with root PTY process IDs in the registry. This resolves a shell or command that runs in an ordinary Flint terminal. If there is no match, use the existing Agent Thread working-directory and agent-kind fallback. This supports coding agents that delegate tool commands to an application server outside the terminal ancestry. Map the matched Agent Thread back to its registered terminal.

Do not use a unique working directory to resolve an ordinary terminal. An unrelated local process can use the same directory. The working-directory fallback stays restricted to registered Agent Threads and its existing agent-kind checks.

After caller resolution, compare the caller and target workspace entities. Reject targets outside the caller's workspace. Thread operations also require that the resolved caller is a registered Agent Thread.

Keep the socket or named pipe local and user-scoped. Keep Unix permissions at `0600`. Never accept a client token as caller identity.

Alternative: Allow any same-user local process. This would let unrelated programs operate all open terminals.

Alternative: Use working directory as the workspace boundary. Two workspaces can contain the same or nested paths, and the directory does not prove terminal ownership.

### Keep terminal identity separate from layout identity

`terminal current` and `terminal list` return terminal metadata, not pane coordinates. Results include ID, title, nullable working directory, Agent Thread state, and exited state. Sort lists by creation sequence so drag and split operations do not reorder automation targets. Exclude the caller by default and add it only for `--all`.

The working directory stays nullable because a shell can report it late or not at all. This also keeps the result shape compatible with a possible later remote-terminal design.

Alternative: Expose pane positions for selection. Pane positions can change without a terminal process change, and the first command set does not manage layout.

### Build bounded reads from the terminal model

Add a shared snapshot operation for `visible`, `recent`, and `recent-unwrapped` sources.

- `visible` reads the current terminal grid.
- `recent` reads the current grid plus available primary-screen scrollback.
- `recent-unwrapped` joins rows that the terminal model marks as soft wrapped.

Apply the line limit before wire serialization where practical. Apply `MAX_RESPONSE_BYTES` to every response. If text must be cut, cut only at a valid UTF-8 boundary and set `truncated: true`. Include alternate-screen state in the result. Do not synthesize rows that already left the alternate screen because normal host scrollback does not contain them.

Use one snapshot implementation for `terminal read` and `terminal wait-output` so match behavior and returned text cannot diverge.

Alternative: Read the PTY output stream directly. The terminal model already owns parsing, screen state, scrollback, and soft-wrap metadata. A second parser would diverge from what the user sees.

### Validate complete input operations before one foreground update

`send-text` validates size and NUL exclusion before it updates the target. It calls terminal input directly, without Enter and without bracketed-paste framing.

`send-key` parses the full list before it writes anything. Reuse the existing terminal key mapping so cursor mode, keypad mode, and platform rules match keyboard events from the UI. Convert all keys first, then write them in one foreground update.

`terminal run` validates the command, then writes the command text and Enter in one foreground update. This prevents another control request from interleaving bytes inside this operation. It does not start a child shell or inspect prompt state.

Alternative: Implement `run` as two client requests. A concurrent request could write between the command text and Enter.

Alternative: Expose raw bytes. Raw input would increase the safety and compatibility surface and would bypass terminal key mode handling.

### Pin output waits to one terminal registration

For `terminal wait-output`, resolve the target once and keep its entity handle, ID, and registry generation. Take a bounded snapshot and search it before registration of a wait. If it does not match, subscribe to terminal content changes, return to the async task, and take a new snapshot after each notification.

Race the observation against timeout, target exit, target release, and client disconnect. Do not hold an entity update across an await point. If a terminal in the same pane is replaced, its different entity or generation cannot satisfy the old wait. Return the final bounded snapshot on a match.

Compile a regular expression before subscription. The CLI supplies a conservative timeout when the user omits it, but every wire request contains an explicit timeout. This keeps server resource use bounded even for a custom client.

Alternative: Re-resolve the terminal ID after every notification. A released target and later replacement could make a wait observe the wrong terminal.

Alternative: Subscribe before the initial search. This adds complexity to close a race that a foreground snapshot-plus-subscription setup can handle in one controlled sequence.

### Add one versioned, length-prefixed protocol

Keep one request and one response per connection. Current clients write a bounded length prefix followed by one JSON request and keep the connection open. During a wait, the server races terminal observation with a transport read that completes when the client disconnects. Use the same message-length rule for Unix sockets and Windows named pipes.

The server reads and validates the length before JSON decoding and rejects an oversized length. It rejects input that does not use the current framing.

Add an explicit protocol major and minor version to each current request and response. Reject unsupported required major versions. Treat minor changes as additive and configure clients to ignore unknown response fields.

Use explicit serialized request names that do not depend on command parser types. Use only the current grouped thread request names. Use operation-specific success payloads, machine-readable expected-error codes, and a separate `NotReady` state for caller-registration races.

The CLI retries `NotReady` with the existing bounded backoff. It does not retry an explicit stale target ID or other hard errors.

Alternative: Keep EOF request framing for all operations. After EOF, the Unix server cannot use another read to distinguish a connected waiting client from a disconnected one.

Alternative: Add a multi-request session protocol. The first command set needs only one request per connection and does not justify session state.

### Keep terminal control on the control server host

Expose only terminals whose PTY root process runs on the same machine as the control server. A local-route terminal in a remote project can be controlled by local `flintctl` because its PTY and caller identity are local. A shell that runs through the remote server is not registered in the local terminal control surface.

Do not copy `flintctl` to remote hosts for this feature. Keep Direct and Tunneled Agent Thread executable, credential, and traffic boundaries unchanged. Remote terminal control needs a separate identity and routing design.

Alternative: Forward local terminal commands to the remote host. The local server cannot verify remote caller ancestry or target identity with the current boundary.

### Package through the existing helper paths and marker

Add `flintctl` to macOS application bundles and Linux and Windows packages. Remove `flint-agent-control` from binary targets, package file lists, signing steps, and updater manifests. Use the existing marker location for discovery and keep endpoint and marker names release-channel scoped. The CLI connects only to a running matching Flint instance and does not launch the application.

Add package tests that inspect each supported package layout and prove that it contains `flintctl` but not `flint-agent-control`. Add skill tests that check the exact new command and marker behavior.

Alternative: Require `flintctl` in `PATH`. Flint already has marker-based discovery, and managed Agent Threads must use the executable from the matching application build.

### Install a dedicated control skill with consent

Bundle one concise, release-matched `flintctl` skill. Its metadata triggers on worktree creation or switching, Agent Thread coordination, and Flint terminal control. Its body first checks for the release-channel marker and matching control endpoint, then runs `terminal current --json` as the authoritative caller probe. A successful ordinary-terminal result permits terminal commands. Only an `is_agent_thread: true` result permits thread commands. A missing endpoint, connection failure, incompatible protocol, or unrecognized caller returns control to the normal task without Flint control.

Expose the bundled text through `flintctl skill print` without connecting to Flint. Add install, status, update, and uninstall operations for agent kinds whose skill directory convention is verified. The Settings Editor and Agent Threads UI show the destination and full text before initial installation. Initial installation always requires a user action.

Store an ownership record outside the skill with the agent kind, destination, bundled skill version, installed content digest, and release channel. Write the skill and record through temporary files in their destination directories and replace them atomically. A future Flint launch updates a recorded skill only when the file still matches the recorded digest. If the user changed it, keep it and show a conflict. An explicit replace action can adopt the current bundled content and digest. Uninstall removes only an unchanged recorded skill.

Do not add, update, or remove text in global `AGENTS.md`, `CLAUDE.md`, or other general instruction files. Existing Flint-managed blocks from earlier versions remain unchanged.

Alternative: Depend on the skill body to discover that it is inside Flint. Rejected because the body is not loaded until metadata triggers; the metadata must name the tasks that need the skill.

Alternative: Install the skill automatically when an agent executable is found. Rejected because skill installation changes an agent's configuration and requires consent.

Alternative: Update every file at a known skill path. Rejected because Flint can safely update only installations for which it recorded ownership and the prior content digest.

### Keep worktree retie explicit

Do not use a terminal environment variable as caller identity. The authoritative probe uses the operating-system peer identity and the control server's live terminal registrations. Direct and Tunneled remote launches keep their existing executable and routing boundaries.

Do not automatically retie a thread whenever its terminal working directory changes. Agents and users can enter another worktree temporarily for inspection, and the terminal working directory does not prove a durable ownership decision. The skill tells the agent to run `flintctl thread retie` after it creates the worktree that the current thread will own. This preserves explicit intent while avoiding always-on global instructions.

Alternative: Retie to every recognized worktree entered by the terminal. Rejected because temporary directory changes would silently move thread ownership and history.

## Risks / Trade-offs

- [A process can change ancestry through a daemon] → Permit the working-directory fallback only for registered Agent Threads with the existing agent-kind constraints. Reject this pattern for ordinary terminals.
- [A terminal closes during a foreground operation] → Use non-owning entity handles and return a typed terminal-not-found or terminal-exited error.
- [A terminal replacement satisfies an old wait] → Pin the entity and registry generation for the complete wait.
- [Large scrollback or output causes high memory use] → Enforce line, request, response, and timeout limits. Truncate text at a UTF-8 boundary.
- [Rapid output generates many notifications] → Coalesce through the existing GPUI notification behavior and take only bounded snapshots.
- [External scripts still invoke `flint-agent-control`] → Mark the removal as breaking and update first-party instructions and package tests to use only `flintctl`.
- [A user edits an installed skill] → Compare its content with the recorded digest, preserve it, and require an explicit replace action.
- [A skill update fails at launch] → Keep the original skill and ownership record intact, show the affected agent and path, and retry on the next launch.
- [Windows caller verification differs from Unix] → Keep transport-specific peer-process tests and require the same caller-resolution contract on both platforms.
- [Local-only control surprises users in remote projects] → Report capabilities through `status` and omit remote PTYs from list results instead of exposing partial control.

## Migration Plan

1. Add the `flintctl` parser, protocol version, status result, and noun-first thread commands. Remove the old binary target, flat commands, serialized aliases, and package references. Keep current thread behavior unchanged.
2. Add the bundled skill, consent-based installation, ownership records, launch-time updates, conflict handling, and current marker rewriting. Stop all new global instruction-file writes without removing old blocks.
3. Add terminal lifecycle registration, stable IDs, caller resolution, workspace checks, `terminal current`, and `terminal list`.
4. Add the shared bounded snapshot implementation and `terminal read`.
5. Add complete validation and atomic foreground updates for text, keys, and run input.
6. Add length-prefixed framing, terminal observations, disconnect cancellation, and `terminal wait-output`.
7. Verify protocol, CLI, server, terminal, skill lifecycle, package, Unix, Windows, local, Direct remote, and Tunneled remote behavior before release.

Rollback to an earlier release restores that release's old executable and marker value through the normal package or updater rollback. A recorded skill can be updated again by the version that next launches, but a user-modified skill is never replaced automatically. Keep the client, skill, marker, and server protocol versions aligned within each application build.
