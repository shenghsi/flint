## Context

`terminal_threads.rs` already proves the core mechanics: launching a
configured command in a center terminal, tracking title/bell/attention via
`TerminalView`/`TerminalEvent`, and focusing an existing tab instead of
duplicating one. What's missing is (1) a docked, always-visible surface
instead of a full-pane tab the user must explicitly open, (2) awareness of
sessions that exist on disk but aren't open in this app session, and (3)
scoping to the current project instead of one global cross-project list. Both
`claude` and `codex` CLIs already persist resumable session history
(`claude --resume <id>`, `codex resume <id>`) — Flint should surface that
instead of only tracking its own in-memory state.

## Goals / Non-Goals

**Goals:**

- Docked sidebar panel, scoped to the current workspace's project (matched
  against worktree roots)
- Merge live and historical threads per agent kind, with simple, explainable
  dedup rules
- Resume a historical thread, optionally with agent-specific extra flags
  (data-driven per kind)
- Keep the registry code-level extensible (no user-facing custom-agent
  config)
- Resolve agent home directories correctly across macOS, Linux, and Windows
- Work correctly for remote (SSH) projects, where agent terminal threads and
  their persisted session history live on the remote host, not the local
  machine

**Non-Goals:**

- ACP integration, a native agent panel, or any dependency on the
  agent/copilot stack removed in `remove-ai-features`
- User-configurable custom agent definitions via settings.json
- Perfect dedup for multiple concurrent same-kind threads in the same project
  (documented heuristic limitation, see Decisions)
- Incremental/paginated "Show more" (v1 reveals everything once expanded)
- Persisting fold/unfold state across restarts

## Decisions

### New crate instead of extending `terminal_view`

`agent_threads` owns the registry, history providers, dedup, and panel UI;
`terminal_view` keeps only what's intrinsically coupled to its own types
(`TerminalView`, `TerminalEvent`). This mirrors the existing precedent of
`project_panel`/`git_panel`/`outline_panel` each being dedicated crates.

Alternative considered: extend `terminal_threads.rs` in place. Rejected
because it would make `terminal_view` own three separate concerns (terminal
rendering, disk-history parsing for two different on-disk formats, and panel
rendering) that don't otherwise relate to it.

### Resolve the project's host before reading agent config directories, then read through `project.fs()`

Terminal threads for a remote (SSH) project are spawned on the remote host
(`crates/project/src/terminals.rs`'s `is_via_remote`/`create_remote_shell`),
so a remote project's `~/.claude`/`~/.codex` directories live on that remote
host, not the local machine. `project.fs()` already handles this
transparently: for local projects it's the real local filesystem, for
remote projects it's a proxy that talks to the remote `headless_project`
server. Reading through it (instead of raw `std::fs`/`smol::fs`) means one
code path is correct for both cases, and it stays consistent with this
codebase's `FakeFs`-based testing convention.

`paths::home_dir()` only resolves the *local* machine's home directory, so
it can't be used as-is to find the remote host's `$HOME`. Home-directory
(and `CLAUDE_CONFIG_DIR`/`CODEX_HOME` override) resolution branches on the
project's connection:

- **Local project**: `paths::home_dir()` plus local `std::env::var(...)`
  overrides — synchronous, no round trip.
- **Remote project**: reuse `crates/project/src/environment.rs`'s existing
  remote-environment resolution (`remote_directory_environment`, already
  used to set up terminal/task environments) to fetch the remote host's env
  map, read `$HOME` and any `CLAUDE_CONFIG_DIR`/`CODEX_HOME` overrides from
  it. This is async and can fail (e.g. connection dropped); on failure, log
  the error and treat that agent kind's historical scan for this project as
  unavailable rather than empty — the panel distinguishes "no history" from
  "couldn't scan" so a connection hiccup doesn't look like an empty history.

Alternative considered: scope remote-project support out of v1 entirely
(local-only, as originally drafted). Rejected once traced through
`terminals.rs` — remote projects are an existing, supported Flint workflow,
and silently showing wrong-host (or no) history for them would be an actual
correctness gap rather than a reasonable cut corner.

### Two-tier dedup instead of a single robust matching scheme

Exact session-id match handles the common, fully-known case (explicit
resume, where the id is known at launch time). A same-kind/same-project/
launch-time heuristic handles brand-new threads whose session id isn't known
until the CLI writes it to disk. A fully robust scheme (e.g. watching for and
claiming newly-created session ids as they appear) was considered and
rejected as disproportionate complexity for an edge case — multiple
concurrent same-kind threads in the same project — that, when it does occur,
only produces a transient duplicate row rather than incorrect behavior.

### Bounded Codex rollout scan instead of a full-history scan

Codex has no single file with both cwd and title, so matching requires
opening rollout files to read their `cwd`. Capping the scan to the most
recent ~200 rollout files (sorted by filename, which embeds an ISO
timestamp, so lexical sort is chronological) bounds this regardless of total
history depth.

Alternative considered: scan every rollout file. Rejected because total
history size is unbounded and the cap costs nothing for typical usage —
recent threads are what matter for resuming.

### One global visible-thread cap, not per-kind

`agent_threads.max_visible_threads_per_agent` applies uniformly to every
registered agent's section. A per-kind override was considered and rejected
as unnecessary configuration surface until a concrete need for differing
caps per agent shows up.
