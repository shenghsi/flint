# Caller Disambiguation for Daemon-Routed Agent CLIs

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

## Non-goals

- Giving a daemon-routed caller the same strong ancestry proof as a direct
  child of a Flint PTY. The cwd, kind, and session fallback has the weaker
  same-user boundary described above.
- Solving ambiguity for a daemon-routed kind that has no per-session
  environment variable available on its tool-call processes. Such a kind
  stays unresolved when two of its threads share a worktree, same as today.
- Guaranteeing immediate control for concurrent fresh Codex threads before
  Flint has attached their session IDs.
- Changing how `resumed_session_id` is discovered or persisted. A supported
  direct session-registration channel is future work.

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
