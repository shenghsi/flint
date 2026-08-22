## 1. Protocol and Command Compatibility

- [x] 1.1 Add protocol major and minor versions, current grouped request names, operation-specific success payloads, not-ready state, typed error codes, terminal metadata, and capability status types; verify serialization, unknown-field tolerance, rejection of old request names, and request/response size tests pass with `cargo test -p agent_control_protocol`.
- [x] 1.2 Add the `flintctl` command parser for `status`, noun-first `thread` commands, all `terminal` commands, and JSON output without old flat aliases; verify current parser and output tests pass and old flat forms fail with `cargo test -p agent_control_cli`.
- [x] 1.3 Build `flintctl` as the only control binary and remove the `flint-agent-control` binary target; verify `flintctl` builds and no `flint-agent-control` artifact is produced.
- [x] 1.4 Update executable lookup and rewrite the existing release-channel-scoped marker with the running version's `flintctl` path and metadata on every application launch; verify path-resolution, stale-marker replacement, and marker round-trip tests pass in `util`, `agent_control_protocol`, and `agent_threads`.
- [x] 1.5 Add stable begin and end markers plus a block format version to the Unix and Windows Agent Thread instruction blocks, and update their commands to `flintctl thread retie`; verify exact text and per-agent capability tests pass with `cargo test -p agent_threads instructions`.
- [x] 1.6 Replace the append-only instruction offer and dismissed state with launch-time synchronization for each supported installed agent; verify tests cover first insertion, unchanged current blocks, version and content replacement, no open workspace, synchronization while control is disabled, missing agents, and synchronization errors.
- [x] 1.7 Migrate exact known unmarked Flint blocks to one current marked block, preserve similar user-authored text, preserve all content outside managed boundaries, and use atomic file replacement; verify file tests cover leading and trailing user text, modified lookalikes, newline boundaries, write failure, and crash-safe replacement.
- [x] 1.8 Route only current noun-first thread requests through the existing retie and create handlers, keep the Agent Thread-only check, and return `caller-not-agent-thread` for ordinary terminal callers; verify current thread control tests and old-command rejection tests pass on Unix and Windows.

## 2. Terminal Identity and Access

- [x] 2.1 Add a process-local terminal control registry with opaque non-reused IDs, creation sequence, generation, root PTY process ID, working directory, and non-owning terminal, view, and workspace handles; verify unit tests cover ID order, non-reuse, stale entries, and nullable working directories.
- [x] 2.2 Register PTY terminals after their views exist, update their location when they move, and remove entries when a terminal or view is released; verify GPUI tests cover creation, cloned splits, moved splits, release, and exclusion of display-only terminals.
- [x] 2.3 Resolve ordinary callers by matching bounded peer-process ancestry to registered root PTY process IDs before the existing Agent Thread fallback; verify tests accept ordinary terminals and delegated Agent Thread processes but reject unrelated same-user processes and ordinary cwd-only matches.
- [x] 2.4 Enforce same-workspace target access and keep the socket, named pipe, and release-channel boundary unchanged; verify server tests return `terminal-outside-workspace`, keep Unix socket mode `0600`, and reject a mismatched release channel.
- [x] 2.5 Implement `terminal current` and `terminal list`, including creation-order sorting, default self-exclusion, `--all`, and terminal metadata; verify protocol, server, and CLI tests cover human and JSON results without pane positions.
- [x] 2.6 Apply bounded client retry only to caller not-ready responses and never to stale explicit terminal IDs; verify retry tests end as `caller-not-recognized` and stale targets return `terminal-not-found` without retry.

## 3. Bounded Terminal Reads

- [x] 3.1 Add one terminal snapshot API for `visible`, `recent`, and `recent-unwrapped`, including soft-wrap reconstruction and alternate-screen state; verify terminal tests cover primary scrollback, visible grids, alternate screens, empty content, and multi-row soft wraps.
- [x] 3.2 Enforce the default 120-line limit, configured maximum line count, `MAX_RESPONSE_BYTES`, UTF-8-safe truncation, and the `truncated` result field; verify boundary tests cover multibyte text at the byte limit and line counts above the maximum.
- [x] 3.3 Implement the `terminal read` server handler and CLI rendering for all sources and line options; verify current, list, and read integration tests cover live, exited, released, and non-PTY targets in human and JSON modes.

## 4. Terminal Input Operations

- [x] 4.1 Define and document the supported terminal key names and modifiers, and adapt the existing UI terminal key mapping for control requests; verify key parser tests cover supported names, terminal modes, platform behavior, and invalid names.
- [x] 4.2 Implement `terminal send-text` with complete pre-validation, no implicit Enter, no bracketed-paste framing, NUL rejection, and request-size enforcement; verify PTY tests assert exact bytes and no bytes for invalid, exited, or released targets.
- [x] 4.3 Implement `terminal send-key` so the full key list is validated before one foreground terminal update; verify multi-key tests assert request order, mode-aware bytes, and zero writes when one key is invalid.
- [x] 4.4 Implement `terminal run` so validated command text and Enter are written in one foreground update without a shell or prompt check; verify concurrency tests show that another control request cannot interleave bytes inside the run operation.
- [x] 4.5 Add human and JSON CLI results and typed failure handling for all three input commands; verify CLI-to-server integration tests cover success, `invalid-key`, `terminal-exited`, `terminal-not-found`, and oversized input.

## 5. Current Framing and Output Waits

- [x] 5.1 Add shared bounded length-prefixed framing for all requests and responses and reject non-current framing; verify codec tests cover partial reads, oversized lengths, malformed JSON, current requests, and EOF-framed rejection.
- [x] 5.2 Update the Unix client and server transport to keep connections open and detect disconnects; verify Unix transport tests cover normal responses, EOF-framed rejection, socket permissions, and mid-wait disconnect.
- [ ] 5.3 Update the Windows client and named-pipe server to use the same message-length and disconnect rules while keeping peer-process verification; verify Windows transport tests cover normal responses, old framing rejection, peer identity, cancellation, and size limits.
- [x] 5.4 Implement literal and Rust regular-expression matching against the shared bounded snapshot, including an initial search before observation; verify tests cover immediate matches, delayed matches, read-source selection, final snapshots, and `invalid-pattern`.
- [x] 5.5 Implement event-driven output observation without holding a GPUI entity update across await, and race it against timeout, exit, release, and client disconnect; verify GPUI tests use the GPUI executor timer and cover each completion path without pending tasks.
- [x] 5.6 Pin each wait to the resolved terminal entity and registry generation; verify a replacement terminal in the same pane or tab cannot satisfy the old wait.
- [x] 5.7 Add the CLI default wait timeout while requiring every wire request to contain an explicit timeout; verify parser and integration tests cover default and custom durations, literal-versus-regex exclusivity, nonzero timeout exit, and JSON output.

## 6. Packaging and Route Boundaries

- [x] 6.1 Exclude remote-host PTYs from the local registry while allowing local-route terminals in remote projects, and leave Direct and Tunneled Agent Thread launch, executable, credential, and traffic boundaries unchanged; verify route tests cover local, Direct remote, and Tunneled remote cases.
- [x] 6.2 Add `flintctl` to macOS, Linux, and Windows build, copy, strip, signing, installer, and package file lists, and remove all `flint-agent-control` entries; verify package-layout tests or dry runs find `flintctl` and do not find the old executable in each supported output layout.
- [ ] 6.3 Update Windows install and rollback handling so an update removes the obsolete `flint-agent-control.exe`, installs `flintctl.exe`, and restores the old file only when rollback returns to a release that contained it; verify `auto_update_helper` tests cover update, rollback, and first install.
- [x] 6.4 Verify each application launch rewrites the bundled marker to the packaged `flintctl` and synchronizes the current noun-first managed instruction block on each supported instruction shell; run the focused marker, upgrade-migration, package, and instruction integration tests.

## 7. Integrated Verification

- [x] 7.1 Run `cargo fmt --all -- --check` and fix all formatting drift.
- [x] 7.2 Run `./script/clippy` and fix all new warnings and errors.
- [x] 7.3 Run the focused test suites for `agent_control_protocol`, `agent_control_cli`, `agent_threads`, `terminal`, `terminal_view`, `workspace`, `util`, and `auto_update_helper`, and record any platform-only test coverage in the pull request.
- [x] 7.4 Build the local macOS bundle with `./script/bundle-tmp-app`; if the documented debug `remote_server` signing step fails, copy the fresh debug bundle manually, then verify `/tmp/Flint-Local.app` contains `flintctl` and does not contain `flint-agent-control`.
- [ ] 7.5 Run an end-to-end local smoke test from one Flint terminal for `status`, `current`, `list`, `read`, text input, key input, run, immediate wait, delayed wait, timeout, self exclusion, and cross-workspace denial; record the observed commands and results in the pull request.
