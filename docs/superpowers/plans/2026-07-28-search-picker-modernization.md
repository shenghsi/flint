# Search and Picker Modernization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete Tasks 6.3 through 6.6 with symbol previews, previewable LSP results, reconstructible picker history, and stable multi-selection in one Wave 6 integration PR.

**Architecture:** Extend Flint's existing `Picker` preview contract and add generic stable-ID selection mechanics there. Keep result-domain behavior in its owning crate, add a focused `lsp_locations` crate for normalized LSP result snapshots, and store only typed reconstruction requests plus serializable picker state in Workspace.

**Tech Stack:** Rust, GPUI entities/tasks/actions, Flint Picker and Workspace modal APIs, Project/Buffer/MultiBuffer, LSP test servers, settings content and Settings Editor.

## Global Constraints

- Work on `feature/search-picker-modernization`; never commit directly to `main`.
- Deliver Tasks 6.3 through 6.6 in one integration PR titled `search: Modernize symbol and LSP pickers`.
- Keep each reviewable feature slice as its own commit on that branch.
- Preserve `.pi/` and unrelated user changes.
- Reuse the preview infrastructure landed in Tasks 6.1 and 6.2.
- Reopen pickers from reconstructible requests; never retain a dismissed live modal, view, entity, task, buffer snapshot, or LSP response.
- Restore the previous query, multi-select mode, and stable selected identities that remain valid.
- Default `editor.lsp_results_location` to `multi_buffer`; existing action payloads remain valid.
- Run each behavior through red-green-refactor before writing its production implementation.
- Propagate or visibly log every fallible operation; do not use `unwrap()`, panicking indexing, or `let _ =` to discard errors.
- Preserve Flint's single-user product boundary and do not add collaboration, account, or cloud behavior.
- Use GPUI executor timers rather than `smol::Timer` in GPUI tests.
- Before pushing Rust changes, run `cargo fmt --all -- --check`.
- Use `./script/clippy`, not `cargo clippy`.

---

## File map

### Shared picker

- `crates/picker/src/picker.rs`: generic stable-ID selection state, actions, reconciliation, restoration, and delegate hooks.
- `crates/picker/src/render.rs`: selected-row and Text Finder parent-selection presentation hooks.
- `crates/picker/src/head.rs`: multi-select mode control beside the search input.
- `crates/picker/src/preview.rs`: project-symbol preview source and stale-load rejection.
- `crates/picker/Cargo.toml`: dependencies needed by shared controls and tests.

### Symbol consumers

- `crates/project_symbols/src/project_symbols.rs`: project-symbol preview construction, request data, and tests.
- `crates/project_symbols/Cargo.toml`: preview/test dependencies.
- `crates/outline/src/outline.rs`: buffer-symbol preview construction, range mapping, request data, and tests.
- `crates/outline/Cargo.toml`: project/preview dependencies.

### Multi-select consumers

- `crates/file_finder/src/file_finder.rs`: stable path identities, multi-confirm, and restoration request.
- `crates/file_finder/src/file_finder_tests.rs`: filtering, refresh, confirm ordering, and reopen coverage.
- `crates/search/src/text_finder/delegate.rs`: file-header identities, match-row parent routing, multi-confirm, and restoration request.
- `crates/search/src/text_finder.rs`: Text Finder reconstruction entry point.
- `crates/search/src/text_finder/render.rs`: header checkbox and parent-selected match styling.
- `crates/search/src/text_finder/tests.rs`: grouped selection, collapse, filtering, and reopen coverage.
- `assets/keymaps/specific-overrides-macos.json`: macOS multi-select shortcut.
- `assets/keymaps/specific-overrides.json`: Linux/Windows multi-select shortcut.

### LSP results

- `crates/lsp_locations/Cargo.toml`: new logical component with `[lib] path = "src/lsp_locations.rs"`.
- `crates/lsp_locations/src/lsp_locations.rs`: normalized result snapshot, picker delegate, preview, navigation, reconstruction request, and tests.
- `Cargo.toml` and `Cargo.lock`: workspace membership and dependency lock.
- `crates/editor/src/actions.rs`: optional `open_results_in` on definitions, implementations, and references.
- `crates/editor/src/editor_settings.rs`: resolved global LSP result location.
- `crates/editor/src/navigation.rs`: one-query result collection and presentation routing.
- `crates/editor/src/editor_tests.rs`: action compatibility, result cardinality, duplicate, server, missing-file, and remote tests.
- `crates/settings_content/src/editor.rs`: `OpenResultsIn` and `lsp_results_location`.
- `assets/settings/default.json`: compatibility-preserving default and user-facing documentation.
- `crates/settings_ui/src/page_data.rs`: exact Settings Editor field.
- `crates/settings_ui/src/settings_ui.rs`: dropdown renderer registration.
- `crates/settings_ui/src/page_data.rs`: exact JSON path assertion in its
  existing test module.
- `crates/flint/Cargo.toml` and `crates/flint/src/flint.rs`: initialize the new picker crate.

### Reconstructible history

- `crates/workspace/src/picker_history.rs`: request trait, restoration state, pending reopen sequencing, and history lifecycle.
- `crates/workspace/src/workspace.rs`: `ReopenLastPicker` action, storage, modal-close integration, and tests.
- `crates/workspace/src/modal_layer.rs`: modal-closed event required for Command Palette and Which Key ordering.
- `crates/command_palette/src/command_palette.rs`: keep the reopen action visible.
- `assets/keymaps/default-linux.json`, `assets/keymaps/default-macos.json`, and `assets/keymaps/default-windows.json`: default reopen shortcut.

### Provenance and delivery

- `docs/superpowers/zed-upstream-ledger.md`: add resolved upstream rows before implementation and mark them landed only after the integration PR merges.
- `docs/superpowers/specs/2026-07-27-upstream-domain-wave-implementation-plan.md`: check off the integrated slices after landing.

---

### Task 1: Register provenance and add symbol previews

**Files:**

- Modify: `docs/superpowers/zed-upstream-ledger.md`
- Modify: `crates/picker/src/preview.rs`
- Modify: `crates/project_symbols/src/project_symbols.rs`
- Modify: `crates/project_symbols/Cargo.toml`
- Modify: `crates/outline/src/outline.rs`
- Modify: `crates/outline/Cargo.toml`

**Interfaces:**

- Consumes: `Picker::uniform_list_with_preview`, `PreviewUpdate::from_buffer`, `Project::open_buffer_for_symbol`, and `MultiBuffer::text_anchor_for_position`.
- Produces: `PreviewUpdate::from_symbol(Symbol)`, project-symbol and buffer-symbol delegates that implement `try_get_preview_data_for_match`.

- [ ] **Step 1: Add the resolved upstream provenance rows**

Add ledger rows for Zed #59863, #61069, #59912, and #61002 with their verified
merge commits, `reimplement` strategy, integrated Wave 6 prerequisite, and
`implementing` status. Preserve the existing #59604, #59838, #59931, and
#60919 rows.

- [ ] **Step 2: Write failing project-symbol preview tests**

Add tests in `crates/project_symbols/src/project_symbols.rs` that construct a
symbol result and assert:

```rust
let update = picker.read_with(cx, |picker, cx| {
    picker.delegate.try_get_preview_data_for_match(cx)
});
assert!(matches!(update, Some(PreviewUpdate::Symbol(_))));
```

Add a second test with two deferred buffer opens. Resolve the older open after
selecting the newer symbol and assert `preview_current_path` remains the newer
path.

- [ ] **Step 3: Run project-symbol tests and verify RED**

Run:

```bash
cargo test -p project_symbols preview -- --nocapture
```

Expected: compilation fails because `PreviewUpdate::from_symbol` and the
delegate preview hook do not exist.

- [ ] **Step 4: Implement the symbol-backed preview source**

Extend the preview update model with:

```rust
pub enum Update {
    Path(PathBuf),
    Buffer {
        buffer: Entity<Buffer>,
        match_range: Range<language::Anchor>,
    },
    Symbol(Symbol),
}

impl Update {
    pub fn from_symbol(symbol: Symbol) -> Self {
        Self::Symbol(symbol)
    }
}
```

In `EditorPreview`, open the symbol buffer through
`Project::open_buffer_for_symbol`, clip its UTF-16 range with `Bias::Left`,
derive anchors, and feed the buffer to `update_from_buffer`. Use the existing
`PreviewLoadGuard` and stored `load_task`; a late result must fail the
`is_current` check.

- [ ] **Step 5: Wire Project Symbols to the shared preview**

Construct it with:

```rust
Picker::uniform_list_with_preview(delegate, project, window, cx).width(rems(34.))
```

Implement:

```rust
fn try_get_preview_data_for_match(&self, _cx: &App) -> Option<PreviewUpdate> {
    let candidate_id = self.matches.get(self.selected_match_index)?.candidate_id;
    Some(PreviewUpdate::from_symbol(self.symbols.get(candidate_id)?.clone()))
}
```

Use checked access for both the selected match and symbol.

- [ ] **Step 6: Run project-symbol tests and verify GREEN**

Run:

```bash
cargo test -p project_symbols -- --test-threads=1
```

Expected: all project-symbol tests pass, including stale completion.

- [ ] **Step 7: Write failing buffer-symbol preview tests**

In `crates/outline/src/outline.rs`, add tests for:

```rust
assert_eq!(
    picker.read_with(cx, |picker, cx| picker.preview_current_path(cx)),
    expected_path
);
```

Cover a project buffer, an unsaved project buffer, and a synthetic outline
range whose endpoints map to different underlying buffers; the last case must
produce no preview.

- [ ] **Step 8: Run outline preview tests and verify RED**

Run:

```bash
cargo test -p outline preview -- --nocapture
```

Expected: the first preview assertion fails because `OutlineView` still uses a
picker without preview.

- [ ] **Step 9: Implement Buffer Symbols preview**

When the active editor has a project, construct the picker with
`uniform_list_with_preview`; otherwise preserve `uniform_list`. Implement
`try_get_preview_data_for_match` by mapping both ends of
`outline_item.selection_range` through the active multibuffer. Return `None`
when they map to different buffers. Otherwise return:

```rust
Some(PreviewUpdate::from_buffer(
    buffer,
    start_anchor..end_anchor,
))
```

- [ ] **Step 10: Run symbol suites and commit**

Run:

```bash
cargo test -p project_symbols -- --test-threads=1
cargo test -p outline -- --test-threads=1
cargo test -p picker
```

Then:

```bash
git add docs/superpowers/zed-upstream-ledger.md crates/picker crates/project_symbols crates/outline Cargo.lock
git commit -m "Add previews to symbol pickers"
```

---

### Task 2: Add the stable picker selection model

**Files:**

- Modify: `crates/picker/src/picker.rs`
- Modify: `crates/picker/src/render.rs`
- Modify: `crates/picker/src/head.rs`
- Modify: `crates/picker/Cargo.toml`

**Interfaces:**

- Consumes: existing `PickerDelegate::can_select`, `render_match`, and picker query/update lifecycle.
- Produces: `PickerItemId`, `PickerRestorationState`, `ToggleMultiSelect`, `MultiSelectNext`, and optional delegate hooks for stable identity and multi-confirm.

- [ ] **Step 1: Write failing selection-state tests**

Add a picker test delegate whose visible indices reorder while its stable IDs
remain `"a"`, `"b"`, and `"c"`. Test:

```rust
picker.update(cx, |picker, cx| {
    picker.set_multi_select_enabled(true, cx);
    picker.toggle_item_selection(1, window, cx);
});
delegate.reorder(["c", "b", "a"]);
assert_eq!(selected_ids(&picker, cx), ["b"]);
```

Add separate tests proving selection survives filtering and asynchronous
refresh, disabled entries cannot be selected, vanished IDs are removed during
explicit reconciliation, and confirmation follows current result order rather
than hash-set order.

- [ ] **Step 2: Run picker selection tests and verify RED**

Run:

```bash
cargo test -p picker multi_select -- --nocapture
```

Expected: compilation fails because the selection types and methods do not
exist.

- [ ] **Step 3: Define stable IDs and delegate hooks**

Add:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PickerItemId(SharedString);

impl PickerItemId {
    pub fn new(value: impl Into<SharedString>) -> Self;
    pub fn as_str(&self) -> &str;
    pub fn into_shared_string(self) -> SharedString;
}
```

Extend `PickerDelegate` with default-disabled hooks:

```rust
fn supports_multi_select(&self) -> bool { false }
fn item_id(&self, _index: usize) -> Option<PickerItemId> { None }
fn item_id_is_valid(&self, _id: &PickerItemId, _cx: &App) -> bool { false }
fn confirm_multi(
    &mut self,
    _ids: Vec<PickerItemId>,
    _window: &mut Window,
    _cx: &mut Context<Picker<Self>>,
) {}
```

The defaults must leave every existing delegate unchanged.

- [ ] **Step 4: Implement Picker-owned selection state**

Add fields:

```rust
multi_select_enabled: bool,
selected_item_ids: HashSet<PickerItemId>,
```

Implement checked toggle/reconcile helpers. `ordered_selected_item_ids` scans
the current result order first, then appends still-valid filtered IDs in their
prior stable insertion order. Store insertion order explicitly rather than
iterating a `HashSet`.

- [ ] **Step 5: Add actions and route confirmation**

Define namespaced actions:

```rust
actions!(picker, [ToggleMultiSelect, MultiSelectNext]);
```

When multi-select is enabled and at least one valid identity is selected,
`do_confirm` calls `confirm_multi`; otherwise it preserves existing
single-confirm behavior. Secondary click starts or toggles multi-select only
for supporting delegates.

- [ ] **Step 6: Add restoration accessors**

Add:

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickerRestorationState {
    pub query: String,
    pub multi_select_enabled: bool,
    pub selected_item_ids: Vec<PickerItemId>,
}

pub fn restoration_state(&self, cx: &App) -> PickerRestorationState;
pub fn restore_state(
    &mut self,
    state: PickerRestorationState,
    window: &mut Window,
    cx: &mut Context<Self>,
);
```

`restore_state` sets the query, records IDs pending result refresh, and applies
them only after the delegate's update completes.

- [ ] **Step 7: Run picker tests and verify GREEN**

Run:

```bash
cargo test -p picker -- --test-threads=1
```

Expected: all selection and existing preview tests pass.

- [ ] **Step 8: Commit the selection model**

```bash
git add crates/picker
git commit -m "Add stable picker multi-selection"
```

---

### Task 3: Add File Finder and Text Finder multi-select controls

**Files:**

- Modify: `crates/picker/src/head.rs`
- Modify: `crates/picker/src/render.rs`
- Modify: `crates/file_finder/src/file_finder.rs`
- Modify: `crates/file_finder/src/file_finder_tests.rs`
- Modify: `crates/search/src/text_finder/delegate.rs`
- Modify: `crates/search/src/text_finder/render.rs`
- Modify: `crates/search/src/text_finder/tests.rs`
- Modify: `assets/keymaps/specific-overrides-macos.json`
- Modify: `assets/keymaps/specific-overrides.json`

**Interfaces:**

- Consumes: Task 2's `PickerItemId`, selection actions, and multi-confirm hooks.
- Produces: stable ProjectPath identities, deterministic multi-open behavior, search-row control, shared checkboxes, and accessible labels.

- [ ] **Step 1: Write failing File Finder behavior tests**

Add tests that select two paths, change the query so one becomes filtered,
refresh candidates, clear the query, and assert both remain selected. Confirm
and assert files open in visible result order. Add disabled/vanished path
tests and a secondary-click routing test.

- [ ] **Step 2: Run File Finder tests and verify RED**

Run:

```bash
cargo test -p file_finder multi_select -- --nocapture --test-threads=1
```

Expected: the delegate reports no multi-select support and no files are
selected.

- [ ] **Step 3: Implement File Finder identities and multi-confirm**

Use a reversible stable key that includes worktree identity and relative path.
Maintain a checked `PickerItemId -> ProjectPath` map for visible and retained
selected paths. Implement `supports_multi_select`, `item_id`,
`item_id_is_valid`, and `confirm_multi`. Reuse the existing file-opening
function; open each path once and surface every error through the workspace.

- [ ] **Step 4: Write failing Text Finder grouped-selection tests**

Add tests asserting:

- only `Entry::Header` renders a checkbox;
- toggling on an `Entry::Match` selects its parent file;
- the match row reflects the parent's selected style;
- a collapsed header remains selectable;
- selecting two matches in one file opens that file only once;
- query/filter refresh preserves selected file identities.

- [ ] **Step 5: Run Text Finder tests and verify RED**

Run:

```bash
cargo test -p search text_finder::tests::multi_select -- --nocapture --test-threads=1
```

Expected: grouped results still route confirmation to one match and expose no
file-level selection.

- [ ] **Step 6: Implement Text Finder parent-file selection**

Map both header and match rows to the same file `PickerItemId`. Render the
shared `Checkbox` only for headers. Use parent-selected styling for match rows.
`confirm_multi` resolves IDs to unique `ProjectPath` values and opens them in
current group order.

- [ ] **Step 7: Add discoverable controls and shortcuts**

Render a multi-select icon button at the end of the picker search row only
when the delegate opts in. The tooltip names `picker::ToggleMultiSelect` and
shows its keybinding. Render shared checkbox components with labels
`"Select {item}"` or `"Deselect {item}"`. Bind `cmd-shift-s` on macOS and
`ctrl-shift-s` on Linux/Windows to `picker::ToggleMultiSelect` in the generic
Picker editor context. Bind `tab` to `picker::MultiSelectNext` in the File
Finder and Text Finder contexts; it toggles the focused identity and advances
to the next selectable row.

- [ ] **Step 8: Test controls, keyboard-only use, and narrow layouts**

Add picker render tests that use a narrow viewport and assert the search input
remains usable, the mode control remains focusable, and checkbox labels expose
selected state. Exercise mode and item toggles only through actions.

- [ ] **Step 9: Run affected suites and commit**

Run:

```bash
cargo test -p picker -- --test-threads=1
cargo test -p file_finder -- --test-threads=1
cargo test -p search -- --test-threads=1
```

Then:

```bash
git add crates/picker crates/file_finder crates/search assets/keymaps
git commit -m "Add multi-select to file and text finders"
```

---

### Task 4: Add previewable LSP result pickers and settings

**Files:**

- Create: `crates/lsp_locations/Cargo.toml`
- Create: `crates/lsp_locations/src/lsp_locations.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/editor/src/actions.rs`
- Modify: `crates/editor/src/editor_settings.rs`
- Modify: `crates/editor/src/navigation.rs`
- Modify: `crates/editor/src/editor_tests.rs`
- Modify: `crates/settings_content/src/editor.rs`
- Modify: `assets/settings/default.json`
- Modify: `crates/settings_ui/src/page_data.rs`
- Modify: `crates/settings_ui/src/settings_ui.rs`
- Modify: `crates/flint/Cargo.toml`
- Modify: `crates/flint/src/flint.rs`

**Interfaces:**

- Consumes: Task 1 preview sources and current Editor navigation queries.
- Produces: `OpenResultsIn`, `LspLocationRequest`, normalized `LspLocationMatch` snapshots, and `lsp_locations::init`.

- [ ] **Step 1: Write failing action/settings compatibility tests**

Test deserialization of all existing payloads plus:

```json
{"open_results_in":"picker"}
```

for `editor::GoToDefinition`, `editor::GoToImplementation`, and
`editor::FindAllReferences`. Assert omitted values deserialize to `None` and
the resolved global default is `OpenResultsIn::MultiBuffer`.

- [ ] **Step 2: Run editor/settings tests and verify RED**

Run:

```bash
cargo test -p editor action -- --nocapture
cargo test -p settings_content lsp_results_location -- --nocapture
```

Expected: `open_results_in` and `lsp_results_location` are unknown.

- [ ] **Step 3: Add the setting and action schema**

Define in settings content:

```rust
#[derive(
    Copy, Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq,
    JsonSchema, MergeFrom, strum::VariantArray, strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum OpenResultsIn {
    #[default]
    MultiBuffer,
    Picker,
}
```

Add `Option<OpenResultsIn>` to `EditorSettingsContent`,
`OpenResultsIn` to resolved `EditorSettings`, and
`"lsp_results_location": "multi_buffer"` to defaults. Add
`open_results_in: Option<OpenResultsIn>` with `#[serde(default)]` to the three
actions without changing `FindAllReferences::always_open_multibuffer`.

- [ ] **Step 4: Add the exact Settings Editor control**

Register `OpenResultsIn` with `render_dropdown`. Add an LSP section field with
`json_path: Some("lsp_results_location")` and getter/setter targeting
`settings_content.editor.lsp_results_location`. In the existing
`page_data.rs` test module, add a test that asserts the field's JSON path is
exactly `editor.lsp_results_location`.

- [ ] **Step 5: Write failing one-query LSP presentation tests**

For definitions, references, and implementations, use test language servers
with counters and assert:

```rust
assert_eq!(request_count.load(Ordering::SeqCst), 1);
select_next_picker_match(&workspace, window, cx);
assert_eq!(request_count.load(Ordering::SeqCst), 1);
```

Cover zero results, one direct result, multiple servers, duplicate locations,
missing files, and a remote project. Assert per-action settings override the
global value.

- [ ] **Step 6: Run LSP tests and verify RED**

Run:

```bash
cargo test -p editor lsp_results -- --nocapture --test-threads=1
```

Expected: multiple results still use only the existing multibuffer path and no
LSP picker is available.

- [ ] **Step 7: Create the `lsp_locations` crate**

Use:

```toml
[package]
name = "lsp_locations"
version = "0.1.0"
edition.workspace = true
publish.workspace = true
license = "GPL-3.0-or-later"

[lib]
path = "src/lsp_locations.rs"
doctest = false
```

Define:

```rust
pub enum LspLocationKind {
    Definition,
    Reference,
    Implementation,
}

pub struct LspLocationRequest {
    pub kind: LspLocationKind,
    pub source_buffer_id: BufferId,
    pub source_position: PointUtf16,
    pub open_results_in: Option<OpenResultsIn>,
}

pub struct LspLocationMatch {
    pub project_path: ProjectPath,
    pub range: Range<PointUtf16>,
    pub line_text: SharedString,
}
```

Normalize by `(ProjectPath, range)` and keep deterministic server/result order.
Build fuzzy candidates once from path and line text. The delegate filters only
that captured snapshot and returns preview updates without invoking Editor or
LSP query methods.

- [ ] **Step 8: Route Editor navigation through one collected result**

Split existing navigation into collection and presentation:

```rust
fn collect_definition_locations(...) -> Task<Result<Vec<Location>>>;
fn collect_reference_locations(...) -> Task<Result<Vec<Location>>>;
fn collect_implementation_locations(...) -> Task<Result<Vec<Location>>>;
fn present_lsp_locations(
    request: LspLocationRequest,
    locations: Vec<Location>,
    window: &mut Window,
    cx: &mut Context<Editor>,
);
```

Preserve current direct-open and definition-fallback behavior. For multiple
usable results, resolve the action override first and global setting second.
Initialize `lsp_locations` from Flint startup and register its dependency.

- [ ] **Step 9: Run LSP and settings suites and commit**

Run:

```bash
cargo test -p lsp_locations -- --test-threads=1
cargo test -p editor lsp_results -- --test-threads=1
cargo test -p settings_content
cargo test -p settings_ui
cargo check -p flint
```

Then:

```bash
git add Cargo.toml Cargo.lock assets/settings crates/lsp_locations crates/editor crates/settings_content crates/settings_ui crates/flint
git commit -m "Add previewable LSP result pickers"
```

---

### Task 5: Reconstruct the last supported picker

**Files:**

- Create: `crates/workspace/src/picker_history.rs`
- Modify: `crates/workspace/src/workspace.rs`
- Modify: `crates/workspace/src/modal_layer.rs`
- Modify: `crates/picker/src/picker.rs`
- Modify: `crates/file_finder/src/file_finder.rs`
- Modify: `crates/search/src/text_finder.rs`
- Modify: `crates/search/src/text_finder/delegate.rs`
- Modify: `crates/project_symbols/src/project_symbols.rs`
- Modify: `crates/outline/src/outline.rs`
- Modify: `crates/lsp_locations/src/lsp_locations.rs`
- Modify: `crates/command_palette/src/command_palette.rs`
- Modify: `assets/keymaps/default-linux.json`
- Modify: `assets/keymaps/default-macos.json`
- Modify: `assets/keymaps/default-windows.json`

**Interfaces:**

- Consumes: Task 2's restoration state and every completed picker constructor.
- Produces: `ReopenablePickerRequest`, `StoredPickerState`, `Workspace::record_picker_request`, and `workspace::ReopenLastPicker`.

- [ ] **Step 1: Write failing Workspace history lifecycle tests**

Add tests proving:

- a request records query, mode, and IDs but no live modal entity;
- reopening builds a different picker entity;
- valid selections restore after asynchronous results arrive;
- invalid selections disappear;
- closing the project or required source buffer clears history;
- invoking through Command Palette and a zero-delay Which Key modal runs only
  after that modal closes.

Use an atomic construction counter and assert it increments from one to two on
reopen.

- [ ] **Step 2: Run Workspace tests and verify RED**

Run:

```bash
cargo test -p workspace reopen_last_picker -- --nocapture --test-threads=1
```

Expected: compilation fails because no reconstruction request API or action
exists.

- [ ] **Step 3: Define the request boundary**

In `picker_history.rs`, define:

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredPickerState {
    pub query: String,
    pub multi_select_enabled: bool,
    pub selected_item_ids: Vec<SharedString>,
}

pub trait ReopenablePickerRequest: 'static {
    fn is_valid(&self, workspace: &Workspace, cx: &App) -> bool;
    fn reopen(
        &self,
        state: StoredPickerState,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>>;
}

pub struct PickerHistoryEntry {
    pub request: Arc<dyn ReopenablePickerRequest>,
    pub state: StoredPickerState,
}
```

The request implementations store only stable values such as `ProjectPath`,
`BufferId`, UTF-16 point, action kind, and options.

- [ ] **Step 4: Add modal-close sequencing**

Emit `ModalClosedEvent` only after `ModalLayer::hide_modal` removes the active
modal. Store `pending_reopen: bool` in picker history. If
`ReopenLastPicker` fires while another modal is active, set the flag; execute
the request from the modal-closed subscription. Clear the flag when an
unrelated picker is explicitly opened.

- [ ] **Step 5: Connect Picker dismissal to history**

Extend `PickerDelegate` with:

```rust
fn reopen_request(
    &self,
    _state: &PickerRestorationState,
    _cx: &App,
) -> Option<Arc<dyn ReopenablePickerRequest>> {
    None
}
```

Before any supported picker emits `DismissEvent`, convert its restoration
state to `StoredPickerState` with `PickerItemId::into_shared_string` and call
`Workspace::record_picker_request`.
Delegates that return `None` remain non-reopenable.

- [ ] **Step 6: Write failing consumer reconstruction tests**

Add one focused test for each:

- File Finder restores query, mode, and surviving paths.
- Text Finder restores query, collapsed-group-independent file selections.
- Project Symbols reruns `Project::symbols`.
- Buffer Symbols resolves the recorded `BufferId` and reruns outline loading.
- LSP results rerun the original action and increment the server request
  counter.

- [ ] **Step 7: Implement typed requests in owning crates**

Define request structs next to their constructors:

```rust
struct FileFinderRequest { /* path-search options only */ }
struct TextFinderRequest { /* query/filter invocation options */ }
struct ProjectSymbolsRequest;
struct BufferSymbolsRequest { source_buffer_id: BufferId }
struct LspLocationsReopenRequest { request: LspLocationRequest }
```

Each `reopen` method upgrades the current Workspace, resolves current Project
state, constructs a new picker, and calls `restore_state`. It returns an error
for an actionable reconstruction failure and reports ordinary invalidation
through `is_valid == false`.

- [ ] **Step 8: Register action, command visibility, and keymaps**

Define `workspace::ReopenLastPicker`, register it on Workspace, leave it
visible in Command Palette, and bind `cmd-k cmd-p` on macOS and
`ctrl-k ctrl-p` on Linux/Windows as in upstream #59912. The action is a no-op
after invalid history is cleared.

- [ ] **Step 9: Run all history and consumer tests and commit**

Run:

```bash
cargo test -p workspace reopen_last_picker -- --test-threads=1
cargo test -p file_finder reopen -- --test-threads=1
cargo test -p search reopen -- --test-threads=1
cargo test -p project_symbols reopen -- --test-threads=1
cargo test -p outline reopen -- --test-threads=1
cargo test -p lsp_locations reopen -- --test-threads=1
```

Then:

```bash
git add crates/workspace crates/picker crates/file_finder crates/search crates/project_symbols crates/outline crates/lsp_locations crates/command_palette assets/keymaps
git commit -m "Reconstruct the last picker"
```

---

### Task 6: Verify and deliver the integrated Wave 6 change

**Files:**

- Modify if required by verification: files already listed in Tasks 1–5.
- Modify: PR body only after verification evidence exists.

**Interfaces:**

- Consumes: every Wave 6 feature slice.
- Produces: one verified integration PR without claiming ledger rows landed prematurely.

- [ ] **Step 1: Run affected crate suites**

Run serially where GPUI scheduling or shared globals require it:

```bash
cargo test -p picker -- --test-threads=1
cargo test -p file_finder -- --test-threads=1
cargo test -p search -- --test-threads=1
cargo test -p project_symbols -- --test-threads=1
cargo test -p outline -- --test-threads=1
cargo test -p lsp_locations -- --test-threads=1
cargo test -p editor -- --test-threads=1
cargo test -p project -- --test-threads=1
cargo test -p workspace -- --test-threads=1
cargo test -p settings_content
cargo test -p settings_ui
```

Expected: all Wave 6 tests pass. Compare any unrelated failure against a
contemporaneous `origin/main` run before classifying it as baseline.

- [ ] **Step 2: Run formatting, lint, and build gates**

```bash
cargo fmt --all -- --check
./script/clippy
cargo check -p flint
```

Expected: all commands exit zero.

- [ ] **Step 3: Build and verify the local macOS app**

Run:

```bash
./script/bundle-tmp-app
```

If it fails only at the known debug `release/remote_server` signing/gzip step,
copy the freshly built bundle with `ditto` from
`target/<target-triple>/debug/bundle/osx/Flint.app` to
`/tmp/Flint-Local.app`. Verify `/tmp/Flint-Local.app/Contents/MacOS/Flint`
exists and is executable.

- [ ] **Step 4: Perform the manual Wave 6 acceptance pass**

In `/tmp/Flint-Local.app`, verify:

- Project Symbols and Buffer Symbols preview and navigate exact ranges.
- Moving quickly rejects stale previews.
- Definition/reference/implementation results follow settings and action
  overrides without duplicate queries.
- File Finder and Text Finder preserve multi-selection through query changes.
- Text Finder headers own checkboxes and focused match rows toggle the parent.
- Reopen Last Picker reconstructs all supported picker families, restores
  valid state, and works through Which Key.
- Preview resizing and Task 6.1/6.2 behavior remain intact.

- [ ] **Step 5: Review the complete diff**

Run:

```bash
git diff origin/main...HEAD --check
git diff origin/main...HEAD --stat
git status --short
```

Confirm only the intended design, provenance, picker, search/navigation,
settings, keymap, and integration files changed; `.pi/` remains untracked.

- [ ] **Step 6: Push and open the integration PR**

Push:

```bash
git push -u origin feature/search-picker-modernization
```

Open a PR titled exactly:

```text
search: Modernize symbol and LSP pickers
```

The body must summarize symbol previews, single-query LSP presentation,
reconstructible history, and stable multi-select; list exact verification
commands; include any contemporaneous baseline comparison; and end with:

```text
Release Notes:

- Improved search and navigation pickers with symbol and LSP previews, reopening, and multi-select.
```

- [ ] **Step 7: Inspect CI and merge**

Wait for required checks. Fix attributable failures test-first on the same
branch. If workspace tests show the known three `editor::hover_links`
failures, compare the exact names to the current `main` run and document the
evidence before merging. Merge only when every new failure is resolved.

---

### Task 7: Mark Wave 6 landed and create the reset point

**Files:**

- Modify: `docs/superpowers/zed-upstream-ledger.md`
- Modify: `docs/superpowers/specs/2026-07-27-upstream-domain-wave-implementation-plan.md`

**Interfaces:**

- Consumes: the merged integration PR and its CI evidence.
- Produces: authoritative landed statuses, updated counts, and a compact handoff before Wave 7.

- [ ] **Step 1: Start the ledger branch from fresh main**

```bash
git fetch origin main
git switch -c docs/upstream-wave-6-ledger origin/main
```

- [ ] **Step 2: Update authoritative records**

Mark #59838, #59863, #59912, #59931, #60919, #61002, and #61069 `landed`.
Link the integration PR, summarize adaptations and excluded upstream behavior,
and record exact test/build/manual evidence. Update Tasks 6.3–6.6 in the
implementation plan without changing unrelated waves.

- [ ] **Step 3: Validate, commit, and open the ledger PR**

```bash
git diff --check
git add docs/superpowers/zed-upstream-ledger.md docs/superpowers/specs/2026-07-27-upstream-domain-wave-implementation-plan.md
git commit -m "Record search and picker modernization"
git push -u origin docs/upstream-wave-6-ledger
```

Open a PR titled `Record search and picker modernization` whose body ends:

```text
Release Notes:

- N/A
```

- [ ] **Step 4: Merge and verify main**

After required checks and merge, fetch `origin/main` and verify every Wave 6
row is terminal and the implementation plan records Tasks 6.3–6.6 complete.

- [ ] **Step 5: Create the compact history reset**

Use the `handoff` skill. Save the handoff to a `mktemp -t handoff-XXXXXX.md`
path after reading the empty file. Reference this plan, the design, ledger,
integration PR, ledger PR, final main commit, counts, baseline failures, and
Wave 7 as the next work. Do not duplicate the documents' contents.
