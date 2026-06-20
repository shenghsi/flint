# Open Changes icon in the editor toolbar

## Summary

Add an "Open Changes" icon to the `QuickActionBar` (the toolbar row above the
editor that already hosts the buffer search, Selection Controls, and Editor
Controls icons). The icon is visible only when the active file has
uncommitted git changes. Clicking it opens a diff view comparing the working
tree version of the file against its committed (HEAD) version.

## Non-goals

- No new globally dispatchable `Action` / keybinding / command palette entry.
  None of the other per-file git operations (`StageFile`, `UnstageFile`,
  `Blame`) are wired for arbitrary editors today, only inside the Git Panel or
  `SoloDiffView` itself; adding that plumbing here would be scope creep.
- No new diff-rendering UI. The diff view is the existing `SoloDiffView`,
  unchanged.

## Design

### 1. Visibility / gating

File: `crates/flint/src/flint/quick_action_bar.rs`

The icon is shown only when:
- the active pane item is an `Editor`, and
- `editor.buffer_kind(cx) == ItemBufferKind::Singleton` (same gate the
  existing search button uses), and
- `editor.read(cx).buffer().read(cx).snapshot(cx).has_diff_hunks()` is true.

`has_diff_hunks()` reflects the buffer's "uncommitted diff" (working tree vs
HEAD), which is the same diff already wired up for every open file's gutter
markers via `update_uncommitted_diff_for_buffer` in `crates/editor/src/git.rs`
(it calls `project.open_uncommitted_diff`, not `open_unstaged_diff`). This
diff includes staged and unstaged changes, so the icon's gating logic matches
"any tracked change, staged or not" with no extra git-status query — it's the
same data already backing gutter markers and the "Next/Previous Hunk" entries
in the Selection Controls menu in this same file.

### 2. New public API on `SoloDiffView`

File: `crates/git_ui/src/solo_diff_view.rs`

`SoloDiffView::open_or_focus` already opens/focuses a single-file diff tab
(working tree vs HEAD, with stage/unstage/restore controls and the
split/unified toggle) given a `GitStatusEntry` + `Entity<Repository>`. Its
fields are `pub(crate)` to `git_ui`, so a new entry point is needed for
callers (like `flint`) that only have an open buffer, not a pre-built
`GitStatusEntry`:

```rust
pub fn open_or_focus_for_buffer(
    buffer: Entity<Buffer>,
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Result<Entity<Self>>>
```

Implementation:
1. Resolve `(repository, repo_path)` via
   `project.read(cx).git_store().read(cx).repository_and_path_for_buffer_id(buffer.read(cx).remote_id(), cx)`
   — the same helper `crates/editor/src/git/blame.rs` uses to go from a buffer
   to its repo.
2. Look up the current status via `repository.read(cx).status_for_path(&repo_path)`.
3. Build a `GitStatusEntry { repo_path, status, staging: status.staging(), diff_stat }`
   from the resulting `StatusEntry`.
4. Delegate to the existing `Self::open_or_focus(entry, repository, workspace, window, cx)`.

If the file isn't part of a repository, or has no status entry (e.g. the
change was committed or reverted in the moment between the icon rendering and
the click), the function returns `Err` instead of guessing, surfaced via the
caller's `detach_and_notify_err`.

### 3. Toolbar wiring

File: `crates/flint/src/flint/quick_action_bar.rs`

A new `IconButton` with `Tooltip::text("Open Changes")` — a plain text
tooltip, not `Tooltip::for_action_*`, consistent with the existing Selection
Controls / Editor Controls trigger buttons, since there's no backing
`Action`. It is added as the leftmost child of the toolbar's `h_flex`, before
`search_button`.

Icon: `IconName::Diff`. This is the codebase's established "git diff" icon —
it's already used for every diff-view tab (`SoloDiffView`, `MultiDiffView`,
`TextDiffView`, and even the generic two-buffer `FileDiffView`). The only
other candidate, `IconName::FileDiff`, is declared in `crates/icons/src/icons.rs`
but never actually rendered anywhere in the codebase, so it is not used here.

`on_click`:
1. Read the active editor's singleton buffer (`editor.buffer().read(cx).as_singleton()`)
   and project (`editor.project()`).
2. Call `SoloDiffView::open_or_focus_for_buffer(buffer, project, workspace, window, cx)`.
3. `.detach_and_notify_err(workspace, window, cx)` on the returned task, the
   same error-surfacing pattern `git_panel.rs`'s `open_solo_diff` already
   uses.

## Testing

- A `git_ui` test that opens a buffer with uncommitted changes (covering
  modified, staged, and untracked/new-file cases) and asserts
  `open_or_focus_for_buffer` produces/focuses the same `SoloDiffView` tab as
  the existing Git Panel `open_solo_diff` path produces for the same file.
- A `git_ui` test asserting `open_or_focus_for_buffer` returns `Err` for a
  file with no uncommitted changes and for a file outside any repository.
- Manual check in the running app: the icon appears/disappears as a file's
  git status changes (edit, stage, commit, revert), and clicking it
  opens/focuses the diff tab and matches the Git Panel's existing solo-diff
  behavior for that file.
