# Worktree-Tied Agent Threads Panel (fresh implementation off `main`)

## Context

The branch `agent-threads/worktree-identity-stage1` built a large, dual-mode
"worktree grouping" redesign of the Agent Threads panel (group headers, collapsible
worktree sections, a `panel_grouping` setting, a full local/Windows/remote-SSH
agent-control broker, etc). The user doesn't like that panel view and wants to start
over, on a fresh branch off `main`, with a much smaller design:

- Keep the existing (main's) agent-kind-grouped panel layout as-is — no group headers,
  no dual mode, no settings toggle. This is the **only** behavior.
- The one real change: every thread gets an **explicit worktree tie**, defaulting to
  whichever worktree its own workspace represents ("main worktree" in the common case).
  A workspace's panel only shows threads tied to *that* workspace's worktree — so
  switching worktree (via the existing title-bar picker) naturally shows a different
  set of threads per agent.
- Confirmed with the user: no cross-worktree *browsing* surface is being added — this
  is not about letting the user go look at other worktrees' threads. Every thread a
  panel renders belongs to that worktree by its tie, and a retie **moves the terminal
  to the tied worktree's workspace** rather than leaving a label pointing elsewhere, so
  tie and owning workspace are always equal — see "Retie moves the terminal" below.
  `focus_thread` therefore stays exactly as it is on `main`.
- The panel highlights the row whose terminal is the currently active item, derived from
  the workspace's own active item rather than from separate click-selection state.
- Also confirmed in scope: **agent-initiated worktree creation/tying** — a thread's own
  CLI process (Codex, Claude Code, etc., running in its terminal) can ask Flint to (a)
  re-tie itself to a different/new worktree, or (b) create a brand-new worktree and
  start a new sibling thread tied to it. Scoped to **local-only** for this pass —
  Windows/remote-SSH transport is an explicit non-goal, to avoid re-growing the
  complexity that made the abandoned branch so large.

All code below targets a **new branch off `main`** — nothing from the current branch is
reused; line numbers cited are from `main`, verified directly via `git show main:<path>`
during planning (not the diverged working tree).

## Baseline already on `main` (no changes needed here, just context)

- `AgentThreadsPanel` is one entity per `Workspace` (registered via `cx.observe_new` in
  `agent_threads.rs`'s `init()`), holding `workspace: WeakEntity<Workspace>`.
  `Render::render()` (`panel.rs:~1820`) computes `project_worktree_roots(project, cx)`
  from its own workspace's project and loops one `render_section` per agent kind
  (`panel.rs:~1088`, 251 lines) — no grouping axis besides agent kind.
- `render_row`/`render_live_row`/`render_historical_row` (`panel.rs:~1339-1543`) are
  agent-kind-aware only, not grouping-aware — reusable unchanged.
- `AgentThreadMetadata` (`store.rs:44-56`) has `project_root: PathBuf`, set straight
  from the terminal's spawn cwd — no explicit worktree concept.
- `live_threads_for_project(kind_id, project_roots)` (`store.rs:498-514`) does a plain
  path-equality filter against `project_root`.
- Only *resumed* threads persist across restarts, via `AgentThreadSessionRestoreRecord`
  (`store.rs:274-282`, `workspace_id`-scoped) in `db::kvp::KeyValueStore`.
- `MultiWorkspace` (`crates/workspace/src/multi_workspace.rs`) already holds many
  retained `Entity<Workspace>` (one per open worktree) with `activate()` to switch the
  foreground one; the title-bar worktree picker already routes through
  `find_or_create_local_workspace_with_source_workspace` (`multi_workspace.rs:~1299`),
  which finds-or-creates a *separate* workspace per worktree path rather than mutating
  the current one. `Workspace::root_paths(cx)` / `Worktree::abs_path()` is the stable
  per-worktree path identity already used everywhere (title bar, worktree picker,
  workspace matching) — reuse it rather than inventing a new worktree identity scheme.
- `git_ui::worktree_service::create_worktree_workspace` (`worktree_service.rs:637-654`)
  already creates a new linked worktree and opens a **background** (non-activated)
  workspace for it — its own doc comments already describe this as intended for "the
  `create_thread` agent tool," confirming it's the right function to reuse rather than
  reimplement, even though no caller of it exists yet.
  - **Correction to note**: `feature_flags::CreateThreadToolFeatureFlag`
    (`flags.rs:66-79`) is *not* usable prior art for this — despite the similar name,
    it gates a different, never-implemented Zed-native-assistant tool ("the agent
    panel sidebar") that has no relation to Flint's terminal-hosted `agent_threads`
    CLIs; there is no `assistant_tools`/native-agent crate in this codebase at all.
    Don't reuse it — and don't add a replacement flag either; use a real setting
    instead, for the reasons worked through in Stage 2.
- `launch_seeded_thread(workspace, kind, initial_prompt, window, cx)`
  (`store.rs:856-877`, `pub(crate)`) already does route-aware launch +
  `seed_launch_command_with_prompt` (`agent_threads.rs:93-113`) — reuse for the "new
  thread" side of agent-initiated creation. Note today it silently discards a `false`
  (failed-to-seed) return — worth fixing when adding a caller that needs to report
  failure back to the calling CLI process.

## Stage 0 — Prerequisite: a production git-readiness signal (IMPLEMENTED)

**Implemented differently from the original plan below, after the original plan's own
premise turned out to be wrong.** The plan was to add a new `initial_scan_complete` bit
derived from `Repository.scan_id`, on the belief that the existing `scan_id > 2`
convention in `handle_subscribe_self` marked "initial snapshot loaded". A real
`#[gpui::test]` against a `FakeFs` repo disproved this immediately:
`Project::git_scans_complete(cx)` (the trusted, extensively-used-in-tests reference
definition of "git state has loaded") resolves at `scan_id == 2`, not `> 2` — so the
borrowed threshold would have permanently waited one tick too long, some tests confirming
readiness before my check would.

Rather than chase a corrected magic number, traced why `git_scans_complete` was
`#[cfg(feature = "test-support")]` in the first place: every one of its ~50 call sites
across the codebase is a test, and its body (`Project::worktrees`, `Worktree::scan_complete`,
`Project::repositories`, `Repository::barrier`) has **zero** test-only dependencies. The
gate was a leftover visibility default, not a technical requirement. Fix: **removed the
`#[cfg(feature = "test-support")]` attribute from `Project::git_scans_complete`
(`project.rs:4821`)**, making the existing, already-correct, already-widely-tested
implementation directly callable from production `agent_threads` code. Verified the full
`project` test suite (`--lib` and the 226-test `--features test-support` integration
suite) still passes unchanged.

This covers the *async, awaitable* need (restore routing, which awaits readiness once
before resolving ties). The *synchronous, per-render* need (`TieResolution.git_ready`) is
still open and left to Stage 1: `git_scans_complete` returns a `Task<()>`, not a
poll-able flag, so the panel will cache "has resolved once" in local state via a
one-time-per-repository spawned task + `cx.notify()`, the standard GPUI pattern — not a
new primitive on `GitStore`/`Repository`.

<details>
<summary>Original plan (kept for context; superseded by the above)</summary>

Both tie-resolution consumers depend on knowing whether git state has loaded, and no
production API provides that today (`Project::git_scans_complete` is
`#[cfg(feature = "test-support")]`, `project.rs:4821`). Add an `initial_scan_complete`
bit plus a corresponding `GitStoreEvent` to `GitStore`/`Repository`, so "loaded, and this
repo has no linked worktrees" is distinguishable from "not loaded yet". Details and
rationale in the "Ordering hazards" note under Stage 1. This lands first — the deleted-
worktree fallback cannot be implemented correctly without it.

</details>

## Stage 1 — Explicit worktree tie + single grouping mode + persistence

**`store.rs`**
- Add `tied_worktree_root: PathBuf` to `AgentThreadMetadata`, alongside (not replacing)
  `project_root` — they diverge in two real cases: `terminal.working_directory` set to
  `current_file_directory` (resolves to a subdirectory, not a worktree root), and after
  a re-tie (capability below), where the OS-level cwd never moves.
- Add `fn resolve_tied_worktree_root(workspace: &Workspace, cx: &App) -> Option<PathBuf>`:
  mirror `TitleBar::effective_active_worktree`'s logic (`title_bar.rs:403-419`) — prefer
  the worktree owning `project.active_repository(cx)`, else `visible_worktrees(cx).next()`
  ("default to main worktree"), else `None` for a worktree-less project.
- Compute it in `spawn_thread_task_inner` (`store.rs:~1815-1929`) right alongside the
  existing `cwd` resolution, and thread it into `register(...)`. This one call site
  covers every existing launch path (fresh launch, resume, credential commands, seeded
  launch) uniformly, and also transparently handles the "new worktree" creation case
  (item below) with zero special-casing, since a thread launched into a genuinely new
  `Workspace` entity naturally resolves its own tie.
- **Retie splits into async orchestration + a synchronous store commit.** One
  `&mut Context<AgentThreadStore>` method cannot do this job: resolving-or-creating the
  destination workspace is asynchronous, and both it and `workspace::move_item` need a
  `Window`/`WindowHandle<MultiWorkspace>`, which a store context does not carry.
  - `control.rs` owns the window-aware `async fn retie_thread(...)`: resolve destination
    workspace → checked item move → store commit → persist. It is the only public entry
    point, and it does not report success until every step reaches a defined success
    state (see "Commit ordering" below).
  - `AgentThreadStore::commit_retie(&mut self, terminal_item_id, ResolvedTie, destination_workspace, cx) -> Result<()>`
    is the synchronous remainder: update `entry.metadata.tied_worktree_root`, update
    `entry.workspace` to the destination, `cx.notify()`, emit
    `AgentThreadStoreEvent::ThreadUpdated { kind_id }`. No IO, no workspace resolution.

### One effective-tie resolver, used at every query site

A raw field comparison is not sufficient once the deleted-worktree fallback exists, and
`live_threads_for_project(&self, kind_id, project_roots)` (`store.rs:498`) takes neither
`cx` nor any repository / project-group / open-workspace input, so it cannot evaluate the
liveness conditions itself. Define exactly one resolver and hand every query site the
same inputs, rather than scattering the logic:

```rust
/// Everything tie resolution needs, gathered once by the caller.
struct TieResolution {
    /// Live worktree roots, per repository (condition 1).
    worktree_roots_by_repo: HashMap<RepositoryId, HashSet<PathBuf>>,
    /// Roots with a workspace currently open (condition 2). Empty for restore.
    open_workspace_roots: HashSet<PathBuf>,
    /// False until git state has loaded; suppresses the fallback entirely.
    git_ready: bool,
}

impl TieResolution {
    /// `None` when the tie is dangling and no in-repo fallback applies.
    fn effective_tie(&self, tie: &ThreadTie) -> Option<PathBuf>;
}
```

- The panel builds one per render (conditions 1 and 2, plus `git_ready`) and passes it
  in: `live_threads_for_project(kind_id, project_roots, &resolution)`.
- Restore builds one with `open_workspace_roots` empty and condition 1 only, per
  "Ordering hazards" below.
- Historical filtering uses the same resolver over the same inputs as the panel.

`project_worktree_roots` still supplies the candidate root *list* for a multi-root
workspace as it does today; what changes is that each root is compared against the
thread's *effective* tie rather than a raw field. A single resolver is what stops the
Stage 1 filter and the fallback section from specifying incompatible implementations.

### Retie consistency (resolves the ownership, duplicate-history, and restore-routing gaps)

A retie has to stay coherent in three places: which workspace actually owns the
terminal, which panel's *historical* list the session shows up in, and where it restores
after a restart. Terminal ownership is handled by reparenting (below); the other two need
a persisted tie — but the obvious key does not work.

**Session ids cannot be the primary key.** `AgentThreadMetadata.resumed_session_id` is an
`Option`, and for fresh Codex/OpenCode threads it stays `None` until asynchronous
discovery fills it in via `attach_discovered_session_id` (`store.rs:554-568`, which
explicitly no-ops if one is already set). A `retie-thread` request can easily arrive
before that. Session ids are also only unique *per provider*, not globally. So:

```rust
// New db::kvp namespace, e.g. "agent-thread-session-tie".
// Key:   (kind_id, session_id)   -- provider-scoped, not bare session_id
// Value: { tied_worktree_root, repo_main_root: Option<PathBuf> }
```

- **In-memory is the source of truth while the thread lives**, keyed by
  `terminal_item_id` — the key the store already uses, always available, never racy.
  This alone drives the live query, so a retie takes effect in the panel immediately
  regardless of session-id discovery.
- **A pending tie is held in memory** when the thread has no session id yet, and is
  migrated into the persisted table when `attach_discovered_session_id` later learns the
  provider id. That hook is the natural migration point and already exists.
- **If discovery never resolves, the tie is never persisted, and that is correct** — not
  a silent loss. `snapshot_records_for_workspace` already hard-filters on
  `let session_id = thread.resumed_session_id?` (`store.rs:~2127`), so a thread with no
  session id is not restorable on `main` today either. The retie is fully honored for
  the life of the process and dies with it, which matches the thread's own durability.
  The control response must therefore not claim durability it does not have — see
  "Commit ordering".

**Retie moves the terminal** (so tie and owning workspace never diverge). Rather than
leaving the terminal in its original workspace and treating the tie as a label that
points elsewhere, `retie_thread` reparents the terminal item into the tied worktree's
workspace. This removes the mismatch as a state the rest of the design has to tolerate:
`entry.workspace` always *is* the tied workspace, so `focus_thread` needs no
cross-workspace activation and stays byte-for-byte as it is on `main`.

The machinery already exists:

- `workspace::move_item(source_pane, destination_pane, item_id, index, activate, window, cx)`
  (`workspace.rs:9184`) removes the item from the source pane and adds it to the
  destination. It operates on `Box<dyn ItemHandle>` and takes `&mut App` rather than a
  workspace-scoped context, so nothing structurally ties it to a single workspace.
- `TerminalView` already implements `Item::added_to_workspace`
  (`terminal_view.rs:1911-1931`) — the hook whose entire purpose is "this item just
  moved to a different workspace"; it even logs *"Updating workspace id for the
  terminal, old: … new: …"*.

**Moving the tab is not enough — five pieces of state must move together.** Assigning
`TerminalView.workspace` in `added_to_workspace` does *not* fix the handles already
captured elsewhere. All of the following must be updated, or `focus_thread`,
`create-thread --worktree current`, navigation, and terminal-originated events keep
operating on workspace A after the tab has visually moved to B:

1. The pane item itself, via a **checked** move (below).
2. `ThreadEntry.workspace` → the destination workspace entity (done by `commit_retie`).
3. `TerminalView.workspace` (`terminal_view.rs:187`) — set at construction, never
   refreshed.
4. `TerminalView.project` (`terminal_view.rs:188`) — must move with the workspace; a
   stale project is what `:1671` and `:1839` resolve against.
5. `TerminalView._terminal_subscriptions` (`:215`, rebuilt at `:1166`) — must be
   **rebuilt**, because `subscribe_for_terminal_events(terminal, workspace, …)`
   (`:1200-1205`) takes the weak workspace *by value into a `move` closure*. Reassigning
   the field afterwards leaves the old handle captured inside the live subscription.

Since 3-5 are private fields, this needs a small `pub fn reparent(&mut self, workspace,
project, window, cx)` on `TerminalView` that sets both handles and rebuilds the
subscriptions in one place, called from `added_to_workspace`. Note that hook's existing
body is guarded by `if self.terminal().read(cx).task().is_none()` and agent threads *are*
task terminals — that DB-rewrite branch stays correctly skipped (agent threads restore
via this design's own session mechanism, not `TerminalDb`), but it means the hook does
nothing for our case today, so the reparent call must sit outside that guard.

**A checked move is required.** `workspace::move_item` (`workspace.rs:9184-9192`) returns
`()` and silently returns early when the source pane no longer contains the item ("Tab was
closed during drag"). Committing the tie after such a no-op would recreate exactly the
ownership divergence this design exists to prevent. Use a checked wrapper that verifies
the item is present in the source before, and present in the destination after, and
returns `Result`.

**Commit ordering.** The control response must describe what actually happened:

1. Resolve-or-create the destination background workspace (Stage 2's
   `find_or_create_background_local_workspace`). Fail → return an error, nothing changed.
2. Checked move into its active pane with `activate: false` — the caller asked for a
   retie, not to be yanked into another worktree. Fail → return an error, nothing
   changed.
3. `TerminalView::reparent` + `commit_retie` (in-memory, infallible once 1-2 succeeded).
   The retie is now fully in effect for this process.
4. Persist the tie — **awaited, not detached** — so the response can distinguish
   `persisted` from `in_memory_only`. A detached, log-only KVP write cannot back a
   truthful structured success. A thread with no session id yet legitimately reports
   `in_memory_only` (see the keying discussion above); a genuine write failure is
   reported as such rather than as success.

> **Retie does not move the process.** Reparenting is Flint-side only: the running CLI's
> cwd is unchanged, so its relative commands, file writes, and tools all still operate in
> the *original* worktree. `retie-thread` changes Flint's ownership, panel grouping,
> history filtering, and restore routing — nothing about the process. The command's own
> help text and its JSON response must say this plainly, and an agent that wants to work
> in the new worktree must `cd` there itself. Leaving this implicit invites an agent to
> believe it is operating in B while it is still modifying A. This is also exactly why
> the historical-rows and restore-routing problems below need solving independently.

**Historical rows (fixes the duplicate-session risk).** Two distinct problems here, and
filtering alone only solves the first.

*Suppressing the duplicate in A.* Each candidate row's `(kind_id, session_id)` is checked
against the tie table. A tie pointing elsewhere excludes the row from A's list entirely —
not merely left unmerged, which is what let a live-but-tied-elsewhere session render as a
false "resumable" row that would spawn a second process on click.

*Making the row appear in B — needs a scan change, not just filtering.* Because the
process cwd never moves, the provider keeps recording the session under root A, so B's
history scan never returns that session at all and there is nothing for a filter to admit.
Filtering cannot conjure a candidate it was never given. So: **each panel scans the union
of its project group's worktree roots**, then applies effective-tie resolution to decide
which of those candidates belong to it. Concretely, the `provider.scan(&legacy_host,
&project_roots)` call in `refresh_history_kind` (`panel.rs:~498`) is given the project
group's roots rather than only this workspace's, and the resulting rows are filtered by
effective tie.

That costs a wider scan per panel. It is the honest price of letting a session's home
change without its on-disk history moving, and the scan results are already cached per
kind behind `HistoricalState`, so the cost is per-refresh rather than per-render.
Alternative considered and rejected: resolving persisted tie ids through a
provider-specific index lookup — narrower, but requires every provider to grow a
by-session-id lookup that none of them has today.

**Restore routing.** `restore_threads_for_workspace`'s selection stops keying purely on
`record.workspace_id` and instead routes by each record's effective tie, so a thread
retied A→B restores whenever B reopens, independent of whether A ever does. Two rules,
split by record vintage:

- **New records always carry a resolved tie.** Every snapshot persists
  `AgentThreadMetadata.tied_worktree_root`, not just retie overrides — see the Persistence
  section. These route by effective tie.
- **Legacy records (`tied_worktree_root: None`) keep `workspace_id` routing**, exactly as
  today. They must *not* fall back to path-equality against `record.project_root`: that is
  not "unchanged behavior", it is a new and wrong behavior, because `project_root` is the
  launch cwd and can be a *subdirectory* under
  `terminal.working_directory = current_file_directory` — the very divergence this design
  cites as its reason for introducing an explicit tie. A subdirectory never equals a
  worktree root, so those threads would silently stop restoring.

### Deleted worktrees fall back to the main worktree

A tie whose worktree no longer exists must not strand its threads (which is what a naive
path-equality filter does — they match no panel and silently vanish). Effective-tie
resolution therefore has one more step, applied everywhere the tie is consumed (live
query, historical filtering, restore routing): **if the tied worktree is no longer a
worktree of its repo, the thread resolves to that repo's main worktree instead.**

- *Liveness check, no filesystem IO.* A tie counts as live when **either** of these holds:
  1. the path is in the repo's already-loaded in-memory worktree set —
     `main_worktree_abs_path()` (`git_store.rs:4530`) plus `linked_worktrees()`
     (`git_store.rs:4560`); **or**
  2. a workspace is currently open at that path.

  Both are in-memory checks, cheap enough for render. Condition 1 alone correctly
  distinguishes "deleted" from "merely not open" — a linked worktree present on disk with
  no workspace open keeps its threads rather than collapsing into main. Condition 2 exists
  for the degenerate case worked through below, and is what keeps the fallback from ever
  contradicting where a terminal actually lives.
- *Fallback target.* The project group's main worktree path, via
  `ProjectGroupKey::path_list()` (`project.rs:4962`) — by definition the main worktree
  path list, and already the identity used to group a repo's workspaces.
- *Scope the fallback to the tie's own repo — via a real repository lookup, not the
  project group key.* `ProjectGroupKey::path_list()` (`project.rs:4962`) returns a
  `PathList`, which in a multi-root workspace holds the main roots of *several*
  repositories; it cannot say which repo owns a given tie. Instead, resolve the owning
  repository at tie time by finding the repository in `git_store.repositories()`
  (`git_store.rs:2093`) whose worktree set contains the tie path, and record **that**
  repo's `main_worktree_abs_path()` (`git_store.rs:4530`):

  ```rust
  repo_main_root: Option<PathBuf>   // None when the tie is outside every repository
  ```

  `Option` is load-bearing: a `retie-thread` target outside every repository has no main
  worktree to fall back to, so once dangling it falls back **nowhere** and simply stops
  matching any panel. That is the intended outcome and it is what the Testing section
  asserts — the earlier prose, which implied such a tie would record some repo's root and
  surface there, contradicted that test and was wrong.
- *Lazy and display-only, not an eager rewrite at deletion time.* The stored tie is left
  untouched; only its resolution changes. This covers deletion performed outside Flint
  (`git worktree remove` in any terminal, or a plain `rm -rf`) identically to deletion
  through Flint's own picker, needs no `git_ui` → `agent_threads` deletion hook, and is
  self-healing: recreating the worktree at the same path silently restores the original
  association. An eager rewrite would achieve none of those and is explicitly rejected.
- *Note on in-Flint deletion.* `can_delete_worktree` (`worktree_picker.rs:465-467`)
  already refuses to delete a worktree that any workspace in the project group has open,
  so Flint's own delete path only ever affects ties whose workspace is closed. External
  deletion is the unconstrained case, which is the main reason the check must be lazy —
  and the reason condition 2 above exists.

**Why condition 2 (open workspace) is required, not belt-and-braces.** Without it, a
worktree deleted *externally while its workspace is still open* reintroduces exactly the
tie/ownership divergence that reparenting was introduced to eliminate. Walk it through
with main worktree `M`, linked worktree `X`, workspace_X open, thread `T` running in
workspace_X's pane and tied to `X`, and someone running `git worktree remove X` in a
terminal (Flint's own picker would have refused, but an external command does not care):

1. Git state refreshes and `X` drops out of `linked_worktrees()`.
2. Condition-1-only resolution calls the tie dangling and falls back to `M`.
3. `T`'s row now renders in **M's** panel and *vanishes from workspace_X's own panel* —
   while `T`'s terminal is still sitting in workspace_X's pane, still running.
4. Clicking that row in M's panel reaches `focus_thread`, whose `entry.workspace` is
   workspace_X. `main`'s `focus_thread` has no workspace-activation step, so it activates
   a pane item inside a non-foreground workspace: **the click does nothing visible.**

Condition 2 removes the whole sequence. The fallback then only ever applies to ties with
no open workspace — precisely the set where there is no terminal for a row to diverge
from — so the invariant "a row's panel is the workspace holding its terminal" holds
universally. Concretely:

- Delete `X` while workspace_X is open → tie stays `X`; the row stays in workspace_X's
  panel next to its terminal.
- Close workspace_X afterwards → nothing holds the tie live, the fallback applies, and
  `T` surfaces under `M`.
- Delete `X` with no workspace open (the only case Flint's picker permits) → falls back
  to `M` immediately.

*Rejected alternative:* reparenting `T` into `M`'s workspace when the fallback fires.
That yanks a running terminal out of a workspace the user may still be looking at,
without them having asked for anything.

> **Ordering hazards to respect during implementation.**
>
> *Git state not yet loaded.* The liveness check is only meaningful once git state has
> loaded. If restore routing runs first, the worktree set is empty, *every* tie looks
> dangling, and all threads wrongly collapse into the main worktree. **Resolved in
> Stage 0**: `Project::git_scans_complete(cx)` is now a plain production `Task<()>`
> (the `#[cfg(feature = "test-support")]` gate was removed — see Stage 0, it was a
> visibility leftover, not a technical requirement). Restore routing awaits it for the
> restoring workspace's project before resolving any tie for that workspace.
>
> Panel rendering needs a *synchronous* per-render `git_ready` rather than something to
> await. Since `git_scans_complete` is inherently a `Task`, not a poll-able flag, the
> panel caches "has resolved once, for this repository" in local state: spawn
> `project.git_scans_complete(cx)` once per repository the panel first observes, flip a
> cached bool + `cx.notify()` when it resolves, and feed that cached bool into
> `TieResolution.git_ready` on every subsequent render. Before the first resolution,
> `git_ready` is `false` and the fallback is suppressed (raw tie kept) exactly as this
> section requires.
>
> *Condition 2 is racy at restore, deliberately.* During startup, "a workspace is open at
> this path" depends on how far workspace restoration has progressed, so the two consumers
> use deliberately different predicates: **restore routing uses condition 1 only** (after
> awaiting git scans), while **panel rendering uses conditions 1 or 2**. This is not an
> oversight to reconcile later. Restore needs a stable, order-independent answer, and a
> worktree that no longer exists cannot meaningfully have its workspace reopened anyway,
> so falling back to main is correct there. Panel rendering runs against a settled UI where
> condition 2 is both well-defined and load-bearing. Worst case the two disagree for the
> span of startup and a thread lands under main instead of a stale workspace — reachable
> either way, and self-correcting on the next render.

**`panel.rs`**
- No shape changes to `render`/`render_section`/`render_row`/etc. — they already pass
  `project_worktree_roots` through; only the store-side comparison field changed.
- Handle the new `ThreadUpdated` event the same as `ThreadOpened` in
  `handle_store_event` (`panel.rs:~405-421`) — just `cx.notify()`. Because every panel
  subscribes to the same global `AgentThreadStore`, one emitted event reaches every
  open workspace's panel, each independently re-running its own live and historical
  queries against its own roots — this is what makes "retied thread disappears from one
  panel, appears in another" work with no new cross-workspace rendering code.

**Highlighting the active thread's row.** The panel highlights the row whose terminal is
the workspace's currently active item, so the panel always shows where you are.

- Derive it, don't track it: subscribe to the panel's own workspace for
  `workspace::Event::ActiveItemChanged` (`workspace.rs:1165`, emitted at `:5234`/`:5446`)
  and read `workspace.active_item(cx)` (`:3663`) at render, downcasting to `TerminalView`
  and comparing its `entity_id` against each row's `terminal_item_id`. The panel already
  holds `workspace: WeakEntity<Workspace>` and already keeps subscriptions
  (`panel.rs:296`), so this is one more subscription and a background color.
- Deriving from the active item is deliberately preferred over a stored "selected row"
  field (what the abandoned branch did): there is no second source of truth to drift, and
  it stays correct when the active tab changes by any route — tab click, keyboard, or a
  thread stealing focus on its own.
- Style: the rows already carry `.hover(|style| style.bg(cx.theme().colors().element_hover))`
  (`panel.rs:1368`, `:1435`), so the active row uses `element_selected` from the same
  palette rather than a new color.

**Tab switching and worktree switching.** Explicitly *not* wiring "activating a terminal
tab switches the active worktree." Reparenting on retie, plus condition 2 of the
deleted-worktree fallback, together make "a tab whose worktree differs from its
workspace" unreachable — every launch path puts a thread in the workspace matching its
tie, retie moves the terminal with the tie, and the fallback never fires while a
workspace is open at that path. The listener would therefore compute "switch to the
worktree I am already in" on every tab switch and do nothing. Adding it anyway would also
put an automatic, high-frequency trigger into `MultiWorkspace::activate`, which can
reparent or hide the very panel handling the callback mid-update — the hazard that forced
a `cx.defer` around the abandoned branch's `focus_thread`.

If reparenting on retie is ever dropped, this decision must be revisited: tabs and
worktrees can genuinely disagree again, and this listener becomes necessary rather than
redundant.

**Persistence**
- Add `tied_worktree_root: Option<PathBuf>` to `AgentThreadSessionRestoreRecord`
  (`store.rs:274-282`) — `Option` so old serialized records still deserialize; `None`
  means "no override, derive from the record's own `project_root`" (see restore routing
  above).
- `snapshot_records_for_workspace` (`store.rs:~2120-2138`): populate this field from
  `AgentThreadMetadata.tied_worktree_root` for **every** new snapshot — not only for
  threads that have a retie override. Restricting it to override hits would leave the new
  explicit tie unpersisted in the common case and recreate the `current_file_directory`
  restore bug for ordinary, never-retied threads (whose `project_root` is a subdirectory
  rather than a worktree root).
- `restore_threads_for_workspace` (`store.rs:~2008-2087`): use the effective-tie routing
  described above instead of `workspace_id`-only filtering; thread the resolved override
  through `resume_thread_task`/`spawn_thread_task_inner` as an explicit parameter so the
  freshly restored thread's in-memory `tied_worktree_root` matches immediately, without
  waiting for a fresh `resolve_tied_worktree_root` call to happen to agree.

## Stage 2 — Agent-initiated worktree creation/tying (local-only)

**Command surface** (two verbs, matching exactly the two requested capabilities):
```sh
"$FLINT_AGENT_CONTROL" retie-thread --worktree <existing-abs-path> [--json]
"$FLINT_AGENT_CONTROL" create-thread --worktree current|new [--name <name>] --agent <kind-id> --prompt "<task>" [--json]
```
- `retie-thread`: ensure a **background** workspace exists for the target path, then run
  the retie orchestration.
  - **Never commit the raw CLI-supplied path as the tie.** `..` segments, symlinks, and
    platform case/spelling differences would all fail the exact path-equality checks the
    resolver performs against `Worktree::abs_path()`, producing a tie that matches nothing.
    Use the incoming path only to *find or open* the destination workspace, then derive
    the committed tie from that workspace's actual resolved worktree root. If no worktree
    root can be resolved for it, reject the request rather than storing an unmatchable
    path. Shallow existence/directory validation is still the only *authorization* check
    in this pass (deep "is this a worktree of my repo" verification remains stage-3), but
    normalization is not optional — it is what makes the tie usable at all.

  - Do **not** use `find_or_create_local_workspace_with_source_workspace` for this:
    verified on `main` that it calls `MultiWorkspace::activate` on both of its
    "found an existing workspace" paths (`multi_workspace.rs:1312` and `:1357`), so an
    agent retying itself would yank the user's foreground workspace. Only its
    *creation* path (`Workspace::new_local`, `:1369-1382`) is non-activating.
  - Add a `pub fn find_or_create_background_local_workspace(...)` to
    `multi_workspace.rs` mirroring that function's structure but calling the existing
    `add_background_workspace` (`multi_workspace.rs:1489-1502`) instead of `activate`
    on every path. `add_background_workspace` already exists on `main` and its own doc
    comment names this exact use case ("the agent's `create_thread` tool spawning a
    sibling worktree in the background"); `create_worktree_workspace` already uses it
    for its `activate: false` path (`worktree_service.rs:1240`). The lookup half needs
    `workspace_for_paths_excluding` (`multi_workspace.rs:1095`, currently private) —
    keep it private and put the new function next to it in the same impl.
- `create-thread --worktree current`: call the existing `launch_seeded_thread` in the
  caller's own workspace.
- `create-thread --worktree new`: call the existing `create_worktree_workspace` (with
  `NewWorktreeBranchTarget::CurrentBranch`), then `launch_seeded_thread` on the returned
  workspace — `resolve_tied_worktree_root` picks up the new tie automatically. Note this
  path has no tie/ownership mismatch to begin with: the new thread is born in the new
  worktree's workspace.
  - Keep it **background** (`create_worktree_workspace` already hardcodes `activate:
    false` in `create_worktree_workspace_inner`). Foregrounding is a one-line change —
    the `activate: bool` parameter already exists — but the default should stay
    background, per that function's own doc comment: a background agent deciding to
    branch should not yank the user out of the worktree they are working in. If a
    foreground variant is ever wanted, it belongs behind an explicit opt-in flag on the
    command, not as the default.
- Reject (structured error, not a silent unseeded launch) when
  `kind.initial_prompt_strategy == InitialPromptStrategy::Unsupported`, fixing the
  existing silent-discard gap in `launch_seeded_thread`'s seed call.

**New crates / module**
- `crates/agent_control_protocol`: plain serde request/response types only
  (`RetieThreadRequest`, `CreateThreadRequest`, `ControlResponse`) — no GPUI/terminal
  deps, keeps the client binary small.
- `crates/agent_control_cli` (binary `flint-agent-control`, `#[cfg(unix)]` only for
  this pass): parses argv, reads `FLINT_AGENT_CONTROL_SOCKET` +
  `FLINT_AGENT_CONTROL_TOKEN` env vars, sends one JSON request over the Unix socket,
  prints the JSON response, exits with a matching status code. (Deliberately drop a
  `FLINT_AGENT_THREAD_ID` env var from the contract — the token alone should resolve
  caller identity server-side; nothing else the client sends should be trusted.)

  - **It must still compile on Windows.** As a workspace member it participates in
    Windows CI even though it is never bundled there, and a crate whose only `main` is
    `#[cfg(unix)]` fails to build with "main function not found". Provide a non-Unix
    `main` stub that prints a clear "not supported on this platform" message and exits
    non-zero, so the Unix-only logic stays behind `#[cfg(unix)]` without breaking the
    Windows build. Add a Windows-target check to verification rather than discovering
    this in CI.

**Executable delivery and location** (the `$FLINT_AGENT_CONTROL` the command examples
above invoke). Three separate pieces, none of which come for free:

- *Locating it at runtime.* Add `get_flint_agent_control_path() -> Result<PathBuf>` to
  `crates/util/src/util.rs`, directly mirroring the existing `get_flint_cli_path()`
  (`util.rs:311-349`): resolve from `std::env::current_exe()`'s parent against a
  platform-specific candidate list, `canonicalize()`, and verify it isn't the running
  executable itself. Candidates: `./flint-agent-control` on macOS (both the bundle's
  `Contents/MacOS/` and the dev `target/<triple>/debug/` layout put it beside `flint`),
  and `["../libexec/flint-agent-control", "./flint-agent-control"]` on Linux/FreeBSD
  (installed vs. dev target dir). Its absolute resolved path is what gets injected as
  `FLINT_AGENT_CONTROL`.
- *Building and bundling it.* `script/bundle-mac` currently builds only `flint` and
  `cli` (line 90) and copies only those two into the bundle (lines 324-325) — add the
  package to the `cargo build` invocation and a matching `cp` into
  `Contents/MacOS/flint-agent-control`. `script/bundle-linux` likewise builds only
  `flint`/`cli` (line 85) and installs into `libexec/flint-editor` + `bin/flint`
  (lines 123-124) — add a `cp` into `libexec/flint-agent-control` to match the
  candidate list above.
- *Signing it.* macOS only: add the new binary to `sign_app_binaries`, which currently
  hard-codes a `codesign` call for `Contents/MacOS/cli` (`bundle-mac:211`). An unsigned
  extra Mach-O inside a signed bundle invalidates the bundle signature.
- Known trap when verifying locally: `script/bundle-tmp-app` exits non-zero on an
  unrelated `remote_server` release-artifact step even when the app built fine, and
  that failure happens *before* its `cp -R` to `/tmp/Flint-Local.app` — so the target
  app is silently left stale. Check the real exit code and copy the fresh bundle by
  hand if needed (already documented in `CLAUDE.md`).

- `crates/agent_threads/src/control.rs` (server side): binds a Unix socket under
  `paths::data_dir().join(format!("agent-control-{}.sock", *release_channel::RELEASE_CHANNEL_NAME))`,
  one JSON request per connection. Every authorization decision is derived server-side
  from the token's resolved `ThreadEntry` — never from client-supplied data.

- **Server ownership and cleanup must be specified, not left to the implementation.** A
  release-channel-only pathname otherwise means a crash leaves a stale socket that blocks
  the next launch, or tempts the code into unlinking a socket owned by another live Flint
  process. Required behavior:
  - *Started exactly once*, from `agent_threads::init`, with the accept-loop `Task` stored
    on the `AgentThreadStore` global so its lifetime is the app's, not a caller's.
  - *Stale vs. live*: before unlinking an existing socket, attempt to connect to it. A
    successful connect means another live Flint owns it — do not unlink; log and leave
    the control surface disabled for this instance. Connection refused means it is stale
    and safe to remove.
  - *Permissions*: create with mode `0600` so only the current user can connect; the
    token is an authorization check, not the only boundary.
  - *Shutdown*: remove the socket on graceful shutdown, and treat leftover files as the
    stale case above rather than assuming they are ours.
- **Token lifecycle must straddle the spawn**, because the child process is live with
  the token in its environment well before Flint knows the thread's `EntityId`: in
  `spawn_thread_task_inner`, the `SpawnInTerminal` task (carrying the env vars) is built
  and handed to `add_center_terminal_view`/`create_terminal_task`, and `register()` only
  runs *after* `terminal_view_task.await?` resolves. A CLI that calls control immediately
  on startup would otherwise be rejected as an unknown token. So:
  - Store `control_tokens: HashMap<String, ControlTokenState>` on `AgentThreadStore`,
    where `ControlTokenState` is `Reserved` or `Bound(EntityId)`.
  - Mint and insert as `Reserved` **synchronously before** the task is spawned
    (alongside the existing `lifecycle_id` minting at `store.rs:~1847`).
  - `register()` promotes it to `Bound(terminal_item_id)`.
  - Remove the reservation if the spawn or registration fails (the `?` paths after the
    await), and on `begin_shutdown`.
  - A request arriving against a `Reserved` token returns a distinct, structured
    `NotReady` response rather than a generic auth failure; the CLI retries with bounded
    backoff (a couple of seconds total) before giving up. Keeping the wait client-side
    avoids parking requests inside the store.

- Env-var injection (the socket path + token) happens at the same
  `spawn_thread_task_inner` call site as `cwd`/`tied_worktree_root`, gated on: local
  project only (no `remote_client()`), Unix host, and a **new user setting**
  `agent_threads.agent_control` (bool, default `true`) on `AgentThreadSettings`.
- **Use a setting, not a feature flag.** Verified against `feature_flags`'s resolution
  order (`store.rs:165-190`) plus a repo-wide grep, Flint's flag system cannot express
  this gate at all in a release build:
  - `enabled_for_all() -> true` returns at `:167-169`, *before* user overrides — so it
    is permanently on and cannot be turned off by anything.
  - Settings overrides are gated on `overrides_enabled()` (`:105-107`), which requires
    `cfg!(debug_assertions) || self.staff` — and nothing outside the `feature_flags`
    crate ever calls `set_staff`, so in a release build overrides are ignored entirely.
  - Server-delivered flags (`:184-187`) are never populated either: nothing outside the
    crate calls `update_flags`. Flint has no cloud backend to deliver them.
  - Net: a Flint release build can only ever have a flag permanently on
    (`enabled_for_all`) or permanently off (`enabled_for_staff`). A real
    `AgentThreadSettings` field is the only gate that actually works for users, and it
    doubles as the local kill-switch. Register the Settings Editor control for it too,
    and confirm it renders — a setting added to `page_data.rs` whose field type has no
    `add_basic_renderer` registration in `settings_ui.rs`'s `init_renderers` silently
    renders as "NO RENDERER" (a plain `bool` is already registered, so this specific
    one is fine; the trap applies if the field is ever made an enum).
- Add `git_ui.workspace = true` to `crates/agent_threads/Cargo.toml` (confirmed absent
  on `main` today; confirmed no reverse dependency from `git_ui` back to
  `agent_threads`, so no cycle).

**Explicit non-goals for this pass**: Windows named-pipe transport, remote-SSH
forwarding, worktree-skill auto-install/discoverability, parent-child origin tracking
in the UI, per-parent concurrency limiting, deep "is this really a worktree of my repo"
validation on `retie-thread`'s target path.

## Testing

Follow the existing `#[gpui::test] async fn ...(cx: &mut TestAppContext)` convention
with real `FakeFs`/`Project::test` (see `panel.rs`'s existing `#[cfg(test)] mod tests`,
`init_workspace`, `wait_for_live_count`-style polling helpers) — no mocked-out store
logic.

- `store.rs`: `resolve_tied_worktree_root` fallback ordering (active-repo worktree →
  first visible worktree → `None`); `retie_thread` mutates in-memory state + writes the
  persisted override + emits + errors on unknown id; snapshot/restore round-trips
  `tied_worktree_root` including the `None` (back-compat) case.
- `panel.rs`: two workspaces on two different worktree paths each show only their own
  thread (the core "switch worktree shows different threads" behavior — currently
  zero coverage since grouping doesn't exist on `main` at all); retie a live thread
  from workspace A's panel and assert it disappears from A's live query and appears in
  B's after `run_until_parked`, **and** that it does not also appear as a duplicate
  resumable row in A's historical list (the concrete regression test for the
  duplicate-session risk); `current_file_directory` setting case showing
  `tied_worktree_root` still resolves to the worktree root even though `project_root`
  is a subdirectory; restart-restore test asserting a retied thread restores into
  workspace B specifically when **only B** (not A, its original workspace) reopens.
- Retie reparents the terminal: after a retie, the terminal item is present in
  workspace B's pane and absent from A's; `entry.workspace` resolves to B; the
  reparent does not activate B (A stays foreground); and `TerminalView`'s `workspace`
  **and** `project` both point at B afterwards, with `_terminal_subscriptions` rebuilt —
  assert the last one behaviorally by firing a terminal event that routes through the
  captured workspace handle and observing it reach B, since a field-only assertion would
  pass even with the stale handle still captured in the closure. Also assert
  `focus_thread` still works on the moved thread using `main`'s unmodified
  implementation — the proof that reparenting made the cross-workspace focus fix
  unnecessary.
- Retie failure paths leave nothing half-applied: a move that no-ops (item already
  closed) returns an error and does **not** commit the tie; a persistence failure is
  reported as such rather than as success; a thread with no session id yet reports
  `in_memory_only` and its tie is migrated to the persisted table when
  `attach_discovered_session_id` later fires.
- Tie normalization: a `retie-thread` path given with `..` segments or through a symlink
  commits the destination workspace's resolved worktree root, and matches the panel
  filter afterwards.
- Historical candidate discovery: after retying A→B and letting the process exit, B's
  panel shows the session as history — this requires the project-group-wide scan, and is
  distinct from (and must be tested separately to) suppressing the duplicate row in A
  while the terminal is still live.
- Restore vintages: a new record with a resolved tie routes by tie; a legacy record with
  `tied_worktree_root: None` still routes by `workspace_id` and is **not** path-compared
  against `project_root` (the regression test for threads whose launch cwd was a
  subdirectory).
- A tie outside every repository records `repo_main_root: None` and, once dangling,
  surfaces in no panel at all.
- Active-row highlight: the row whose terminal is the workspace's active item renders
  highlighted, the highlight moves when the active tab changes, and no row is
  highlighted when the active item is not an agent thread terminal.
- Deleted-worktree fallback: a thread tied to linked worktree X appears in the **main**
  worktree's panel once X is removed from the repo's worktree set *and* X has no open
  workspace, and restores into the main worktree after a restart; a worktree that merely
  has no workspace open (still present in `linked_worktrees()`) does **not** trigger the
  fallback; recreating X at the same path returns the thread to X (self-healing, proving
  the resolution is lazy rather than a destructive rewrite); a dangling tie pointing
  outside the repo does not surface in this repo's main panel (the `repo_main_root`
  scoping); and — the ordering hazard — restore with git state not yet scanned does not
  collapse every tie into main.
- The degenerate case specifically (condition 2): with workspace_X **open**, remove X
  from the repo's worktree set and assert the thread's row stays in workspace_X's panel
  and does *not* appear under main — i.e. the row never separates from the workspace
  holding its terminal. Then close workspace_X and assert the row moves to main. This is
  the regression test for the divergence that condition 2 exists to prevent.
- `create-thread --worktree new`: end-to-end through `git_ui::worktree_service`'s own
  `FakeFs`-based git fixtures (matching its existing test module's setup) into
  `launch_seeded_thread`, asserting the new thread's tie equals the new worktree path
  and the prompt was actually seeded; a rejection test for
  `InitialPromptStrategy::Unsupported`.
- `control.rs`: table-driven unit tests for request parsing/dispatch/token-scope
  resolution against raw byte slices (no socket needed), plus one real end-to-end
  `#[gpui::test]` round-tripping a request over an actual Unix socket, including token
  rejection for unknown/expired tokens and a thread whose terminal already closed;
  a `Reserved`-token request returns `NotReady` (not an auth failure) and succeeds
  after `register()` binds it; a failed spawn drops the reservation.
- `retie-thread` does **not** change the active workspace: assert
  `multi_workspace.workspace()` is unchanged across a retie whose target path already
  has an open background workspace (the regression test for the `activate`-on-found
  trap above).
- `agent_threads.agent_control = false` suppresses env-var injection entirely (no
  `FLINT_AGENT_CONTROL*` vars in the spawned task's env), and the Settings Editor
  control for it renders rather than falling back to "NO RENDERER".

Standard verification for every change in this plan, matching session conventions:
real `cargo test -p agent_threads -p git_ui -p project -p workspace -p terminal_view -p util -p settings_ui -p agent_control_protocol -p agent_control_cli`
(not just compiling), `cargo fmt --all -- --check`, and `./script/clippy` on every
touched crate before committing.

Additionally, because `agent_control_cli` is Unix-only in behavior but a workspace member
everywhere, verify it still builds for Windows —
`cargo check -p agent_control_cli --target x86_64-pc-windows-msvc` — so the `#[cfg(unix)]`
arrangement doesn't break Windows CI with "main function not found".

## Open questions flagged for follow-up, not blocking this plan

1. **Resolved, and superseded by a simpler answer** — retie now reparents the terminal
   into the tied workspace (see "Retie moves the terminal"), so tie and ownership never
   diverge and `focus_thread` needs no change at all. The earlier plan here — teaching
   `focus_thread` to activate another workspace — is no longer needed.
2. The new `session_id -> tied_worktree_root` override table needs its own pruning
   story (entries for sessions that no longer appear in any provider's history scan),
   independent of `prune_stale_session_restore_snapshots`'s existing app-session-
   generation-based pruning for the restore-snapshot blob. Worth a locked-in regression
   test that a retied thread's tie survives at least one full prune cycle, so a future
   change to either pruning path doesn't silently regress this.
