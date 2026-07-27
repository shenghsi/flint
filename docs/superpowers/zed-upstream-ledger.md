# Zed Upstream Integration Ledger

**Flint fork point:** `6e9465a4288c332208643892e23b9d35d7be5c79`
**Initial reviewed range:** Zed v1.6 through v1.12, plus selected
v1.13.0-pre safety fixes
**Program design:**
`docs/superpowers/specs/2026-07-27-upstream-domain-wave-design.md`
**Implementation plan:**
`docs/superpowers/specs/2026-07-27-upstream-domain-wave-implementation-plan.md`

This ledger is the source of truth for selective Zed integration. Release notes
are discovery inputs. Upstream pull requests are the integration unit.

Before implementation, prove that the final upstream commit is absent from the
fork-point ancestry, inspect later corrections on Zed main, reproduce the
missing behavior in current Flint, and choose the integration strategy from
current code rather than patch applicability.

## Status definitions

- `proposed`: accepted into the program but not yet investigated against
  current Flint.
- `investigating`: ancestry, applicability, dependencies, or final upstream
  behavior is being resolved.
- `implementing`: a Flint branch and pull request are in progress.
- `landed`: the Flint pull request passed CI and review and is merged.
- `deferred`: applicable work intentionally postponed with a recorded reason.
- `superseded`: a later upstream or Flint change provides the required
  behavior.
- `baseline-present`: the final upstream commit is already an ancestor of the
  Flint fork point.
- `excluded`: the change does not apply to Flint, with the product or
  architecture boundary recorded.

Only merged work may be marked `landed`.

## Integration strategies

- `candidate`: isolated upstream change that may be cherry-picked after
  dependency and behavior review.
- `adapt`: port the tested behavior into overlapping Flint code.
- `reimplement`: use the upstream pull request as a behavioral specification
  and build a Flint-native vertical slice.
- `audit`: prove applicability before deciding whether code is required.
- `none`: no implementation because the change is already present, superseded,
  or excluded.

## Product-boundary exclusions

These categories remain excluded unless a later design proves that Flint
depends on a specific external interface:

| Category | Status | Reason |
| --- | --- | --- |
| Zed collaboration, calls, channels, and shared projects | `excluded` | Flint has no collaboration backend. |
| Zed accounts, sign-in, billing, and hosted model access | `excluded` | Flint is single-user and account-free. |
| Zed-hosted edit prediction and telemetry | `excluded` | Flint has no cloud service of its own. |
| Zed provider catalog, merchandising, and cloud onboarding | `excluded` | These describe Zed-owned products rather than Flint behavior. |

The `zed_extension_api` crate name, `zed:api-version` and
`zed:extension/*` WIT namespaces, `ZED_*` environment variables, upstream Zed
service endpoints, and `zed-industries` dependencies remain compatibility
interfaces and are not covered by these exclusions.

## Wave 1: Safety and data integrity

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P0 | [#60584](https://github.com/zed-industries/zed/pull/60584) | `eb962794a33cbd414c6a343a8bb1136c94fe6901` | v1.13.0-pre notes | Absent | `crates/buffer_diff`, `crates/git_ui` | `adapt` | None | Task 1.1 repeated-line stage/unstage and index integrity | `landed` | [#68](https://github.com/shenghsi/flint/pull/68) | Canonicalize ambiguous hunk placement. |
| P0 | [#61185](https://github.com/zed-industries/zed/pull/61185) | `2a983bca8616c6d8ad111667a5ed6064cc3cbb61` | v1.13.0-pre notes | Absent | `crates/askpass`, `crates/git`, `crates/project`, `crates/remote` | `adapt` | #60584 | Task 1.2 rejecting `commit-msg` hook and slow operation | `landed` | [#70](https://github.com/shenghsi/flint/pull/70) | Run native hooks, keep the timeout on SSH connection establishment, and terminate canceled Git children. |
| P0 | [#58275](https://github.com/zed-industries/zed/pull/58275) | `29622911de00305340012f798f17c04a274f4b31` | v1.8 notes | Absent | Removed `agent_ui` archival flow | `none` | None | Task 1.3 source and deletion-call-site evidence | `excluded` | [#69](https://github.com/shenghsi/flint/pull/69) | Flint has no thread archival path that deletes worktrees. |
| P0 | [#58339](https://github.com/zed-industries/zed/pull/58339) | `381f2f4977b2c104190073af01ae9762ecdd9c9e` | v1.6 notes | Present | `crates/fs` | `none` | None | Existing symlink trash behavior | `baseline-present` | — | Release-note timing is later than the commit and does not indicate absence. |

Task 1.3 exclusion evidence (reviewed 2026-07-27): upstream #58275
protects the worktree deletion flow in the removed
`crates/agent_ui/src/thread_worktree_archive.rs` module. Flint's
`AgentThreadStore::begin_shutdown` only removes the live terminal entry,
terminates its local or remote process, and releases its egress lease.
`agent_threads` has no archive action, no `git_ui` dependency, and no
worktree-deletion call site. Flint's production `Repository::remove_worktree`
callers are limited to the user-initiated worktree picker and rollback of a
failed worktree creation. Importing upstream's created-worktree registry,
creation-time RPC, and archive verification would therefore add no reachable
safety behavior to Flint.

## Wave 2: Core stability and resource management

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P0 | [#58994](https://github.com/zed-industries/zed/pull/58994) | `e569b955f55b1a2cee026a084eb16578c6e2e30a` | v1.6 notes | Absent | `crates/fs`, workspace dependency | `adapt` | Resolve final `notify` stack | Task 2.1 watcher registration completion | `landed` | [#71](https://github.com/shenghsi/flint/pull/71) | Selected `notify` revision `0890bbb8ca40a4b5d1f67031698dd7918b37d991`, which includes the later deterministic FSEvents startup and stream-rebuild corrections. |
| P0 | [#59045](https://github.com/zed-industries/zed/pull/59045) | `bae8065e2fbc0331cdd2240ea249a25ab162c77b` | v1.6 notes | Absent | `crates/fs`, workspace dependency | `adapt` | #58994 | Task 2.1 watch/unwatch hang regression | `landed` | [#71](https://github.com/shenghsi/flint/pull/71) | Uses the final compatible `notify` revision and treats an already-removed native watch as a successful removal. |
| P0 | [#59714](https://github.com/zed-industries/zed/pull/59714) | `37679b98a558a0c8a46b46761b677494fdbfb011` | v1.8 notes | Absent | `crates/fs` | `adapt` | #58994, #59045 | Task 2.1 case-only and canonical-path events | `landed` | [#71](https://github.com/shenghsi/flint/pull/71) | Adds case- and Unicode-normalization-aware registration identity and indexed event routing. |
| P1 | [#59560](https://github.com/zed-industries/zed/pull/59560) | `3723eef7f673300cc6818ae7a327fc6a30952068` | v1.8 notes | Absent | `crates/fs` | `adapt` | #58994, #59045 | Task 2.1 burst-event responsiveness | `landed` | [#71](https://github.com/shenghsi/flint/pull/71) | Includes later #60098 rescan coalescing and #60662 watch-limit cooldown retry corrections. |
| P0 | [#58867](https://github.com/zed-industries/zed/pull/58867) | `7f6f93c089e5ed50342e2c4288a71545ddaf4f5d` | v1.7 notes | Absent | `crates/lsp` | `adapt` | None | Task 2.2 bounded queue and blocked-sender shutdown | `landed` | [#72](https://github.com/shenghsi/flint/pull/72) | Flint retained the vulnerable unbounded stdout channel. The final upstream behavior remains a lossless 128-message queue; the only later edit was an unrelated `BufReader` dependency cleanup. |
| P1 | [#61176](https://github.com/zed-industries/zed/pull/61176) | `c7148c8190d7740d51f6e380f993ba589aaf0751` | v1.12 notes | Absent | Deleted `crates/agent` diagnostics tool; no `agent_threads` consumer | `none` | #58867 | Task 2.3 source and reachability evidence | `excluded` | [#73](https://github.com/shenghsi/flint/pull/73) | Zed's only caller is its native diagnostics tool. Flint commit `c8c9f55255` deleted that tool and the `agent`/`agent_ui` crates; the retained `pull_workspace_diagnostics_once` method has no caller, and Agent Threads has no diagnostics wait path. |
| P0 | [#58683](https://github.com/zed-industries/zed/pull/58683) | `7854e4535ddb8fee8f0d72b46bbf98c28a1f4463` | v1.10 notes | Absent | `crates/terminal/src/pty_info.rs` | `adapt` | None | Task 2.4 bounded process map and Linux procfs descriptor churn | `landed` | [#74](https://github.com/shenghsi/flint/pull/74) | Flint retained sysinfo 0.37's full-system, task-enabled snapshot and stale foreground entries. Later #61467 changes process-group shutdown, not this cache ownership, and remains out of scope. |
| P0 | [#59128](https://github.com/zed-industries/zed/pull/59128) | `e1bfcf85db56f75a2f6d67143aad2da22c3d2240` | v1.8 notes | Absent | `crates/util/src/command/darwin.rs` | `adapt` | Resolve #59156 and #59358 | Task 2.5 failed-spawn descriptor test | `landed` | [#75](https://github.com/shenghsi/flint/pull/75) | Flint's raw pipe descriptors leaked on every failed `posix_spawn`; adopt them into `File` immediately for error-path cleanup. |
| P0 | [#59156](https://github.com/zed-industries/zed/pull/59156) | `d4cc8d240965e1b3c86b1132df2278e4d01333f6` | v1.8 notes | Absent | workspace async-process patch, `crates/util/src/command/darwin.rs` | `adapt` | Review #59358 | Task 2.5 repeated child-exit reaping | `landed` | [#75](https://github.com/shenghsi/flint/pull/75) | Current Zed still pins reaper revision `0b6d6713570af61806e1e5cb40e0f757cb93fd9d`; adopt custom-spawn PIDs into that reaper. |
| P0 | [#59358](https://github.com/zed-industries/zed/pull/59358) | `a873cf402c8d5ffa13ab54efd29ccd4df59c7e46` | later main correction | Absent | Standalone GPUI platform dependency cleanup | `none` | #59128, #59156 | Task 2.5 final process ownership audit | `excluded` | [#75](https://github.com/shenghsi/flint/pull/75) | This removes `util` from separately consumed GPUI platform crates but does not remove the application-wide async-process patch. It is not part of Flint's spawn/reaper ownership fix. |
| P0 | [#58885](https://github.com/zed-industries/zed/pull/58885) | `c642b422deaf6119aad2943ea22ec3074f39ef3c` | v1.8 notes | Absent | `crates/util`, `crates/terminal`, active process launchers | `adapt` | None | Task 2.6 direct, kill-on-drop, detached, and ConPTY child/grandchild termination | `landed` | [#76](https://github.com/shenghsi/flint/pull/76) | Shared Job Object ownership covers native kernels, language servers, managed helpers, terminals, and local Agent Threads. Retained debugger sources are not workspace members. No later `util::process` correction exists on Zed main. |

Wave 2 completed on 2026-07-27 at Flint commit
`096a7a3d9d6a20bdfe9e7ad090aa2a6d19f7bfc6`. The process-resource workflow
passed its `util` and `terminal` tests and clippy checks on Linux and Windows,
including Windows direct-child, kill-on-drop, detached-child, and ConPTY
process-tree coverage. The macOS spawn and reaper tests passed in PR #75, and
the Linux PTY ownership tests passed in PR #74. The filesystem watcher and
bounded LSP queue regressions passed in PRs #71 and #72. No applicable Wave 2
platform behavior remains deferred or unclassified.

## Wave 3: Remote development and terminal reliability

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | [#58157](https://github.com/zed-industries/zed/pull/58157) | `36a3a2a784fb51f7e58bc6f90e9e0202587ad4bb` | v1.8 notes | Absent | `crates/worktree`, `crates/proto`, `crates/remote_server` | `adapt` | None | Task 3.1 create, modify, rename, delete, protocol round-trip, and remote editing events | `landed` | [#78](https://github.com/shenghsi/flint/pull/78) | The existing wire format carries updated paths and removed entry IDs. The shared remote-worktree client now emits those paths without changing route or protocol capabilities. |
| P1 | [#59272](https://github.com/zed-industries/zed/pull/59272) | `338530f2e9b36ea05cc27db96e8f2270bd32cd12` | v1.8 notes | Absent | `crates/recent_projects`, `crates/workspace` | `adapt` | #58157 | Task 3.2 remote picker, close, and remove transitions | `landed` | [#80](https://github.com/shenghsi/flint/pull/80) | Flint already reopens restored remote groups through its shared remote-project path. The two remaining local fallback sites now avoid creating ghost workspaces for unloaded remote neighbors. |
| P1 | [#53953](https://github.com/zed-industries/zed/pull/53953) | `2838ea3f59458fc550d844e78fb4fec8eaf39fa3` | v1.12 notes | Absent | `crates/recent_projects` | `adapt` | #58157 | Task 3.2 local/remote same-path and different-host filtering | `landed` | [#80](https://github.com/shenghsi/flint/pull/80) | Persisted workspace locations are compared with open folders by semantic remote connection identity. Direct and Tunneled remain transport modes for the same checkout, and the persistence key did not change. |
| P1 | [#60139](https://github.com/zed-industries/zed/pull/60139) | `7b128f9263396555041d3c416ba75cf7554fe1a4` | v1.10 notes | Absent | `crates/workspace` | `none` | None | Task 3.3 source and reachability evidence | `excluded` | [#82](https://github.com/shenghsi/flint/pull/82) | Zed's fix validates its editable trust-path input, which Flint does not contain. Flint derives the parent scope without local-platform absolute-path validation. |
| P1 | [#59134](https://github.com/zed-industries/zed/pull/59134) | `503292376ed04fca814c8b4533b38f90863675fb` | v1.12 notes | Absent | `crates/proto`, `crates/project`, `crates/remote_server`, `crates/git_ui` | `adapt` | None | Task 3.4 qualified and unqualified remote default-branch RPC | `landed` | [#83](https://github.com/shenghsi/flint/pull/83) | `include_remote_name` now crosses the shared remote Git RPC, so the existing worktree picker can create from `origin/main`. Direct and Tunneled do not branch in this project RPC. |
| P1 | [#57049](https://github.com/zed-industries/zed/pull/57049) | `5a7d414a23938c5efb674d0c2948813e37448eea` | v1.11 notes | Absent | `crates/fs` | `adapt` | Wave 2 watcher stack | Task 3.5 native, poll, and root symlink-target parent selection | `implementing` | — | Keep watching the symlink target, but skip its parent when registration would use a recursively scanning poll watcher. The shared remote-server startup path does not branch on Direct or Tunneled Agent Threads routing. |
| P1 | [#59999](https://github.com/zed-industries/zed/pull/59999) | `0deb6c0deaa91d12bafae3b76d41c965bd4d7615` | v1.10 notes | Absent | `crates/project` | `adapt` | None | Task 3.6a remote code-lens resolution | `proposed` | — | Test Direct and Tunneled routes. |
| P1 | [#56487](https://github.com/zed-industries/zed/pull/56487) | `0b458e53a5b52fb205f8420db3f12315e9268915` | v1.10 notes | Absent | `crates/extension_host`, `crates/extension` | `adapt` | None | Task 3.6b language-only remote extension | `proposed` | — | Preserve Zed extension compatibility identifiers. |
| P1 | [#52537](https://github.com/zed-industries/zed/pull/52537) | `776585038e56672e2bb5ee48899c79c654aeaba2` | v1.10 notes | Absent | `crates/terminal_view` | `adapt` | None | Task 3.6c paths outside worktrees | `proposed` | — | Include non-ASCII rows and columns. |
| P1 | [#58240](https://github.com/zed-industries/zed/pull/58240) | `513e2b2ee3d59373bbafc5995119088d3ea2d368` | v1.6 notes | Present | removed native agent terminal path | `none` | None | Source evidence | `baseline-present` | — | Local-only sandbox terminal temporary directories. |
| P1 | [#58533](https://github.com/zed-industries/zed/pull/58533) | `44fb295593d5e1c10b61a64bc0be2fc43e49f5b1` | v1.7 notes | Present | removed native `agent_ui` | `none` | None | Source evidence | `baseline-present` | — | Remote native-agent terminal restoration panic. |

Task 3.3 exclusion evidence (reviewed 2026-07-27): upstream #60139 changes
Zed's editable `Folder to trust` field so its absolute-path validation uses the
remote worktree store's path style instead of the client's platform.
Flint's security modal has no editable trust-path field or
`Path::is_absolute` rejection path. Its checkbox derives the parent directly
from the restricted worktree path, and `TrustedWorktrees::trust` already uses
`WorktreeStore::path_style()` when checking absolute paths. Direct and
Tunneled projects both reach this shared modal and trust store, so importing
the upstream input validator would add an otherwise absent UI feature rather
than fix reachable Flint behavior.

All applicable Wave 3 tests must cover both remote routes. Direct uses only the
configured ambient remote executable. Tunneled uses only the pinned
Flint-managed executable and local traffic tunnel.

## Wave 4: Agent Threads

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | [#60292](https://github.com/zed-industries/zed/pull/60292) | `4aa8ad9742b1ee948d64429a5814d9b9a861350a` | v1.11 notes | Absent | `crates/agent_threads`, `crates/terminal_view` | `reimplement` | None | Task 4.1 local, restored, and remote thread search | `proposed` | — | Reuse terminal search instead of restoring `agent_ui`. |
| P1 | [#59374](https://github.com/zed-industries/zed/pull/59374) | `fd5d42dd55fcc185c27920e88aab21c077be5738` | v1.8 notes | Absent | `crates/agent_threads`, settings crates | `reimplement` | None | Task 4.2 PTY ordering, failure, and route matrix | `proposed` | — | Exact setting path requires capability design. |
| P1 | [#58779](https://github.com/zed-industries/zed/pull/58779) | `905e955a702707cd81a2e5bae9b381a7a9c7f614` | v1.12 notes | Absent | `crates/gpui*`, `crates/agent_threads` | `adapt` | Audit existing desktop notification | Task 4.3 inactive-window attention | `proposed` | — | Avoid duplicate completion notifications. |
| P1 | [#58962](https://github.com/zed-industries/zed/pull/58962) | `620ceaaaca40b346736660f12eefce38e235cb59` | v1.8 notes | Absent | removed native agent database; inspect Agent Threads persistence | `audit` | Flint history index and restoration | Task 4.4 quit during output and restore | `proposed` | — | Expected supersession or exclusion. |
| P1 | [#59968](https://github.com/zed-industries/zed/pull/59968) | `ea87b0579464067eb45a1c1a1f2c1bdb80af7e1f` | v1.11 notes | Absent | `crates/agent_threads`, `crates/agent_history` | `adapt` | None | Task 4.5 bare checkout identity | `proposed` | — | Test live and historical grouping. |
| P2 | [#57747](https://github.com/zed-industries/zed/pull/57747) | `7e0f63412c60008f9dae7fcf65fc6ab6d7e0f957` | v1.11 notes | Absent | `crates/terminal_view` | `adapt` | None | Task 4.6 shell escaping and agent recognition | `proposed` | — | Cover Codex, Claude, and Pi references. |
| P2 | [#60067](https://github.com/zed-industries/zed/pull/60067) | `f5c975162cf217f2c9cd1a2c1192eb2bb4653cdc` | v1.11 notes | Absent | `crates/terminal`, `crates/terminal_view`, settings crates | `adapt` | None | Task 4.7 mouse reporting and link opening | `proposed` | — | Preserve ordinary terminal clicks. |

Every Wave 4 entry requires explicit settings/defaults, Settings Editor,
visibility, actions, history/resume, local, Direct, Tunneled, and
provider-capability results.

## Wave 5: Git workflow

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | [#59884](https://github.com/zed-industries/zed/pull/59884) | `65e1c5af258d4c80036467d583691f3f9ded0897` | v1.12 notes | Absent | `crates/git_ui`, settings crates | `adapt` | Wave 1 complete | Task 5.1 staging groups and partial state | `proposed` | — | First Git workflow slice. |
| P1 | [#46541](https://github.com/zed-industries/zed/pull/46541) | `c31b2b0dc7180247b2981eb084594efaf11ee396` | v1.11 notes | Absent | `crates/git_ui`, `crates/workspace` | `reimplement` | #59884, Wave 1 complete | Tasks 5.2-5.3 multibuffers and hunk actions | `proposed` | — | Broad change; preserve Wave 1 safety tests. |
| P2 | [#59043](https://github.com/zed-industries/zed/pull/59043) | `076fd14c88336fca9d2a4093452f3820c27453dd` | v1.9 notes | Absent | `crates/git_ui`, settings crates | `adapt` | #59884 | Task 5.4 view options and settings migration | `proposed` | — | Replaces `git_panel.sort_by_path`. |
| P2 | [#57886](https://github.com/zed-industries/zed/pull/57886) | `83c52e38785efbed6c0b9013cae1866f72218921` | v1.6 notes | Present | `crates/git_ui` | `none` | None | Existing compare-with-branch coverage | `baseline-present` | — | Do not reimplement. |
| P2 | [#56152](https://github.com/zed-industries/zed/pull/56152) | `5d3b9e467e9b789fa4422c4cf9208c497838d43b` | v1.6 notes | Present | `crates/git_ui` | `none` | None | Existing dedicated diff coverage | `baseline-present` | — | Do not reimplement. |
| P2 | [#58163](https://github.com/zed-industries/zed/pull/58163) | `52956d93e4cf219b078a1c9dd1f70ee959ba0089` | v1.6 notes | Present | `crates/git_ui` | `none` | None | Existing split commit view coverage | `baseline-present` | — | Do not reimplement. |

## Wave 6: Search and picker modernization

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P2 | [#59604](https://github.com/zed-industries/zed/pull/59604) | `ccf4058b7a6b05207d4f1dd25106ec5fc439cc74` | v1.9 notes | Absent | `crates/picker`, `crates/file_finder`, `crates/search`, `crates/workspace` | `reimplement` | None | Tasks 6.1-6.2 layout, cancellation, and search state | `proposed` | — | Split the 88-file upstream change into vertical slices. |
| P2 | [#59838](https://github.com/zed-industries/zed/pull/59838) | `63692b8b4724357fa63d6318b45f3c3fee6f672a` | v1.12 notes | Absent | `crates/search`, `crates/editor`, `crates/project`, `crates/picker` | `reimplement` | Picker and symbol previews | Task 6.4 definition/reference/implementation results | `proposed` | — | Preserve existing actions and avoid duplicate LSP queries. |
| P2 | [#59931](https://github.com/zed-industries/zed/pull/59931) | `94b6d377badf9c2202850b551c4700a54b83895f` | v1.12 notes | Absent | `crates/picker`, finder consumers | `reimplement` | #59604 | Task 6.6a stable multi-selection | `proposed` | — | Selection must survive filtering and async refresh. |
| P2 | [#60919](https://github.com/zed-industries/zed/pull/60919) | `90b3aa0b3bd3b453775b11a386907c7ac9acd997` | v1.12 notes | Absent | `crates/picker`, finder consumers | `reimplement` | #59931 | Task 6.6b controls and accessibility | `proposed` | — | Separate behavior from discoverability UI. |

Project-symbol preview, buffer-symbol preview, and reopen-last-picker remain
discovery tasks until their final upstream PRs are resolved. Add them as new
ledger rows before implementation.

## Wave 7: Performance and settings compatibility

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P2 | [#61275](https://github.com/zed-industries/zed/pull/61275) | `ac5538b7239196b7413da76a6258bc9ac4a017fe` | v1.13.0-pre notes | Absent | `crates/editor`, `crates/language`, `crates/project` | `audit` | Relevant wave work landed | Task 7.1 per-subsystem benchmark | `proposed` | — | Decompose anchor, shaping, and scan changes. |
| P2 | [#58881](https://github.com/zed-industries/zed/pull/58881) | `253606e8e0396da2d6897c1eb996ea92aece23c4` | v1.9 notes | Absent | crash handler and process launch | `adapt` | Wave 2 process ownership | Task 7.1 startup workload and lifecycle | `proposed` | — | Keep separate from editor hot paths. |
| P2 | [#58681](https://github.com/zed-industries/zed/pull/58681) | `ab2683b04cc6f3563e3a745dd10076746030cac0` | v1.7 notes | Present | text implementation | `none` | None | Existing large-edit behavior | `baseline-present` | — | Structural sharing is already inherited. |
| P2 | [#59710](https://github.com/zed-industries/zed/pull/59710) | `76e07d5c9ac38930d051c153b21eeb57ba71cbb4` | v1.10 notes | Absent | default settings and formatter behavior | `audit` | Explicit product decision | Task 7.2 default and migration tests | `proposed` | — | Preserve current behavior until separately approved. |
| P2 | [#59860](https://github.com/zed-industries/zed/pull/59860) | `40d20036af34343a09f0ce6a2eb38c9e5a60e9ae` | v1.10 notes | Absent | `crates/settings_ui`, `crates/settings_content`, `crates/agent_threads` | `audit` | None | Task 7.3 exact Agent Threads JSON paths | `proposed` | — | Native model-provider and MCP pages are excluded; audit external-agent coverage only. |
| P1 | [#60870](https://github.com/zed-industries/zed/pull/60870) | `b9bfd5722e6520cdb54378c2d8a341edf5981e6d` | v1.10.3 notes | Absent | `crates/node_runtime` | `candidate` | None | Task 7.4 npm 12 and earlier output fixtures | `proposed` | — | Language-server installation compatibility. |
| P1 | [#60970](https://github.com/zed-industries/zed/pull/60970) | `a25f19cb2f55baa4cf8638981043ac64af741d62` | v1.12 notes | Absent | `crates/languages` | `adapt` | Review #61126 | Task 7.5 TypeScript 6 and 7 projects | `proposed` | — | Pinning may be only an intermediate fix. |
| P1 | [#61126](https://github.com/zed-industries/zed/pull/61126) | `c7ee116ead3476eaf6f34a8bbb833f628d300959` | v1.13.0-pre notes | Absent | `crates/languages`, extension recommendation | `audit` | #60970 | Task 7.5 final vtsls and tsgo behavior | `proposed` | — | Avoid false invalid-tsserver errors. |
| P2 | [#58259](https://github.com/zed-industries/zed/pull/58259) | `53667331bdaa09d25193d5156393d6169b12d84a` | v1.7 notes | Present | extension download architecture | `none` | None | Source evidence | `baseline-present` | — | x86 extension-managed binary downloads were already removed. |

## Wave 8: Final audit and recurring maintenance

Wave 8 adds newly discovered PRs before implementation and closes every seeded
entry with a terminal classification.

Completion requires:

- reconciliation of all v1.6-v1.12 stable and preview release-note PRs;
- no unclassified P0-P2 entry;
- a macOS, Linux, Windows, local, Direct, and Tunneled compatibility matrix;
- an upstream baseline recorded by commit and release tags; and
- an owner and cadence for recurring stable, preview, and later-main review.
