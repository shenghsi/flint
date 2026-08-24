# Caller Disambiguation for Daemon-Routed Agent CLIs

## Status

Change A is implemented. Change B did not pass its required step 0 platform
check and is not implemented.

On 2026-08-24, a live macOS test spawned a same-user child with a unique
environment variable and refreshed it with the pinned `sysinfo` 0.37.2
`ProcessRefreshKind::with_environ(UpdateKind::Always)`. `sysinfo` found the
child and its executable name but returned no matching environment entry. The
active Codex `CODEX_THREAD_ID` did match both `session_meta.payload.id` and
`session_meta.payload.session_id` in its rollout file, so the identifier
assumption passed and the macOS environment-access assumption failed. Per step
0, do not implement Change B with peer-process environment reads. A future
design needs another connection-bound session signal before Change B can resume.

Two limits apply. Read them before the rest of this document:

- The tie-break in Change B needs an attached session ID on **every** candidate.
  Codex gets its session ID from a background history scan that runs every 30
  seconds, so two *fresh* concurrent Codex threads stay unresolved. See "What
  this does not fix".
- Change B rests on two unverified assumptions. Step 0 verifies them. If either
  assumption fails, do not build the rest of Change B.

## Goal

Give the agent control server (`crates/agent_threads/src/control.rs`) the best
available answer to one question: which live Agent Thread, if any, does this
connecting process belong to?

The answer must be exact when the signals identify one thread. The server must
return `caller-not-recognized` when the signals are missing or ambiguous. This
rule must also apply to a CLI that routes tool commands through a shared,
long-lived daemon instead of forking its own child process.

## Background

`control.rs` and
`docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md` already
describe a two-step caller resolution:

1. **Process ancestry.** The server reads the true peer process ID of the
   connecting socket from the operating system (`LOCAL_PEERPID` on macOS,
   `SO_PEERCRED` on Linux, the named-pipe client PID on Windows). It then walks
   the parent chain for a PID it already tracks as a live terminal. This step
   needs no cooperation from the agent CLI. It is the strong signal.
2. **Working-directory fallback.** Codex runs tool commands through an
   already-running daemon (`codex app-server`). No ancestor PID is one that
   Flint tracks, so step 1 finds nothing. The server then matches the connecting
   process's own working directory against each tracked thread's tied worktree
   root. When more than one thread ties to that root, the server tries to break
   the tie with the caller's ancestry process names against each candidate's
   `kind_id`.

This design is agent-neutral by construction. Unit tests in `control.rs` already
model the Codex daemon topology
(`resolve_caller_thread_falls_back_to_cwd_when_ancestry_has_no_match`).

## The problem

The tie-break by process name works only when the ambiguous candidates are of
*different* kinds. When two threads of the *same* kind tie to the same worktree,
the caller's process name matches every candidate equally. The server correctly
refuses to guess (`resolve_caller_thread_refuses_an_ambiguous_cwd_match`).

This is the normal case for a user who runs more than one Codex thread in one
worktree. This machine's own Flint log
(`~/.local/share/flint/logs/Flint.log`) shows it:

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

Both `codex` candidates have `kind_id == "codex"`, and the caller's ancestry
contains `"codex"` whichever thread it belongs to.

## Two independent changes

Change A and Change B have no dependency on each other. Ship them separately.

## Change A: remove the stale environment gate

`crates/agent_control_skill/skills/flintctl/SKILL.md` gates all use of
`flintctl` on `FLINT_AGENT_THREAD=1` in the caller's own environment. Flint sets
this variable once, at launch, for every local Agent Thread process
(`apply_control_skill_environment` in `store.rs`).

A CLI that forks its own tool subprocess inherits a current value. Codex does
not. Its value comes from whenever the shared daemon last started, which can
predate the current thread or be missing. The gate can therefore hide a live
Agent Thread from the skill even when the server would resolve the caller.

This is a confirmed defect, and it needs nothing from Change B. Ship it alone.

Remove `FLINT_AGENT_THREAD` completely. Put no environment variable in its
place. Flint also sets `TERM_PROGRAM=flint`, `TERM_PROGRAM_VERSION`, and
`ZED_TERM=true` in every terminal (`insert_flint_terminal_env` in
`crates/terminal/src/terminal.rs`), but those carry the same defect and must not
become the new gate. A daemon-routed tool process inherits the daemon's
environment, so such variables fail in both directions. They are absent when the
daemon started outside Flint although the caller really is in a thread, and they
persist after Flint quit although the caller is not.

The skill uses a two-stage gate instead.

**Stage 1: a cheap negative check.** Test that the release-matched marker exists
and that the control endpoint exists — the socket on Unix, the named pipe on
Windows. Neither test reads the caller's environment, so no stale daemon can
affect the answer. The marker proves Flint is installed. The endpoint proves
Flint is running, because the control server creates it at start and removes it
on quit. If either is absent, continue the task without Flint.

Stage 1 earns its place on cost. When the server cannot resolve the caller, the
CLI sleeps through `RETRY_BACKOFFS` of 250 ms, 500 ms, and 1000 ms before it
reports `caller-not-recognized` (`crates/agent_control_cli/src/lib.rs`). An agent
that never runs in Flint would pay about two seconds on every skill activation.

**Stage 2: the probe.** Run `flintctl terminal current --json`. It is the only
authoritative answer, and one call answers two questions:

- The call succeeds. The caller is in a live Flint terminal and can use the
  terminal commands.
- The response also has `is_agent_thread: true`. The caller is an Agent Thread
  and can additionally use `thread retie` and `thread create`.

A connection failure, a protocol mismatch, or `caller-not-recognized` means the
skill continues without any Flint control. `is_agent_thread: false` is not such a
case. It withdraws only the thread commands.

Also correct the skill's frontmatter description. It currently reads "Outside a
Flint Agent Thread, continue without Flint control commands", which understates
what an ordinary Flint terminal caller may do.

## Change B: session-ID tie-break

**Rule:** never treat a value a local process can freely claim about itself as
sufficient identity on its own. Use it only to break a tie after the operating
system has supplied the peer PID and Flint has narrowed the candidates by working
directory and agent kind. The session ID is a disambiguation signal. It is not an
authorization token.

The daemon fallback stays weaker than terminal-PID ancestry. A same-user process
can choose its working directory, process name, and environment. The local socket
remains user-scoped and the server still requires a matching live local Agent
Thread, but the fallback cannot prove the caller descended from a Flint PTY. This
is an accepted limit. The server must not describe this fallback as equivalent to
the ancestry match.

### The step

Add one step, used only when the working-directory step still leaves more than
one candidate of the *same* kind:

3. **Session-ID tie-break.** Codex sets `CODEX_THREAD_ID` in the environment of
   the tool-call process, so a `flintctl` run from that process inherits it.
   Flint already stores a Codex session ID per thread for history resume
   (`AgentThreadMetadata::resumed_session_id`, populated by
   `attach_discovered_session_id` in `store.rs`). The server keeps only the
   candidate whose stored session ID equals the caller's. Exactly one match
   resolves the caller. Zero or more than one match stays unresolved.

### The server reads the peer's environment

The server reads the connecting process's own environment through `sysinfo`'s
`Process::environ()` and `ProcessRefreshKind::with_environ`. `flintctl` does not
send the value.

The deciding reason is which side knows what to look for. By this point the
server has narrowed the candidates to one kind and knows that kind's
`caller_session_env_var`. A generic `flintctl` binary is not told in advance
which kind it will be matched against, so it cannot know which variable to read.
It would have to send every known kind's value speculatively, which grows the
request surface with each new kind and leaks values the server does not need.
Reading server-side also keeps the existing sentence in the 2026-08-21 design
literally true: "No token supplied by the client is treated as caller identity."

The cost is real and this design accepts it. Process-environment access can fail
on every supported platform because of permissions, process exit, or operating
system restrictions. An unavailable, missing, non-Unicode, or empty value is no
match. The server must not guess after a failure.

### Fail-closed rules

1. Exactly one cwd-and-kind candidate resolves as today. The session ID is not
   needed.
2. With several same-kind candidates, every one must already have an attached
   session ID. If one or more is still unassociated, return
   `caller-not-recognized`. Do not compare only the associated candidates.
   `attach_discovered_session_id` infers the ID from history files instead of
   receiving it from Codex, so an unassociated candidate can still be the true
   caller.
3. Exactly one attached ID equal to the caller's session ID resolves that
   candidate.
4. No match, or more than one match, returns `caller-not-recognized`.

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
any remaining candidate has no attached session ID?   [NEW]
  |  yes -> unresolved
  |  no
  v
caller's session ID -> candidate's stored session ID   [NEW]
  |  exactly 1 match -> resolved
  |  0 or 2+ matches
  v
caller-not-recognized (unchanged, fail closed)
```

### Generalizing beyond Codex

Add one optional field to `AgentKindDefinition`
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

Only Codex sets this today (`Some("CODEX_THREAD_ID")`). A kind whose CLI forks
its own tool subprocess never needs it, because step 1 already answers for it.

### Remote callers

Remote development uses this same resolution order on the remote host, as
`docs/superpowers/specs/2026-08-22-flintctl-remote-dev-design.md` requires. That
document owns the mechanism; this section states only what Change B must supply.

The remote server reads the peer PID, ancestry, working directory, process
names, and the configured session variable from the true remote peer process.
`AgentKindDefinition` is local application state, so local Flint must send the
kind, the kind's `caller_session_env_var`, the attached session ID, and the tied
worktree root as connection-bound metadata on the matching remote PTY
registration. The tied worktree root is needed because the working directory
captured at PTY registration does not follow a later retie.

All paths in this metadata are remote-host paths. The remote server must not
scan local paths or infer identity from a client claim. Local Flint sends the
metadata when it binds the registration and updates it after a retie or after
session discovery attaches an ID. Each update is bound to the authenticated
project connection and the current registration generation; the server rejects a
stale generation and discards the metadata when the PTY, connection, or
generation ends. Until the metadata arrives, ambiguous same-kind remote callers
stay unresolved. Direct and Tunneled use the same exchange, which does not change
how either executable starts or reaches its provider.

Fresh remote Codex threads hit the same limit as local ones. 08-22 additionally
requires extending the background discovery loop to remote projects through the
existing remote history index over the authenticated project connection, using
the same project, kind, launch-time, and already-bound rules as local discovery.
A remote history or connection failure leaves the thread unassociated and retries
on the next bounded interval.

## What this does not fix

Codex has `session_id_flag: None` (`agent_threads.rs`), so Flint cannot assign a
session ID at launch. The ID arrives only from the background history scan, which
runs every 30 seconds and can itself stay ambiguous when several new sessions
appear in one project.

Rule 2 above requires an attached ID on every same-kind candidate. Therefore two
*fresh* concurrent Codex threads in one worktree — the case in the log above —
stay unresolved for at least 30 seconds, and permanently if discovery stays
ambiguous. Change B fixes the case where both threads are old enough to have been
discovered. It does not fix the first 30 seconds.

Measure this before you commit to Change B. If most real reports are fresh
threads, the tie-break buys less than its cost, and the better answer is a
supported Codex-to-Flint session registration channel. The current official Codex
documentation does not establish a launch option that lets Flint assign a session
ID to a fresh session, so this design does not invent one.

The skill can retry a not-ready response through the existing CLI retry deadline,
but it must not wait without a bound.

## Non-goals

- Giving a daemon-routed caller the same strong ancestry proof as a direct child
  of a Flint PTY.
- Solving ambiguity for a daemon-routed kind that has no per-session environment
  variable on its tool-call processes. Such a kind stays unresolved when two of
  its threads share a worktree, the same as today.
- Guaranteeing control for concurrent fresh Codex threads. See "What this does
  not fix".
- Inventing a Codex session-registration channel that Codex does not support.

## Open questions

- Does every supported Codex version put `CODEX_THREAD_ID` in every tool-call
  process? The implementation must fail closed when it is absent.
- Can a supported future Codex hook or app-server protocol report the session ID
  together with a Flint-provided terminal identity? That would remove the
  fresh-session limit and make Change B unnecessary.

## Verification

For Change A:

- Skill tests for a successful Agent Thread probe, an ordinary-terminal response
  that keeps the terminal commands and withdraws only the thread commands, a
  missing marker, a missing endpoint, a connection failure, a protocol mismatch,
  and `caller-not-recognized`.
- A test that stage 1 stops before it runs `flintctl` when the endpoint is
  absent, so a caller outside Flint never pays the retry backoff.
- A test that a stale `TERM_PROGRAM=flint` or `ZED_TERM=true` in the caller's
  environment changes no decision, in either direction.
- Remove the production and test references to the exact variable
  `FLINT_AGENT_THREAD`, which appear only in `store.rs` and `SKILL.md`. Do not
  remove `FLINT_AGENT_THREAD_ID` in `crates/agent_threads/src/remote_process.rs`.
  That is a different variable, it carries the remote lifecycle guard that stops
  orphaned remote agent processes, and a substring search matches it too.

For Change B, extend the existing `control.rs` suite with:

- Two same-kind candidates disambiguated by a matching `caller_session_env_var`
  value on the connecting test process.
- The same setup with no matching stored ID, and with an ID matching more than
  one candidate. Both stay unresolved.
- A kind with no `caller_session_env_var` still refuses to disambiguate
  (regression cover for `resolve_caller_thread_refuses_an_ambiguous_cwd_match`).
- A matching peer session ID while another same-kind candidate is still
  unassociated stays unresolved (rule 2).
- A caller session ID that is absent, empty, or an invalid operating-system
  string stays unresolved.
- Environment access that returns no data stays unresolved, and a peer process
  that exits during inspection stays unresolved.
- An end-to-end control test with a detached process, two live Codex-kind
  terminals in one worktree, and distinct attached session IDs. Verify that
  `terminal current --json` selects the matching terminal.
- Verify on Linux, macOS, and Windows that an environment-read failure counts as
  no match. A manual platform check can supplement this but cannot replace the
  detached-process regression test.
- A session mismatch produces a log line and an unresolved result, never a panic
  or a process exit, and the log contains no session ID value.

For remote callers (see "Remote callers"; 08-22 owns the transport tests):

- Remote bridge tests for connection-bound kind, session-variable name, session
  ID, and tied-worktree metadata; metadata updates after retie and after session
  discovery; linked-worktree repository matching; same-kind disambiguation;
  stale-generation rejection; disconnect cleanup; and missing-metadata
  fail-closed behavior.
- Remote session-discovery tests for fresh Codex threads, reconnect, history
  lookup failure, already-bound IDs, and ambiguous concurrent sessions.

## Implementation order

0. **Verify the two assumptions before writing code.** First, capture
   `CODEX_THREAD_ID` from a live Codex tool-call process and compare it with the
   `session_meta.payload.id` of that session's rollout file (`parse_summary` and
   `summary.id` in `crates/agent_history/src/codex.rs`). Sample rollout files
   show that `payload.id` and `payload.session_id` hold the same UUID, which
   supports the assumption but does not prove it. If the values name different
   things, the tie-break never matches, it fails closed and silently, and nothing
   reports the defect. Second, confirm that the macOS and Windows production
   builds can read a peer process environment at all. If either check fails, stop
   and ship only Change A.

Change A, shippable on its own:

1. Update `SKILL.md` to the two-stage gate, and correct its frontmatter
   description so it does not restrict the terminal commands to Agent Threads.
2. Remove `FLINT_AGENT_THREAD`, `apply_control_skill_environment`, its launch
   path call, and its unit test from `store.rs`.
3. Update
   `openspec/changes/add-flintctl-terminal-control/specs/terminal-agent-threads/spec.md`
   and its related tasks so they require the probe instead of the environment
   gate.

Change B, only after step 0 passes:

4. Add `caller_session_env_var` to `AgentKindDefinition`; set it for Codex only.
5. Add the tie-break to `resolve_caller_thread` in `control.rs`, gated on more
   than one same-kind candidate remaining after the existing steps. Add a
   diagnostic **log line only** at the comparison point, so a future divergence
   between the two identifiers is visible rather than silent. Do not assert.
   The caller's value is caller-controlled and unauthenticated, so a missing,
   stale, empty, or false value is a normal input, not a program error; an
   assertion would let any local process stop the control server. Redact the
   values in the log — record the candidate count, whether each side was present,
   and whether the formats agreed, not the session IDs themselves.
6. Add the Change B tests listed under Verification.
7. Extend the remote route: send the connection-bound identity metadata with the
   remote PTY registration, update it after retie and session discovery with
   generation checks, and extend background session discovery to remote projects.
   See "Remote callers".
8. Add the new step to the "Caller resolution and access boundary" section of
   `docs/superpowers/specs/2026-08-21-flintctl-terminal-control-design.md`, so the
   two documents stay consistent. That section already carries the weaker-fallback
   caveat; only the new step is missing.
