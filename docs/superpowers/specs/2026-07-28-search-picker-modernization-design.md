# Search and Picker Modernization Design

**Date:** 2026-07-28

**Status:** Approved

**Scope:** Tasks 6.3 through 6.6 in
`2026-07-27-upstream-domain-wave-implementation-plan.md`

## Objective

Complete the remaining search and picker modernization work as one integrated
Wave 6 change:

- preview project and buffer symbols;
- show definitions, references, and implementations in previewable pickers;
- reconstruct the most recently dismissed supported picker;
- add stable, accessible multi-selection to File Finder and Text Finder.

The implementation must extend the preview infrastructure landed in Tasks 6.1
and 6.2, preserve Flint's existing search and navigation behavior by default,
and avoid importing upstream collaboration, account, or cloud behavior.

## Upstream provenance

The implementation adapts behavior from these merged Zed changes:

| Behavior | Upstream PR | Merge commit |
| --- | --- | --- |
| Project symbol preview | #59863 | `9448417157a9e690d87213c89ea9913803373b4f` |
| Buffer symbol preview | #61069 | `1d2a4b3f7f194184dccadfa7a091d16a06482752` |
| LSP result pickers | #59838 | `63692b8b4724357fa63d6318b45f3c3fee6f672a` |
| Reopen last picker | #59912 | `8186af99a347dfa9f9fd5af88da419b97b9727fa` |
| Reopen with Which Key | #61002 | `4ebc1545d299b1270bc76813fa841357ee711b19` |
| Picker multi-select | #59931 | `94b6d377badf9c2202850b551c4700a54b83895f` |
| Multi-select controls | #60919 | `90b3aa0b3bd3b453775b11a386907c7ac9acd997` |

This is a reimplementation against Flint's current architecture. In
particular, Flint will not retain and reveal a dismissed live modal as upstream
#59912 does.

## Architecture

### Picker responsibilities

`Picker` owns mechanics that are independent of any result domain:

- preview layout, sizing, and refresh;
- one focused result;
- an optional multi-selection keyed by stable delegate-provided identities;
- deterministic keyboard and pointer routing;
- reconciliation after filtering, reordering, or asynchronous refresh;
- generic multi-select controls and accessible state.

Picker state must never identify selected entries only by their current result
indices. A supporting delegate supplies stable identities and defines how
multiple selected entries are confirmed.

### Consumer responsibilities

Each picker-owning crate retains its domain behavior:

- File Finder identifies and opens project paths.
- Text Finder identifies matched files and opens those files.
- Project Symbols resolves symbols to current project buffers.
- Buffer Symbols maps outline positions into its existing multibuffer.
- The LSP result picker presents a normalized snapshot returned by one LSP
  query.

Consumers opt into multi-select explicitly. Symbol and LSP pickers remain
single-select because Wave 6 does not require multi-confirm behavior for them.

### Workspace responsibilities

Workspace owns picker lifecycle history through a typed,
reconstructible request. A request contains:

- the picker kind;
- the invocation context needed to issue it again;
- action options;
- the previous query;
- stable selected identities where the consumer supports restoration.

The request does not contain a live view, entity, delegate, task, buffer
snapshot, or server response. Reopening always creates a new picker and reruns
the relevant project or LSP operation.

Picker construction remains in the owning crate. Workspace coordinates
reconstruction through registered request handlers rather than depending on
delegate implementations.

## Symbol previews

Project Symbols uses the existing picker preview contract with a symbol-backed
preview request. The preview resolves the selected symbol against the current
project, opens its buffer asynchronously, and highlights its declaration range.
Each new selection cancels or invalidates the prior load so late completion
cannot replace the current preview.

Buffer Symbols derives the source buffer and anchor range from the active
editor's multibuffer. It previews only when both ends of the symbol range map
to the same underlying buffer. Editors without a project retain the current
non-preview picker.

Both paths support local, remote, and unsaved buffers through existing Project,
Buffer, and MultiBuffer APIs. They do not introduce separate remote transport.

## LSP result pickers

Definitions, references, and implementations perform one LSP query per action
invocation. The resulting locations are normalized and deduplicated before
presentation. Navigation between picker matches only updates the preview and
must not issue another LSP request.

Presentation follows these rules:

1. No usable results show existing user-facing feedback.
2. One usable result navigates directly.
3. Multiple usable results open either a multibuffer or picker.
4. A per-action `open_results_in` value overrides the global setting.
5. With no override, `editor.lsp_results_location` chooses the presentation.

`editor.lsp_results_location` accepts `multibuffer` and `picker`.
`multibuffer` is the default to preserve existing behavior. The setting is
available at that exact JSON path in Settings Editor. Existing action payloads
without `open_results_in` remain valid.

The picker groups results by file and filters the captured result snapshot by
path and source text. Missing or unavailable targets may remain visible with
an unavailable state, but cannot be confirmed. Remote results use the existing
project buffer-opening path.

## Reopen Last Picker

When a supported picker is dismissed, it records its latest reconstructible
request, query, and stable selections. `workspace::ReopenLastPicker` dispatches
that request after any transient command or Which Key modal has closed.

Reconstruction restores the prior query and any stable selections that remain
valid in the new result set. It discards vanished entries rather than
substituting current indices. File Finder, Text Finder, Project Symbols, Buffer
Symbols, and LSP result pickers participate.

History is cleared when reconstruction is no longer meaningful, including
when:

- its workspace or project is gone;
- an originating editor or buffer required by the request is gone;
- a required project or language-server capability is unavailable;
- the picker explicitly invalidates its request.

Reopening an LSP picker reruns the original action from its recorded buffer
position and options. It never reuses prior server results.

## Multi-selection

File Finder selections are stable project paths. Text Finder selections are
stable matched files, not individual matching lines. This makes multi-confirm
open each selected file once and remains meaningful when result groups expand
or collapse.

Multi-select behavior is:

- regular confirmation retains existing single-result behavior;
- the platform secondary modifier plus click toggles an entry;
- the keyboard toggle selects or deselects the focused entry and advances
  deterministically;
- an explicit control in the search row exposes multi-select mode;
- selected entries render with the shared checkbox component and accessible
  labels;
- filtering, reordering, and asynchronous refresh preserve identities that
  still exist;
- disabled or vanished entries cannot be newly selected and are removed during
  reconciliation;
- dismissal clears live selection state after recording reopenable state.

Multi-confirm opens valid selections in deterministic result order. Individual
open failures are surfaced through Flint's existing error-notification path;
they are not silently discarded.

## Error handling

Asynchronous buffer and LSP errors propagate to existing workspace UI
notifications. Preview failures show an unavailable preview without closing
the picker. Stale asynchronous completions are rejected.

Reconstruction failure clears the unusable history entry and reports an error
only when the failure is actionable. Ordinary invalidation, such as closing a
project, is a no-op when the reopen action is invoked.

Fallible operations are propagated or logged explicitly. The implementation
must not use panicking indexing, `unwrap()`, or `let _ =` to discard errors.

## Testing

Implementation follows red-green-refactor for each behavior.

Picker tests cover:

- stable selection across filtering, reordering, and asynchronous refresh;
- disabled and vanished entries;
- keyboard and pointer routing;
- deterministic confirmation order;
- narrow preview layouts;
- controls, focus, and accessible labels;
- cleanup and recorded state on dismissal.

Symbol tests cover:

- project and buffer symbol preview ranges;
- local, remote, and unsaved buffers;
- multi-buffer range validation;
- stale preview completion.

LSP tests cover:

- definitions, references, and implementations;
- zero, one, and multiple results;
- multiple servers and duplicate locations;
- missing files and remote results;
- global settings and per-action overrides;
- compatibility of existing action payloads;
- exactly one query while navigating picker results;
- the exact Settings Editor JSON path.

Reopen tests cover:

- File Finder, Text Finder, both symbol pickers, and LSP result pickers;
- restored queries and valid stable selections;
- discarded invalid selections;
- closed projects, missing buffers, and missing capabilities;
- invocation through the command palette and Which Key.

Wave completion runs the picker, File Finder, search, editor, project,
project-symbol, outline, settings, Settings Editor, and workspace suites.
It also runs formatting, `./script/clippy`, the app build and bundle workflow,
and the manual checks required by the upstream domain-wave implementation plan.

## Delivery

Tasks 6.3 through 6.6 are delivered in one integration pull request. The
ledger gains the resolved #59863, #61069, #59912, and #61002 provenance before
the implementation is declared complete, and all Wave 6 rows are updated only
after the integration PR lands.
