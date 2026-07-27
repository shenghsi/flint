# Upstream Domain-Wave Integration Design

**Date:** 2026-07-27
**Status:** Draft for written-spec review

No implementation is authorized by this design review alone.

Design owner: Codex.

## Problem

Flint is based on Zed commit
`6e9465a4288c332208643892e23b9d35d7be5c79` from 2026-06-06. Zed v1.12.0
contains 736 later commits that change 1,305 files. Flint has independently
changed 688 of those files while removing collaboration, accounts, and
cloud-owned product behavior and adding Agent Threads and managed remote agent
routes.

A merge or rebase would therefore mix useful editor improvements with removed
product areas and overwrite intentional Flint architecture. Replaying selected
commits in release or chronological order would reproduce the same problem in
smaller increments.

Zed stable and preview releases are also parallel stabilization branches.
Neither is a strict superset of the other, and fixes may be cherry-picked
between them. Release tags are useful discovery inputs but are not a reliable
unit of integration.

Flint needs a repeatable way to discover, evaluate, port, test, and track
upstream work without restoring removed Zed systems or allowing later feature
work to regress earlier safety guarantees.

## Goals

- Integrate important Zed changes from the fork point through the P0-P2
  backlog.
- Prioritize repository safety, user data, hangs, crashes, and resource leaks
  ahead of visible features.
- Preserve Flint's single-user, account-free, collaboration-free product
  boundary.
- Preserve the Direct and Tunneled remote route boundary.
- Deliver work through small, reviewable pull requests.
- Require regression or acceptance tests for every ported behavior.
- Reimplement broad features against Flint's current architecture instead of
  forcing obsolete upstream structure into the fork.
- Record every included, deferred, superseded, and intentionally excluded
  upstream change.
- Establish a reusable process for reviewing later Zed stable and preview
  releases.

## Non-goals

- Merging or rebasing Flint onto a later Zed tag.
- Replaying every upstream commit.
- Restoring Zed collaboration, calls, channels, accounts, billing, telemetry,
  hosted edit prediction, or cloud-owned onboarding.
- Matching Zed's version number or release cadence.
- Landing the full P0-P2 program in one pull request.
- Preserving upstream implementation details when Flint has a different
  architectural boundary.
- Changing the extension API, WIT namespaces, upstream service endpoints, or
  other external Zed compatibility interfaces without a specific compatibility
  requirement.

## Accepted Approach

Manage upstream integration as ordered domain waves backed by a PR-level
ledger.

The waves are:

1. Safety and data integrity.
2. Core stability and resource management.
3. Remote development and terminal reliability.
4. Agent Threads.
5. Git workflow.
6. Search and picker modernization.
7. Performance and settings compatibility.
8. Compatibility audit and recurring upstream maintenance.

Each wave begins with upstream validation and failing regression tests. Each
change is classified as a cherry-pick candidate, adaptation, or
reimplementation. A wave completes only after its scoped work is landed or
explicitly deferred and its completion gates pass.

Later waves may depend on earlier waves but may not weaken their guarantees.
Git UI work cannot bypass index-safety tests. Agent Threads work cannot bypass
remote route capabilities. Settings work cannot silently invalidate existing
Flint configuration.

## Alternatives Considered

### Process Zed releases in order

Review v1.6, then v1.7, and continue through v1.12.

This makes release-note auditing straightforward, but each release mixes
safety fixes, collaboration changes, hosted AI changes, and unrelated editor
features. Important fixes in later releases would wait behind lower-value
earlier work. Parallel stable and preview branches also make a release tag an
incomplete dependency boundary.

### Replay selected commits chronologically

Select useful commits and apply them in upstream commit order.

This preserves more historical dependencies, but it becomes a manual rebase.
Intermediate upstream implementations may already have later corrections, and
large architectural commits would carry unwanted Zed dependencies into Flint.

### Maintain only an informal issue list

Record desirable release-note entries without a common integration process.

This is lightweight, but it does not capture dependencies, conflict surface,
test coverage, intentional exclusions, or whether a later upstream fix
supersedes an earlier one. It cannot provide a reliable completion boundary.

## Upstream Integration Ledger

The ledger is the source of truth for the program. It is maintained at
upstream PR granularity because PRs carry intent, discussion, tests, and later
corrections more reliably than release-note bullets.

Each entry records:

- upstream PR and final commit;
- stable, preview, or main-branch provenance;
- domain wave;
- user impact and priority;
- affected Flint crates and files;
- prerequisite and superseding upstream PRs;
- overlap and conflict estimate;
- integration strategy;
- required unit, integration, GPUI, remote, and platform tests;
- feature branch and Flint pull request;
- status: proposed, investigating, implementing, landed, deferred,
  superseded, or excluded;
- exclusion or deferral reason; and
- follow-up observations for future upstream reviews.

Release tags seed the ledger. Before implementation, the assigned engineer
checks the upstream PR, its final commit, later changes to the same code, and
whether the fix exists on both current release branches.

## Integration Classifications

### Cherry-pick candidate

Use when a change is isolated, has little or no overlap with Flint, preserves
the same architectural boundary, and includes usable tests.

The commit is still reviewed before application. Classification does not
authorize an automatic cherry-pick.

### Adaptation

Use when upstream behavior is applicable but the affected code overlaps Flint
or relies on nearby structure that Flint changed. Port the behavior and tests
while preserving Flint naming, product boundaries, error propagation, and
remote routing.

### Reimplementation

Use for broad UI or architectural changes, changes with substantial overlap,
or changes rooted in systems Flint removed. Treat the upstream PR as a
behavioral specification and implement the smallest complete vertical slice
against Flint's current architecture.

## Standard Wave Workflow

Every wave uses the same workflow:

1. Refresh release and PR metadata.
2. Confirm the final upstream implementation and later corrections.
3. Resolve prerequisites and group only inseparable fixes.
4. Measure overlap and choose an integration classification.
5. Write a failing regression or acceptance test.
6. Implement the smallest complete behavior.
7. Run focused tests.
8. Run repository formatting and lint gates.
9. Build and verify a fresh local application bundle for user-visible or
   macOS-sensitive work.
10. Open a focused pull request with the required release-notes section.
11. Update the ledger after CI and review establish the result.
12. Run the wave regression suite before beginning the next wave.

Platform-specific process and resource fixes remain separate unless they share
one tested abstraction. Broad UI work lands as a foundation only when the same
pull request includes its first end-to-end consumer; dormant infrastructure is
not introduced in advance.

## Wave 1: Safety and Data Integrity

### Scope

- Canonicalize ambiguous repeated-line hunk placement so staging cannot corrupt
  the Git index.
- Ensure commit operations run `commit-msg` hooks and are not terminated by an
  unrelated short network timeout.
- Prevent agent-thread archival from deleting manually created worktrees.
- Trash symlinks rather than their targets.

### Initial upstream set

- Zed PR #60584: staging corruption with repeated lines.
- Zed PR #61185: commit hooks and long-running Git operations.
- Zed PR #58275: manual worktree preservation.
- Zed PR #58339: safe symlink trashing.

### Required tests

- Repeated identical diff lines stage and unstage the selected hunk without
  changing a different hunk or corrupting the index.
- Protected agent-created worktrees may be removed by their owner flow while
  manually created worktrees survive thread archival.
- Trashing a symlink removes the link and leaves the target intact.
- A rejecting `commit-msg` hook prevents the commit and presents its error.
- A slow Git operation is allowed to complete or fail with its real error.

### Completion gate

All repository-mutating paths covered by this wave pass their regression tests
before any Wave 5 Git UI feature begins.

## Wave 2: Core Stability and Resource Management

### Scope

- Fix workspace-opening and filesystem-watcher hangs.
- Preserve filesystem events on case-insensitive filesystems.
- Bound incoming language-server message queues.
- Stop agent diagnostics requests from hanging indefinitely.
- Fix process-tree, file-descriptor, and reaper leaks on Linux, macOS, and
  Windows.
- Evaluate large-worktree watcher performance changes that share prerequisites
  with the correctness fixes.

### Initial upstream set

- Zed PRs #58994 and #59045: workspace and watcher hangs.
- Zed PR #59714: missed case-insensitive filesystem events.
- Zed PR #58867: bounded LSP message queue.
- Zed PR #61176: bounded agent diagnostics collection.
- Zed PR #58683: Linux PTY descriptor leak.
- Zed PRs #59128 and #59156: macOS descriptors and process reaping.
- Zed PR #58885: Windows process-tree cleanup.
- Zed PR #59560: large-worktree watcher performance.

### Required tests

- Scheduler-controlled timeout tests for workspace and watcher completion.
- Case-only and canonical-path event tests on case-insensitive platforms.
- A synthetic flooding LSP cannot grow the pending queue without bound.
- An unresponsive diagnostics provider produces a visible, bounded failure.
- Child process trees terminate when their owning terminal, agent, debugger, or
  application scope ends.
- Repeated process spawn failure and terminal lifecycle tests do not leak
  descriptors.

### Completion gate

Focused tests pass on every affected platform. Platform-specific changes may be
deferred only with a recorded reason and must not block unrelated platforms.

## Wave 3: Remote Development and Terminal Reliability

### Scope

- Include changed paths in remote filesystem events.
- Keep client temporary-directory variables out of remote terminal
  environments.
- Restore remote agent terminals without crashing.
- Remove ghost projects during local and remote project transitions.
- Validate trust roots using the remote host's path style.
- Distinguish local and remote recent projects with identical checkout paths.
- Create remote worktrees from remote default branches.
- Avoid SSH failures caused by symlinked Git configuration.
- Restore remote code lens, language-only extensions, and terminal path links.

### Initial upstream set

- Zed PRs #58157, #58240, #58533, #59272, #60139, #53953, #59134, #57049,
  #59999, #56487, and #52537.

### Required tests

Every applicable behavior is exercised through both remote routes:

- Direct uses only the configured ambient remote executable and exposes no
  Flint-managed provisioning or credential controls.
- Tunneled uses only the pinned Flint-managed remote executable and routes its
  traffic through local Flint.

Tests cover reconnect, restoration, mixed local and remote projects, POSIX and
Windows path styles, default-branch worktree creation, remote extension
language registration, and opening terminal links inside and outside the
worktree.

### Completion gate

The direct and tunneled route matrices pass without weakening their capability
boundaries. Older remote-server compatibility is preserved or explicitly
migrated with a versioned capability.

## Wave 4: Agent Threads

### Scope

- Search within terminal threads.
- Run a configurable initialization command for new terminal threads.
- Request operating-system attention when agent work completes.
- Restore threads interrupted by application quit or update.
- Group threads correctly for bare-checkout worktrees.
- Insert dragged file paths in a form coding agents recognize.
- Open links while terminal applications have mouse reporting enabled.

### Initial upstream set

- Zed PRs #60292, #59374, #58779, #58962, #59968, #57747, and #60067.

Zed native-agent context compaction is tracked separately. It is not part of
this wave unless a Flint-owned Agent Threads requirement demonstrates that
external terminal agents need Flint to own context compaction.

### End-to-end audit

For each addition, audit and test:

- settings and defaults;
- exact Settings Editor JSON paths;
- panel visibility and actions;
- history, resume, and restoration;
- local and remote behavior;
- direct and tunneled capabilities; and
- provider-specific UI gating.

### Completion gate

Every supported coding agent has an explicit capability result for each new
behavior. Intentional omissions are represented and tested rather than inferred
from registry membership.

## Wave 5: Git Workflow

### Vertical slices

1. Add staged and unstaged grouping in the existing Git panel.
2. Add dedicated staged and unstaged multibuffers.
3. Add hunk-level stage, unstage, restore, and restore-all actions.
4. Add branch comparison.
5. Add dedicated single-file diff tabs.
6. Add split-mode commit-history diffs.
7. Add list/tree, sorting, and grouping controls with settings migration.

### Initial upstream set

- Zed PR #59884: staging groups.
- Zed PR #46541: staged and unstaged multibuffers.
- Related v1.6-v1.12 Git comparison, diff, and panel-control PRs recorded in the
  ledger during wave preparation.

### Required tests

- Wave 1 index-safety tests remain part of the wave suite.
- Every hunk action affects the selected hunk with repeated and nearby
  identical lines.
- Partial staging persists across panel and multibuffer refreshes.
- Remote repositories behave consistently with local repositories.
- Settings migrations preserve existing Flint user choices.

### Completion gate

All repository mutations pass Wave 1 regressions, and every new view exposes a
complete keyboard and pointer path.

## Wave 6: Search and Picker Modernization

### Vertical slices

1. Make picker layout resizable and add the first File Finder preview consumer.
2. Add Text Finder with live preview while preserving project-search state.
3. Add project-symbol and buffer-symbol previews.
4. Add filterable LSP result pickers for definitions, references, and
   implementations.
5. Add reopen-last-picker behavior.
6. Add File Finder and Text Finder multi-select.

### Initial upstream set

- Zed PR #59604: resizable picker foundation and finder previews.
- Zed PR #59838: LSP result pickers.
- Zed PRs #59931 and #60919: finder multi-select.

### Compatibility requirements

- Existing actions keep their behavior unless a migration is explicitly
  approved.
- Existing keybindings remain valid.
- Preview work is cancellable and cannot apply stale results after the query or
  picker changes.
- Large result sets remain bounded and responsive.
- Multi-select has deterministic keyboard and pointer semantics.

### Completion gate

Each slice is independently usable and tested. The foundation does not land
without its first end-to-end consumer.

## Wave 7: Performance and Settings Compatibility

### Performance scope

- Preserve structural sharing while applying large edits.
- Integrate editor, anchor, line-shaping, worktree-scanning, startup, and
  project-search improvements whose behavior is measurable and independent of
  removed Zed services.
- Retain the Wave 2 LSP and filesystem resource bounds.

### Settings decisions

Explicitly decide and test:

- migration from `git_panel.sort_by_path` to `git_panel.sort_by` and
  `git_panel.group_by`;
- whether Flint adopts Zed's changed format-on-save default;
- placement of language-model providers, external agents, and MCP servers in
  the Settings Editor;
- 32-bit extension-managed binary download support;
- TypeScript 7 language-server behavior; and
- npm 12 language-server installation compatibility.

No default or setting path changes silently. Existing configurations either
continue to work or receive deterministic migration behavior.

### Completion gate

Performance ports have a reproducible workload or regression test. Settings
changes have defaults, schema, Settings Editor, migration, and exact JSON path
coverage where applicable.

## Wave 8: Compatibility Audit and Maintenance

### Final audit

- Reconcile all Zed v1.6-v1.12 release entries against the ledger.
- Confirm that each relevant entry is landed, deferred, superseded, or
  intentionally excluded.
- Run macOS, Linux, Windows, local, Direct remote, and Tunneled remote smoke
  suites.
- Record the resulting upstream baseline by commit and release tags.
- Document remaining known differences that affect extension or remote-server
  compatibility.

### Recurring process

For every later Zed stable and preview release:

1. Ingest release-note PRs into a temporary review queue.
2. Deduplicate stable and preview entries by upstream PR.
3. Check later corrections on Zed main.
4. Classify against Flint domains and product boundaries.
5. Immediately schedule new safety or data-integrity regressions.
6. Add accepted work to the permanent ledger.
7. Record intentional exclusions with reasons.

### Completion gate

The initial program has no unclassified P0-P2 entries, and the recurring review
has an owner, cadence, and recorded upstream baseline.

## Testing and Validation

### Per-pull-request gates

- Write the regression or acceptance test before production changes.
- Run the smallest relevant crate tests while iterating.
- Run `cargo fmt --all -- --check`.
- Run `./script/clippy` before pushing Rust changes.
- Run platform or remote integration tests required by the ledger entry.
- Build `/tmp/Flint-Local.app` for user-visible or macOS-sensitive changes and
  verify that the bundle contains the fresh build.

### Per-wave gates

- Run all tests introduced by the wave.
- Run inherited regression suites from earlier waves that protect the modified
  subsystem.
- Confirm the ledger matches the landed implementation and CI result.
- Perform a focused manual smoke test for user-visible workflows.

### Program gates

- Run the cross-platform and remote route matrix.
- Audit settings paths and migrations.
- Audit external Zed extension compatibility.
- Confirm removed Zed product systems have not re-entered the build or UI.

## Error Handling

Backported async failures propagate to the UI layer with meaningful context.
The integration must not introduce ignored fallible operations, hidden
timeouts, or success states after partial failure.

Remote capability mismatches use explicit fallback or unsupported behavior.
They do not silently switch Direct work to Flint-managed execution or Tunneled
work to an ambient executable.

Ledger automation failures leave the previous ledger intact and surface the
release or PR that could not be inspected. An incomplete metadata refresh does
not mark entries superseded or excluded.

## Pull Request Boundaries

A pull request normally contains one upstream behavior or one inseparable fix
bundle. Separate pull requests are required when changes:

- affect different platforms through different mechanisms;
- combine safety fixes with visible feature work;
- mix remote route policy with UI;
- require independent rollback; or
- would make the release note ambiguous.

Every pull request uses a clear imperative title and ends its body with the
required `Release Notes:` section. Documentation-only process changes use
`- N/A`.

## Risks and Mitigations

### Hidden upstream prerequisites

Later upstream code may assume refactors not mentioned in release notes.

Mitigation: inspect final PR commits, parent changes, and later corrections
before writing the Flint regression test.

### Behavioral drift during adaptation

A conflict-free patch can still violate Flint's product or remote boundaries.

Mitigation: classify by behavior and capability, not patch applicability, and
require Flint-specific acceptance tests.

### Program starvation

Large visible features could delay safety work, or the long program could
remain permanently unfinished.

Mitigation: ordered completion gates, small PRs, explicit deferrals, and a
ledger with no unclassified entries.

### Regression-suite growth

Running every accumulated test on every pull request could become too slow.

Mitigation: keep focused per-PR suites, subsystem wave suites, and a broader
program matrix at wave and release boundaries.

### Reintroduction of removed Zed systems

Agent, settings, and workspace changes can carry account, collaboration, or
hosted-service assumptions.

Mitigation: reimplement broad changes against Flint abstractions and audit the
build graph and user-visible strings at each wave boundary.

## Success Criteria

- No P0-P2 upstream entry from the initial v1.6-v1.12 review remains
  unclassified.
- Safety and data-integrity regressions land before dependent Git features.
- Remote changes preserve and test Direct and Tunneled boundaries.
- Agent Threads additions expose explicit capabilities and tested omissions.
- Broad UI features land as independently usable vertical slices.
- Existing Flint settings are preserved or deterministically migrated.
- Every repository change is delivered through a focused pull request.
- Future stable and preview releases can be triaged without repeating the
  initial audit from scratch.
