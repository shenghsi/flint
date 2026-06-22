# Flint performance improvement plan

## Summary

Improve Flint's CPU and memory behavior by reducing fork-specific always-on
work before tuning inherited Zed systems. The highest-confidence target is the
Agent Threads panel: it is loaded for every workspace, immediately scans Codex
and Claude history, refreshes on terminal activity, and keeps history refresh
tasks alive even when the panel is not visible.

This plan intentionally skips a separate benchmark/discovery phase. It starts
with targeted changes whose cost is visible in the code and whose user-facing
behavior can be preserved.

## Fork baseline

The fork has removed or disconnected substantial inherited functionality, which
narrows the Flint-specific startup surface but does not by itself prove a CPU or
memory improvement:

- Collaboration source and development tooling were deleted in commits
  `56997b20f7` and `273e3d5588`. The collaboration crates were already absent
  from the build graph before the source deletion, so commit size is not
  evidence of a runtime saving.
- Commit `c56d1f6d67` removes settings UI code orphaned by earlier feature
  removals. It does not establish that all AI or assistant code is gone.
- Current `crates/flint/src/flint.rs` startup wiring does not load an inherited
  assistant panel, while it does eagerly load Agent Threads.

Agent Threads is therefore the strongest currently identified Flint-specific
always-on cost, not necessarily the only one. This plan addresses that
high-confidence target first; inherited or other fork-specific costs can be
investigated after these changes if CPU and memory remain at Zed's baseline.

## Current Findings

### Agent Threads loads eagerly

File: `crates/flint/src/flint.rs`

`initialize_panels` loads `AgentThreadsPanel` with the core project, outline,
terminal, and git panels. This means every workspace constructs the panel and
starts its history work, even when the user never opens Agent Threads.

Relevant code:
- `initialize_workspace` calls `agent_threads::init(cx)`.
- `initialize_panels` creates `AgentThreadsPanel::load(...)`.
- `futures::join!` adds that panel alongside the core panels.

### History scanning starts on panel construction

File: `crates/agent_threads/src/panel.rs`

`AgentThreadsPanel::new` subscribes to the global store and spawns
`refresh_history`. `refresh_history` creates one task per registered agent kind.
Each task scans persisted history, updates panel state, then waits for history
directory changes before scanning again.

This is useful when the panel is open, but expensive as a default workspace
startup cost.

### Remote history watch is polling

File: `crates/agent_threads/src/history.rs`

`RemoteHistoryFs::watch` cannot use a real remote filesystem watcher, so it
returns a stream that wakes every second. Each wake allows the panel history loop
to scan again. On remote projects this can become repeated RPC directory reads
and file reads with no user interaction.

This polling was introduced recently in commit `5963be93a0` ("Fix history
scanning for remote projects"), together with `RemoteHistoryFs` itself, so it is
new code with no established user expectations. Removing periodic refresh in
favor of panel and thread lifecycle events is therefore low-risk.

### Terminal activity rerenders the thread list

Files:
- `crates/agent_threads/src/store.rs`
- `crates/agent_threads/src/panel.rs`

Each registered terminal subscribes to tab updates, bells, title changes, and
wakeups. These events update thread metadata and emit `AgentThreadStoreEvent`,
which causes the panel to rerender even when the change is not structural.
Terminal wakeups can be frequent while an agent is producing output.

The panel also subscribes unconditionally, so store updates notify it while its
dock is closed or another panel is selected.

### Resumed sessions replace their history row

File: `crates/agent_threads/src/store.rs`

`merge_threads` removes a historical row when a live terminal has the same
session ID, then inserts a separate live row. The row therefore changes identity,
appearance, ordering source, and click behavior when resumed. Keeping the
history row and attaching live state to it gives the session one stable row and
avoids duplicate resume actions.

### History parsers load whole files

Files:
- `crates/agent_threads/src/codex_history.rs`
- `crates/agent_threads/src/claude_history.rs`

Codex scanning bounds rollout file reads to the latest 200 files, but still
loads `session_index.jsonl` and `history.jsonl` in full on every scan. Claude
scanning loads `history.jsonl` and each matching project history file in full.
This can cost CPU and temporary memory for users with long agent histories.

### Rendering clones loaded history

File: `crates/agent_threads/src/panel.rs`

`render_section` clones loaded historical threads before merging with live
threads. After scan frequency is reduced, this is a secondary issue, but it is
still avoidable render-time allocation.

## Goals

- Reduce Flint-specific startup CPU and memory without removing Agent Threads.
- Eliminate periodic agent history polling.
- Avoid thread-list rerenders and scans while the panel is hidden or inactive.
- Refresh the list only for structural thread open/close actions.
- Keep resumed sessions in their historical position and visibly mark them live.
- Preserve the current Agent Threads user experience once the panel is opened.
- Keep remote projects from doing frequent history RPC work.
- Keep changes localized to Agent Threads and Flint workspace panel wiring,
  except for the narrow remote metadata response needed for cache identity.

## Non-goals

- No broad rewrite of GPUI, project scanning, LSP startup, git status, extension
  host, or telemetry systems.
- No removal of Codex or Claude integration.
- No user-visible benchmark UI or diagnostics panel in this change.
- No live terminal title, bell, or activity-based reordering in the thread list.
- No attempt to make Flint outperform Zed across inherited baseline systems in
  one pass.

## Plan

### 1. Load Agent Threads lazily

Files:
- `crates/flint/src/flint.rs`
- `crates/agent_threads/src/panel.rs`

Stop loading `AgentThreadsPanel` in `initialize_panels`. Keep action
registration in `agent_threads::init`, but create and add the panel only when
the user runs `agent_threads::ToggleFocus` or opens Agent Threads from the menu.

Expected behavior:
- Workspace startup no longer constructs Agent Threads.
- The first toggle creates the panel, adds it to the configured dock, and
  focuses it.
- Later toggles reuse the existing panel.

Implementation notes:
- Add a helper such as `ensure_agent_threads_panel(workspace, window, cx)`.
- Follow existing workspace panel patterns for adding/focusing panels.
- Preserve the current `AgentThreadsPanel::load` path for tests and direct
  callers if it remains useful.
- The existing `flint_actions::agent_threads::ToggleFocus` handler (added in
  commit `585b84400a`) calls `workspace.toggle_panel_focus::<AgentThreadsPanel>`,
  which silently no-ops when the panel is absent. Lazy loading requires
  rewriting this handler to construct-then-focus, not just adding a helper
  alongside it.
- Update `test_agent_threads_toggle_focus_opens_panel` (also added in
  `585b84400a`), which asserts the panel is in the left dock after toggle. Under
  lazy loading it must drive the construct-on-toggle path.

### 2. Make panel activity the refresh boundary

Files:
- `crates/agent_threads/src/panel.rs`

Implement `Panel::set_active` for `AgentThreadsPanel` and make it the explicit
boundary for rendering notifications and history work:

- `active = true` means Agent Threads is the selected panel in an open dock.
  Perform one catch-up history scan and render the latest store state.
- `active = false` means the dock closed or another panel became selected.
  Cancel any in-flight scan and stop notifying the panel for store changes.
- Moving focus from the panel to the workspace center while its dock remains
  open does not make the panel inactive or cancel an in-flight scan.
- Reopening or reselecting the panel performs one fresh catch-up scan.

Track at most one in-flight scan task per agent kind. Starting a replacement
scan drops the previous task. Hidden agent kinds do not scan, and changing a
kind to hidden cancels its task.

Do not add a background-refresh setting in this pass. It would complicate the
lifecycle and contradict the requirement that inactive panels do no refresh
work.

Expected behavior:
- Opening Agent Threads shows live threads immediately and historical rows after
  scan completes.
- Closing its dock or selecting another panel cancels history tasks and ignores
  subsequent store notifications.
- Moving keyboard focus away while Agent Threads remains visible does not cancel
  work or make the panel inactive.
- Hiding Codex or Claude in settings cancels that kind's scanner.

### 3. Replace activity updates with structural store events

Files:
- `crates/agent_threads/src/store.rs`
- `crates/agent_threads/src/panel.rs`

Replace the undifferentiated `Updated` event with structural events carrying the
affected agent kind:

- `ThreadOpened { kind_id }` after a terminal is registered.
- `ThreadClosed { kind_id }` after its terminal is released.

Remove terminal subscriptions for `ItemEvent::UpdateTab`, `TerminalEvent::Bell`,
`TerminalEvent::TitleChanged`, and `TerminalEvent::Wakeup`. Registration already
has a stable launch or resume title, and the requested live styling no longer
depends on bell state. Keep only the release subscription required to remove a
closed terminal.

Simplify `AgentThreadMetadata` accordingly:
- Keep terminal ID, kind, stable title, project root, launch time, and optional
  resumed session ID.
- Sort fresh live rows by launch time.
- Do not mutate list metadata on terminal output.

Panel event handling:
- When active, `ThreadOpened` rerenders from store state without rescanning
  history.
- When active, `ThreadClosed` rerenders immediately and schedules one history
  scan for that kind so a newly persisted session can become historical.
- Coalesce concurrent close-triggered scans per kind by replacing the existing
  task. Use a short GPUI executor delay before the one-shot scan so the CLI can
  finish writing its history file.
- When inactive, record no dirty flag and do no work; the next activation's
  catch-up scan and store read are authoritative.

### 4. Preserve resumed history rows and mark them live

Files:
- `crates/agent_threads/src/store.rs`
- `crates/agent_threads/src/panel.rs`

Change the merged row model so a historical row can carry optional live terminal
metadata. For example:

```rust
enum AgentThreadRow {
    Historical {
        thread: HistoricalThread,
        live_terminal_item_id: Option<EntityId>,
    },
    FreshLive(AgentThreadMetadata),
}
```

Merge rules:
- A live terminal with `resumed_session_id` attaches its terminal ID to the
  matching historical row instead of suppressing that row.
- The historical title, session ID, and history timestamp remain the row's
  identity and sort key while it is live.
- A live terminal with no persisted session ID remains a `FreshLive` row.
- Preserve the existing fresh-launch suppression heuristic so a just-created
  history entry does not appear beside its still-unidentified live terminal.
- When the live terminal closes, the same historical row remains and simply
  loses its live state.

Rendering and interaction:
- Historical rows remain muted when inactive.
- A historical row with a live terminal uses `Color::Success` for its status
  icon and title.
- Clicking a live historical row focuses its existing terminal.
- The resume-options button and resume context menu are unavailable while that
  row is live, preventing a duplicate resume.
- Fresh live rows use the same success color and focus behavior.

### 5. Eliminate periodic history watches

File: `crates/agent_threads/src/history.rs`

Remove the watch loop from `refresh_history`; each invocation performs one scan
and exits. History scans occur only:
- once when the panel becomes active;
- once for an affected kind after a live thread closes while the panel is
  active; or
- after a relevant visible agent setting changes.

Remove `HistoryFs::watch`, `RemoteHistoryFs`'s background executor, and
`NoopWatcher` if they have no remaining callers. Local and remote history now
follow the same event-driven scan policy, with no timer or polling stream.

### 6. Cache parsed history by file identity

Files:
- `crates/agent_threads/src/history.rs`
- `crates/agent_threads/src/codex_history.rs`
- `crates/agent_threads/src/claude_history.rs`
- `crates/proto/proto/worktree.proto`
- `crates/remote_server/src/headless_project.rs`

First extend `HistoryFs` with a metadata operation that returns a deliberately
small history-specific identity:

```rust
struct HistoryFileIdentity {
    modified_at: MTime,
    length: u64,
}

async fn metadata(
    &self,
    path: &Path,
) -> Result<Option<HistoryFileIdentity>>;
```

For local history, map `fs::Fs::metadata` to modification time and length. For
remote history, extend `GetPathMetadataResponse` to include optional modification
time and length, populate them in the remote server handler, and call that RPC
from `RemoteHistoryFs`. Treat missing metadata as a cache miss rather than
silently reusing an entry.

Introduce scanner state per agent kind that owns both its task and parsed-history
cache. Extend the provider scan contract to receive that cache rather than
placing provider-specific parsed data in `HistoryFs`. Cache results by path and
`HistoryFileIdentity`; replace an entry after a successful parse and evict
entries for files no longer returned by directory scans. Keep the cache bounded
by the same file caps used by each provider.

Codex changes:
- Avoid reloading `session_index.jsonl` and `history.jsonl` when unchanged.
- Keep the existing rollout scan cap.
- Cache titles for rollout files whose first user-message title was already
  parsed.

Claude changes:
- Avoid reloading `history.jsonl` when unchanged.
- Cache per-project history file summaries.
- Sort and cap project history files by recency if many files are present.

This is a follow-up optimization after the event-driven behavior is working.
The event model removes the high-frequency path; caching limits the cost of the
remaining activation and close-triggered scans.

### 7. Reduce render-time allocation

File: `crates/agent_threads/src/panel.rs`

Avoid cloning all historical threads during render. Options:
- Store loaded history as `Arc<[HistoricalThread]>`.
- Precompute merged visible rows after each scan/store update.
- Pass references through merge/render helpers and clone only values needed by
  event handlers.

This should happen after scan gating, because it is a smaller win and is easier
to validate once background churn is gone.

### 8. Add focused tests

Files:
- `crates/agent_threads/src/panel.rs`
- `crates/agent_threads/src/history.rs`
- `crates/flint/src/flint.rs`
- `crates/remote_server/src/headless_project.rs`

Add or update tests for:
- Agent Threads panel is not present immediately after workspace initialization.
- Toggling Agent Threads creates and focuses the panel.
- History is scanned once when the panel becomes active.
- Closing the dock or activating a sibling panel cancels an in-flight scan.
- Moving focus to the workspace center while Agent Threads remains visible does
  not cancel history tasks.
- Hidden agent kinds do not scan history.
- Terminal wakeups, title changes, bells, and tab updates do not emit structural
  store events or rerender the panel.
- Opening a thread rerenders an active panel but does not scan history.
- Closing a thread rerenders an active panel and triggers one coalesced,
  close-delayed scan for that kind.
- Opening or closing threads while the panel is inactive performs no panel work.
- Reactivating the panel performs one catch-up scan and shows current live state.
- A resumed session remains a historical row and is marked live.
- Clicking that live history row focuses the existing terminal.
- A live history row cannot launch a duplicate resume or open resume options.
- Closing the terminal leaves the historical row present and inactive.
- Fresh live sessions still render as live rows.
- Local and remote history perform no periodic watch or polling work.
- Local and remote metadata identities avoid re-reading unchanged files.
- A changed identity causes the file to be read and reparsed.
- Missing metadata is handled as a cache miss.

Use GPUI executor timers in tests rather than `smol::Timer::after`.

## Suggested Implementation Order

1. Make panel creation lazy and update toggle behavior.
2. Add panel active state and one-shot per-kind history scans.
3. Replace activity-driven updates with structural open/close store events.
4. Preserve historical rows for resumed sessions and add live styling/focus.
5. Remove local watches and remote polling.
6. Add local and remote metadata identities, then history parse caching.
7. Reduce render-time cloning.
8. Run focused tests and `./script/clippy`.

## Acceptance Criteria

- A persisted session remains one history row before, during, and after resume.
- While its terminal is live, that row is success-colored and clicking it
  focuses the existing terminal.
- A live persisted row cannot start another resume.
- Fresh sessions without a known persisted ID still appear as live rows.
- Terminal output, wakeups, title changes, bells, and tab changes do not
  rerender or reorder the Agent Threads list.
- Outside panel activation and relevant settings changes, only thread
  registration and terminal closure update an active list.
- An inactive or hidden panel performs no scan, rerender, timer, watch, or
  polling work in response to thread activity.
- Reactivating the panel performs one catch-up scan and displays current live
  terminals.
- Local and remote history use no periodic refresh loop.
- Focused tests pass, followed by `./script/clippy`.

## Expected Impact

Lazy loading and active-state gating should reduce Flint-specific startup work
and idle memory for users who do not use Agent Threads. Removing terminal
activity notifications, filesystem watches, and remote polling should provide
the largest idle CPU reduction, especially while agents are producing output or
projects are remote. Removing unused terminal subscriptions and cancelling
inactive scans provides a smaller memory reduction. History caching should help
users with large Codex or Claude histories by avoiding repeated whole-file
parsing during the remaining one-shot scans.

After these changes, measure again before attributing any remaining CPU or
memory parity with Zed. The next investigation should compare inherited startup
and idle systems and re-check the remaining fork-specific wiring.
