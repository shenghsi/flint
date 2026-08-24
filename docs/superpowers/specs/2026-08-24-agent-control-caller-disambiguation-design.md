# Caller Disambiguation and Agent-Initiated Terminal Creation

## Status

This document is a design proposal. No implementation work has started.

## Goal

Give the agent control server (`crates/agent_threads/src/control.rs`) the best
available way to answer one question: which live Agent Thread, if any, does
this connecting process belong to? The answer must be exact when the
available signals identify one thread. The server must return
`caller-not-recognized` when the signals are missing or ambiguous. This rule
must also apply to a CLI that routes tool commands through a shared,
long-lived daemon instead of forking its own child process.

After Flint identifies the caller, let the caller create a plain terminal,
create a split terminal, or create a sibling Agent Thread. Terminal creation
must use the caller's workspace and current placement as its scope. It must
not depend on whichever pane the user has focused.

## Background

`crates/agent_threads/src/control.rs` and
`docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md`
already describe a two-step caller resolution:

1. **Process ancestry.** The server reads the true peer process ID of the
   connecting socket from the operating system (`LOCAL_PEERPID` on macOS,
   `SO_PEERCRED` on Linux, the named-pipe client PID on Windows), then walks
   its parent chain looking for a PID the server already tracks as a live
   terminal. This step needs no cooperation from the agent CLI and is the
   strong signal.
2. **Working-directory fallback.** Some CLIs, Codex among them, run tool
   commands through an already-running daemon (`codex app-server`) instead
   of forking a child of the interactive session. No ancestor PID is ever
   one Flint tracks, so step 1 finds nothing. The server then matches the
   connecting process's own current working directory against each tracked
   thread's tied worktree root. When more than one thread ties to that root,
   the server tries to break the tie using the connecting process's own
   ancestry process names against each candidate's `kind_id` (for example,
   `"codex"`).

This design is already agent-neutral by construction. Unit tests in
`control.rs` already model the Codex daemon topology
(`resolve_caller_thread_falls_back_to_cwd_when_ancestry_has_no_match`) and
pass.

## The problem

Step 2's tie-break by process name only works when the ambiguous candidates
are of *different* kinds. When two Agent Threads of the *same* kind are tied
to the same worktree at the same time, the connecting process's name (for
example, `"codex"`) matches every candidate equally, and the server correctly
refuses to guess. This is exercised today by
`resolve_caller_thread_refuses_an_ambiguous_cwd_match`.

This is not a rare case. It is the normal case for a user who runs more than
one Codex Agent Thread in the same worktree at once. Live evidence from this
machine's own Flint log (`~/.local/share/flint/logs/Flint.log`) confirms it:

```text
agent control caller 891556 could not be resolved:
ancestry [(891556, "flintctl"), (2946707, "codex"), (2946588, "node"), (1, "systemd")]
matched none of the tracked pids [...]
tracked worktrees:
  (codex, /mnt/storage/xis/dev/flint)
  (codex, /mnt/storage/xis/dev/flint)
  (claude, /mnt/storage/xis/dev/flint)
  ...
```

Two `codex`-kind threads are tied to the same worktree. Working-directory
matching finds both. Process-name matching cannot separate them: both
candidates' `kind_id` is `"codex"`, and the caller's own ancestry contains
`"codex"` regardless of which of the two threads it belongs to. The server
reports `caller-not-recognized`, correctly, given the information it has
today.

### A separate, smaller problem

`crates/agent_control_skill/skills/flintctl/SKILL.md` gates all use of
`flintctl` on `FLINT_AGENT_THREAD=1` being set in the calling process's own
environment. Flint sets this variable once, at launch time, for every local
Agent Thread process
(`apply_control_skill_environment` in `crates/agent_threads/src/store.rs`).
For a CLI that forks its own tool subprocess (Claude Code, for example), the
variable is inherited and current. For Codex, the variable's value comes
from whenever its shared `app-server` daemon last started, which can predate
the current Agent Thread entirely, or be missing if the daemon started
outside any Flint Agent Thread.

This gate sits in front of the server-side resolution above and can hide a
real Agent Thread from the skill even when the server-side resolution would
have found it. It is a real defect, but fixing it alone does not fix the
same-kind ambiguity problem, which is a caller-identity gap in the server,
not a skill-side gate problem.

## Proposed method

**Rule:** never treat a value a local process can freely claim about itself
(an environment variable, a declared ID) as sufficient identity on its own.
Use it only to break a tie after the operating system has supplied the peer
PID and Flint has narrowed the candidates by current working directory and
agent kind. The session ID is a disambiguation signal. It is not an
authorization token.

The daemon fallback is weaker than terminal-PID ancestry. A same-user process
can choose a working directory, process name, and environment. The local
socket remains user-scoped and the server requires a matching live local
Agent Thread, but the fallback cannot prove that the caller descended from a
Flint PTY. This is an accepted limit for daemon-routed agents. The server
must not describe this fallback as equivalent to the strong ancestry match.

### New step: session-ID tie-break

Add one step, used only when the working-directory step still has more than
one candidate of the *same* kind:

3. **Session-ID tie-break.** Some agent CLIs carry a stable, per-run session
   ID in their own process environment, even when routed through a shared
   daemon. Codex sets `CODEX_THREAD_ID` on every tool-call process it
   spawns, confirmed present and stable across calls in the same session.
   Flint already learns and stores a matching Codex session ID per Agent
   Thread today, for history resume
   (`AgentThreadMetadata::resumed_session_id`, populated by
   `attach_discovered_session_id` in `store.rs`). The server can read the
   ambiguous candidates' stored session IDs and the connecting process's own
   `CODEX_THREAD_ID` (via `sysinfo`'s `Process::environ()` and
   `ProcessRefreshKind::with_environ`) and keep only the candidate whose
   stored ID matches. Exactly one match resolves the caller. Zero or more
   than one match stays unresolved, the same fail-closed behavior the server
   already uses everywhere else.

Process-environment access can fail on every supported platform because of
permissions, process exit, or operating-system restrictions. An unavailable,
missing, non-Unicode, or empty value is no match. The server must not fall
back to guessing after this failure.

### Session association readiness

The tie-break can work only after Flint has attached a session ID to the live
terminal. A fresh Codex thread starts with
`AgentThreadMetadata::resumed_session_id == None`; the existing background
history discovery runs every 30 seconds and can also remain ambiguous when
several new sessions appear in the same project.

The current discovery candidate filter excludes remote projects. Extend it to
use the existing remote history index through the authenticated project
connection. Apply the same launch-time, project, kind, and already-bound rules
as local discovery. Do not scan a remote user's files from the local
filesystem. A connection failure leaves the session unassociated and retries
on the next bounded discovery interval.

This design does not claim that `CODEX_THREAD_ID` solves that earlier
association problem. The server follows these rules:

1. If exactly one cwd-and-kind candidate exists, resolve it as today. The
   session ID is not needed.
2. If several same-kind candidates exist, use only candidates that already
   have an attached session ID.
3. If exactly one attached ID matches the peer's session ID, resolve it.
4. If no ID matches, more than one ID matches, or one or more candidates are
   still unassociated and prevent a unique result, return
   `caller-not-recognized`.

The skill can retry a not-ready response through the existing CLI retry
deadline, but it must not wait without a bound. If concurrent fresh Codex
threads cannot be associated by the existing history discovery, control from
those daemon-routed threads remains unavailable. A later design can add a
supported Codex-to-Flint session registration channel if Codex exposes one.
The current official Codex documentation does not establish a launch option
that lets Flint assign a session ID to a fresh session, so this design does
not invent one.

### Remote caller disambiguation

Remote development uses the same resolution order on the remote host. The
remote `flintctl` bridge gets the peer PID, and `flint-remote-server` reads the
remote process ancestry, cwd, process names, and configured session
environment variable. Local Flint must send the live Agent Thread kind and
attached session ID as connection-bound metadata for the matching remote PTY
registration. The remote server must not scan local paths or infer this state
from a client claim.

When local Flint attaches or changes a session ID, it updates the live remote
registration through the authenticated project connection. The remote server
discards this metadata when the PTY, connection, or registration generation
ends. Until the update arrives, ambiguous same-kind remote callers remain
unresolved. Direct and Tunneled routes use the same identity exchange; the
exchange does not change how either agent executable starts or reaches its
provider.

### Generalizing beyond Codex

To keep this usable for a future daemon-routed agent kind without special
casing, add one optional field to `AgentKindDefinition`
(`crates/agent_threads/src/agent_threads.rs`):

```rust
/// Environment variable this kind's own CLI sets on every tool-call
/// process, carrying a session ID stable for that CLI run. Used only to
/// break a tie between live threads of this kind tied to the same worktree,
/// after ancestry, cwd, and process name have already narrowed the
/// candidates to that kind. This value is not an authorization token.
/// `None` for kinds that do not provide a stable session ID in each
/// tool-call process.
pub caller_session_env_var: Option<&'static str>,
```

Only Codex sets this today (`Some("CODEX_THREAD_ID")`). A kind whose CLI
forks its own tool subprocess directly never needs it, because step 1
already gives a strong, unambiguous answer for it.

### Full resolution order

```text
peer PID -> tracked terminal PID (ancestry walk)
  |  no match
  v
peer's own cwd -> tracked worktree root
  |  0 candidates -> unresolved
  |  1 candidate  -> resolved
  |  2+ candidates
  v
peer's own ancestry process names -> candidate kind_id
  |  narrows to 1 -> resolved
  |  2+ candidates of the SAME kind remain
  v
peer's own <kind's caller_session_env_var> -> candidate's stored session ID   [NEW]
  |  exactly 1 match -> resolved
  |  0 or 2+ matches
  v
caller-not-recognized (unchanged, fail closed)
```

### `SKILL.md` probe

Remove `FLINT_AGENT_THREAD` completely. The skill locates the release-matched
executable and calls `flintctl terminal current --json`. A successful response
with `is_agent_thread: true` is the sole positive answer. A missing marker,
connection failure, protocol mismatch, `caller-not-recognized`, or
`is_agent_thread: false` means that the skill must continue without Flint
Agent Thread control.

Remove `apply_control_skill_environment`, its launch-path call, and its unit
test from `store.rs`. Update the active OpenSpec requirement and scenarios so
they require the probe instead of the environment variable.

## Agent-initiated creation commands

The current `terminal` command group can inspect and operate only terminals
that already exist. Add these commands:

```text
flintctl terminal open [--cwd <path>] [--focus]
flintctl terminal split (--current|--terminal <terminal-id>) \
  --direction <left|right|up|down> [--cwd <path>] [--focus]

flintctl thread create --worktree <current|new> --agent <agent> \
  --prompt <prompt> [--split <left|right|up|down>] [--focus]
```

`thread create` already exists. This change keeps it in the `thread` group,
adds optional placement for a current-worktree thread, returns the created
terminal identity, and teaches the installed skill when to use it. Do not add
a second terminal command that starts an agent.

### `terminal open`

Create a new plain shell terminal as another item in the caller's current
terminal pane. The new terminal has a new `Terminal`, PTY, shell process,
`TerminalView`, and `TerminalControlId`. It does not copy the caller's running
shell state or start an Agent Thread.

The default working directory is the caller terminal's last known working
directory. If that value is unavailable, use the workspace terminal default.
An explicit `--cwd` must be an absolute existing directory on the machine
that will own the PTY. The caller can already change to any accessible local
directory, so the path does not have to be inside a project root.

The command does not move user focus by default. `--focus` activates the new
terminal after creation. The response returns the same terminal metadata as
`terminal current`.

### `terminal split`

Create a new plain shell terminal in a new pane adjacent to the selected
terminal. `--current` selects the caller. `--terminal` selects an existing
terminal in the caller's workspace. The two forms are mutually exclusive and
one is required. The direction is required so the server never guesses from
window geometry or focus state.

The default working directory is the selected terminal's last known working
directory, with the workspace terminal default as the fallback. `--cwd` and
`--focus` follow `terminal open` semantics. A split creates an empty terminal;
it does not use Flint's clone mode and does not copy the selected shell state.

The server splits the exact pane that owns the selected `TerminalView`. It
must work for a terminal in the terminal panel and for a terminal in the
workspace center. It must not dispatch the normal UI split action, because
that action uses active-pane state and normally moves focus. Add a shared
placement helper that takes the source pane, pane group, direction, and focus
choice explicitly.

The registry must therefore also retain the terminal's current owning `Pane`
and placement surface. Update this information when the terminal moves. Pane
identity remains an internal placement detail; it does not become a public
control ID.

### `thread create`

Only a resolved Agent Thread can use `thread create`, as today. The command
continues to validate the agent kind, prompt support, worktree mode, and
remote route before it changes UI state.

For `--worktree current`:

- Without `--split`, create the Agent Thread as another item in the caller's
  current terminal pane.
- With `--split`, create it in a new pane adjacent to the caller in the
  requested direction.
- Do not move focus unless `--focus` is present.

For `--worktree new`, Flint creates or opens the destination worktree
workspace and launches the Agent Thread there. `--split` is invalid because
the caller's pane belongs to another workspace and there is no target pane in
the destination workspace. `--focus` activates the destination only when the
caller asks for it. The default remains a background, non-activating
workspace.

Refactor `launch_seeded_thread` so the control handler receives the created
terminal view instead of a Boolean. The success response includes:

```json
{
  "worktree": "/path/to/worktree",
  "terminal": {
    "id": "t7",
    "title": "codex",
    "working_directory": "/path/to/worktree",
    "is_agent_thread": true,
    "has_exited": false
  }
}
```

This lets the caller immediately use `terminal read`, `terminal wait-output`,
or other terminal commands without racing `terminal list`.

### Creation completion and errors

A creation request succeeds only after the PTY, `TerminalView`, pane
placement, Agent Thread registration when applicable, and
`TerminalControlRegistry` entry exist. Return the created terminal metadata
in the same response. Do not leave an unplaced terminal or an unregistered
Agent Thread when a later creation step fails.

Add these typed errors:

```text
invalid-working-directory
invalid-split-direction
invalid-placement
terminal-create-failed
terminal-placement-failed
remote-terminal-create-failed
```

`flintctl status --json` reports `terminal-open`, `terminal-split`, and the
current `thread-create` capability separately. A client must not infer support
from the protocol minor version alone.

### Access and remote boundaries

Any caller that resolves to a live local controllable terminal can use
`terminal open` and `terminal split`. Only a caller that also resolves to a
registered Agent Thread can use `thread create`. Every explicit target must
belong to the caller's workspace.

Local and remote callers get the same command forms, defaults, response
shapes, focus behavior, and workspace checks. The new PTY follows the selected
terminal's route:

- `terminal open` follows the caller terminal's local or remote route.
- `terminal split` follows the selected terminal's local or remote route.
- A remote `--cwd` is a path on the remote host and uses that host's path
  style and directory validation.
- Local Flint owns pane placement and terminal metadata for both routes. The
  remote server owns only the remote PTY process and its registration.

The remote bridge forwards the same control request to local Flint. Local
Flint creates the terminal through the existing project terminal route and
waits for the new remote PTY registration before it returns success. A remote
creation failure returns a typed error and cleans up local partial placement.

`thread create` keeps its existing local, Direct, and Tunneled route behavior.
Placement options must not change executable, credential, or traffic routing.
Direct uses the configured ambient remote executable. Tunneled uses the pinned
Flint-managed remote executable and its existing local traffic tunnel.

### Skill behavior

After the probe succeeds, the skill uses the installed executable's help as
the syntax authority. Local and remote managed skills use their respective
release-matched markers and the same command policy. The skill uses:

- `terminal open` when the user needs another shell terminal without a new
  pane;
- `terminal split` when the user asks for a split terminal or needs visible
  side-by-side terminal work;
- `thread create` when the user asks for another coding agent or when a
  delegated Agent Thread is necessary for the requested work.

The skill preserves the caller's working directory unless the user asks for
another directory. It does not request focus unless the user asks to focus the
new terminal. It must not create a worktree or Agent Thread only because a
plain terminal is sufficient.

## Non-goals

- Giving a daemon-routed caller the same strong ancestry proof as a direct
  child of a Flint PTY. The cwd, kind, and session fallback has the weaker
  same-user boundary described above.
- Solving ambiguity for a daemon-routed kind that has no per-session
  environment variable available on its tool-call processes. Such a kind
  stays unresolved when two of its threads share a worktree, same as today.
- Guaranteeing immediate control for concurrent fresh Codex threads before
  Flint has attached their session IDs.
- Inventing a Codex session-registration channel that Codex does not support.
  This design extends the existing history discovery to remote projects but
  keeps its conservative association rules.
- Moving, closing, resizing, or reordering existing panes through
  `flintctl`.

## Open questions

- Does every supported Codex version put `CODEX_THREAD_ID` in every tool-call
  process? The implementation must fail closed when it is absent, regardless
  of the answer.
- Do the macOS and Windows production builds have permission to read the peer
  process environment? The implementation must treat an empty result as no
  match and the release validation must exercise both platforms.
- Can a supported future Codex hook or app-server protocol report the session
  ID together with a Flint-provided terminal identity? If so, a later design
  can remove the fresh-session readiness limit.

## Verification

- Extend the existing `control.rs` test suite with:
  - Two same-kind candidates tied to the same worktree, disambiguated by a
    matching `caller_session_env_var` value on the connecting test process.
  - The same setup with no matching stored session ID (stays unresolved).
  - The same setup with the stored session ID matching more than one
    candidate (stays unresolved).
  - A kind with no `caller_session_env_var` set still refuses to
    disambiguate same-kind candidates (regression coverage for
    `resolve_caller_thread_refuses_an_ambiguous_cwd_match`).
  - A matching peer session ID with candidates whose stored session IDs are
    all missing stays unresolved.
  - Process-environment access that returns no data, an empty value, or an
    invalid operating-system string stays unresolved.
  - A peer process that exits during inspection stays unresolved.
- Add an automated end-to-end control test with a detached process, two live
  Codex-kind terminals in one worktree, and distinct attached session IDs.
  Verify that `terminal current --json` selects the matching terminal.
- Add a test that `terminal current --json` stays unresolved for two
  concurrent fresh Codex-kind terminals whose session IDs are not attached.
- Add skill tests for a successful Agent Thread probe, an ordinary-terminal
  response, a missing marker, a connection failure, a protocol mismatch, and
  `caller-not-recognized`.
- Add protocol and CLI tests for `terminal open`, `terminal split`, the four
  split directions, mutually exclusive targets, cwd validation, focus flags,
  typed errors, capability reporting, and the expanded `thread create`
  result.
- Add GPUI tests that create and register a terminal in the caller's pane,
  split beside the exact selected terminal in both the terminal panel and
  workspace center, keep focus by default, focus only when requested, and
  clean up all partial state after failure.
- Add Agent Thread tests for current-worktree tab placement, each split
  direction, returned terminal identity, new-worktree background behavior,
  rejection of `--split` with `--worktree new`, and exact error propagation.
- Test that an ordinary local terminal can open and split terminals but
  cannot create an Agent Thread.
- Test local, Direct remote, and Tunneled remote boundaries for all three
  creation operations. For Direct and Tunneled, verify matching command forms,
  cwd behavior, placement, focus, returned identity, cleanup, and exact
  executable and traffic boundaries.
- Add remote bridge tests for connection-bound kind and session metadata,
  update after session discovery, same-kind disambiguation, stale-generation
  rejection, disconnect cleanup, and missing-session fail-closed behavior.
- Add remote session-discovery tests for fresh Codex threads, reconnect,
  history lookup failure, already-bound IDs, and ambiguous concurrent
  sessions.
- Verify on Linux, macOS, and Windows that environment-read failure is handled
  as no match. A manual platform check can supplement the automated tests but
  cannot replace the detached-process regression test.
- Remove all production and test references to `FLINT_AGENT_THREAD`.

## Implementation order

1. Add `caller_session_env_var` to `AgentKindDefinition`; set it for Codex
   only.
2. Add the session-ID tie-break step to `resolve_caller_thread` in
   `control.rs`, gated on there being more than one same-kind candidate
   after the existing steps.
3. Add the new tests listed under Verification.
4. Update `SKILL.md` to use the `flintctl terminal current --json` probe.
5. Remove `FLINT_AGENT_THREAD`, `apply_control_skill_environment`, and their
   tests.
6. Update
   `docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md`'s
   "Caller resolution and access boundary" section to describe the new
   step, so the two documents stay consistent.
7. Update
   `openspec/changes/add-flintctl-terminal-control/specs/terminal-agent-threads/spec.md`
   and its related tasks and design text to replace the environment gate with
   the control-server probe.
8. Extend the terminal registry with exact pane and placement-surface state,
   and add shared no-focus terminal placement helpers.
9. Extend conservative session discovery to remote projects and synchronize
   attached kind and session metadata to the connection-bound remote PTY
   registration.
10. Add `terminal open` and `terminal split` to the protocol, CLI, local and
   remote dispatchers, capability result, and installed skills.
11. Extend `thread create` with current-worktree split placement, focus
    control, and the created terminal metadata result. Keep new-worktree
    creation non-activating by default.
12. Update
    `docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md`
    `docs/superpowers/specs/2026-08-22-flintctl-remote-dev-design.md`, and the
    active OpenSpec with the new creation commands, access rules, remote
    behavior, response shapes, errors, tests, and capabilities.
