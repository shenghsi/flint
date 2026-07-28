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
| P0 | [#58339](https://github.com/zed-industries/zed/pull/58339) | `381f2f4977b2c104190073af01ae9762ecdd9c9e` | v1.6 notes | Present | `crates/fs` | `none` | None | `test_realfs_trash_preserves_symlink_target` | `baseline-present` | [#124](https://github.com/shenghsi/flint/pull/124) | `RealFs::trash` deliberately makes paths absolute without canonicalizing them; the regression test proves the link, rather than its target, is trashed and restored. |

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
| P1 | [#57049](https://github.com/zed-industries/zed/pull/57049) | `5a7d414a23938c5efb674d0c2948813e37448eea` | v1.11 notes | Absent | `crates/fs` | `adapt` | Wave 2 watcher stack | Task 3.5 native, poll, and root symlink-target parent selection | `landed` | [#85](https://github.com/shenghsi/flint/pull/85) | Symlink targets remain watched, while parents that require a recursively scanning poll watcher are skipped. The shared remote-server startup path does not branch on Direct or Tunneled Agent Threads routing. |
| P1 | [#59999](https://github.com/zed-industries/zed/pull/59999) | `0deb6c0deaa91d12bafae3b76d41c965bd4d7615` | v1.10 notes | Absent | `crates/proto`, `crates/project`, `crates/remote_server` | `adapt` | None | Task 3.6a remote resolution and post-LSP-start refetch | `landed` | [#87](https://github.com/shenghsi/flint/pull/87) | The shared project RPC does not branch on Direct or Tunneled Agent Threads routing. Zed collaboration forwarding remains excluded. |
| P1 | [#56487](https://github.com/zed-industries/zed/pull/56487) | `0b458e53a5b52fb205f8420db3f12315e9268915` | v1.10 notes | Absent | `crates/extension_host`, `crates/extension` | `adapt` | None | Task 3.6b language-provider dependency selection, deduplication, and missing-provider fallback | `landed` | [#89](https://github.com/shenghsi/flint/pull/89) | Language-only extensions sync only when required by a remote-loadable extension. Zed extension compatibility identifiers remain unchanged. |
| P1 | [#52537](https://github.com/zed-industries/zed/pull/52537) | `776585038e56672e2bb5ee48899c79c654aeaba2` | v1.10 notes | Absent | `crates/workspace`, `crates/terminal_view` | `adapt` | None | Task 3.6c remote absolute paths outside worktrees with Unicode row/column preservation | `landed` | [#91](https://github.com/shenghsi/flint/pull/91) | Remote terminal links use the shared project path-resolution RPC; Agent Threads routing is unchanged. |
| P1 | [#58240](https://github.com/zed-industries/zed/pull/58240) | `513e2b2ee3d59373bbafc5995119088d3ea2d368` | v1.6 notes | Present | removed native agent terminal path; `crates/agent_threads` | `none` | None | Agent Threads launch-source and route tests | `baseline-present` | [#124](https://github.com/shenghsi/flint/pull/124) | The inherited fix remains in the removed native-agent path. Flint's external Agent Threads never creates or injects a client-side sandbox temp directory; Direct preserves only the configured command environment and Tunneled adds its explicit proxy environment on the remote. |
| P1 | [#58533](https://github.com/zed-industries/zed/pull/58533) | `44fb295593d5e1c10b61a64bc0be2fc43e49f5b1` | v1.7 notes | Present | removed native `agent_ui`; `crates/agent_threads` | `none` | None | Agent Threads restore and route tests | `baseline-present` | [#124](https://github.com/shenghsi/flint/pull/124) | The inherited native-panel fix was removed with `agent_ui`. Flint restores external sessions by rebuilding a launch from the saved project root and current remote route; it does not read a leased workspace through the removed terminal-metadata helper. |

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

Wave 3 completion evidence (reviewed 2026-07-27): the integrated remote-server
suite passed 46 tests; remote recent-project identity passed 27 tests; workspace
remote transitions passed 8 tests; terminal remote links passed 4 tests;
extension dependency sync passed 3 tests; and symlink watcher coverage passed 5
tests. The Agent Threads route matrix also passed six Tunneled-policy tests and
the Direct-by-default SSH identity test. Those routing tests prove that managed
provisioning, managed credentials, managed resume, and the local traffic tunnel
remain Tunneled-only, while Direct continues to select the configured ambient
command.

## Wave 4: Agent Threads

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | [#60292](https://github.com/zed-industries/zed/pull/60292) | `4aa8ad9742b1ee948d64429a5814d9b9a861350a` | v1.11 notes | Absent | `crates/agent_threads`, `crates/terminal_view`, default keymaps | `reimplement` | None | Task 4.1 opt-in context, local/resumed launch, and terminal search | `landed` | [#94](https://github.com/shenghsi/flint/pull/94) | Agent terminals reuse `buffer_search::Deploy`; the shared post-launch registration path covers restored, Direct, and Tunneled threads without restoring `agent_ui`. Hidden kinds do not remove search from an already launched center-pane terminal, and panel width does not participate in its layout. |
| P1 | [#59374](https://github.com/zed-industries/zed/pull/59374) | `fd5d42dd55fcc185c27920e88aab21c077be5738` | v1.8 notes | Absent | `crates/agent_threads`, `crates/project`, settings crates | `reimplement` | None | Task 4.2 per-agent settings, shell ordering/failure, resume, and route matrix | `landed` | [#96](https://github.com/shenghsi/flint/pull/96) | `agent_threads.<kind>.initialization_command` runs in the configured local or remote shell after route-specific executable selection. Direct retains the ambient executable; Tunneled retains the pinned executable and tunnel. Credential commands intentionally omit initialization. |
| P1 | [#58779](https://github.com/zed-industries/zed/pull/58779) | `905e955a702707cd81a2e5bae9b381a7a9c7f614` | v1.12 notes | Absent | `crates/gpui*`, `crates/agent_threads` | `adapt` | Audit existing desktop notification | Task 4.3 inactive-window attention | `landed` | [#98](https://github.com/shenghsi/flint/pull/98) | GPUI attention is wired into Flint's existing terminal-bell subscription without restoring `agent_ui`. One bell produces one desktop notification and one request for the relevant inactive window; active windows and disabled notifications are suppressed, with new, resumed, and multi-window threads covered. Upstream supplied no tests. Process exit, failure, and cancellation remain intentionally non-signals because Flint's notification contract is the agent-emitted terminal bell. |
| P1 | [#58962](https://github.com/zed-industries/zed/pull/58962) | `620ceaaaca40b346736660f12eefce38e235cb59` | v1.8 notes | Absent | removed native agent database; inspect Agent Threads persistence | `audit` | Flint history index and restoration | Task 4.4 quit during output and restore | `excluded` | [#100](https://github.com/shenghsi/flint/pull/100) | Zed flushes native-agent content to keep its separate metadata and conversation databases consistent; Flint has neither database and external CLIs own their transcripts, so agent output cannot create that orphaned-content race. Flint PRs [#23](https://github.com/shenghsi/flint/pull/23) and [#24](https://github.com/shenghsi/flint/pull/24) already atomically snapshot known external session IDs, protect the quit snapshot from teardown, restore background workspaces once, and suppress live duplicates. Running task terminals are not workspace-serialized, preventing ghost terminals. Fresh Codex sessions remain explicitly non-restorable because Codex exposes no session-ID assignment flag. Current snapshot, deduplication, sequential restore, and scoped key-value tests pass. |
| P1 | [#59968](https://github.com/zed-industries/zed/pull/59968) | `ea87b0579464067eb45a1c1a1f2c1bdb80af7e1f` | v1.11 notes | Absent | `crates/worktree`, `crates/project`, remote-project protocol, `crates/agent_threads`, `crates/agent_history` | `adapt` | None | Task 4.5 bare checkout identity | `landed` | [#101](https://github.com/shenghsi/flint/pull/101) | Flint now tracks linked-worktree identity locally and through its remote-project protocol, grouping bare-checkout and nested linked worktrees under the shared repository while keeping ordinary sibling subdirectories distinct. Agent history and resume continue to use actual checkout roots; their complete suites pass. Direct and Tunneled routes share this project metadata transport, and the existing route matrix passes. Zed collaboration database/RPC paths remain excluded because Flint has no collaboration backend. |
| P2 | [#57747](https://github.com/zed-industries/zed/pull/57747) | `7e0f63412c60008f9dae7fcf65fc6ab6d7e0f957` | v1.11 notes | Absent | `crates/terminal`, `crates/terminal_view` | `adapt` | None | Task 4.6 shell escaping and agent recognition | `landed` | [#103](https://github.com/shenghsi/flint/pull/103) | Dropped POSIX paths use bare backslash escaping so terminal coding agents recognize file references, while PowerShell and cmd retain target-shell quoting. Tests cover spaces, both quote types, backslashes, Unicode, multiple paths, target path style, and every supported terminal drop source. The shared `TerminalView` covers Codex, Claude, and Pi across local, Direct, and Tunneled terminals. Zed's removed native `agent_ui` helper remains excluded. Upstream supplied no automated tests. |
| P2 | [#60067](https://github.com/zed-industries/zed/pull/60067) | `f5c975162cf217f2c9cd1a2c1192eb2bb4653cdc` | v1.11 notes | Absent | `crates/terminal`, `crates/terminal_view`, settings crates | `adapt` | None | Task 4.7 mouse reporting and link opening | `landed` | [#105](https://github.com/shenghsi/flint/pull/105) | `terminal.open_links_in_mouse_mode` defaults to true and is exposed at its exact Settings Editor path. Cmd/Ctrl-click opens links during mouse reporting; capture mismatches are consumed without malformed PTY sequences, while ordinary and non-link clicks and the disabled setting forward press/release. Shift remains the terminal escape hatch. Tests cover plain shells and Vim, Claude, and OpenCode mouse protocols. Shared terminal behavior covers every agent and local, Direct, and Tunneled routes. Later #60880 fixes Shift-drag selection independently and remains for Wave 8 reconciliation. |

### Wave 4 completion audit

- Settings and visibility: Codex, Claude, and Pi have defaults and exact
  Settings Editor controls for `initialization_command` and `hidden`.
  Provider-neutral terminal completion, file-drop, link-opening, notification,
  restoration, and panel settings remain visible independently of agent kind.
- Actions and history: registry, panel, terminal-search, launch, and restoration
  tests cover hidden kinds, new and resumed threads, live-thread focus, and
  history for all three agents. Fresh Codex sessions remain explicitly
  non-restorable because Codex exposes no session-ID assignment flag.
- Routes: local launches use the local command; Direct uses only the configured
  ambient remote command and exposes no Flint-managed launch or credential
  controls; Tunneled uses only the pinned Flint-managed command and local
  traffic tunnel. New, resumed, and initialization-command tests cover those
  route boundaries.
- Provider capabilities: credential and plan-usage UI is gated on explicit
  capabilities. Codex and Claude declare both; Pi intentionally declares
  neither. Remote credential UI appears only for Tunneled sessions and offers
  sign-out without remote sign-in or provider-management actions.
- Verification: all 201 `agent_threads` tests passed after Task 4.7, alongside
  the affected terminal, terminal-view, settings, and Settings Editor suites.

## Wave 5: Git workflow

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | [#59884](https://github.com/zed-industries/zed/pull/59884) | `65e1c5af258d4c80036467d583691f3f9ded0897` | v1.12 notes | Absent | `crates/git_ui`, settings crates | `adapt` | Wave 1 complete | Task 5.1 staging groups and partial state | `landed` | [#107](https://github.com/shenghsi/flint/pull/107) | `git_panel.group_by` persists no grouping, tracked/untracked, and staged/unstaged modes and is exposed at its exact Settings Editor path. Partially staged files project into both staging sections without double-counting, section and row actions follow their projection, structural rows are skipped by keyboard navigation, and empty sections remain visible. The adaptation includes later #60976 conflict locking and partial/range-action corrections while preserving Flint's legacy `sort_by_path` compatibility and unified `ProjectDiff`; separate staged/unstaged multibuffers remain Task 5.2. Later #61608 fixes discard scope independently and remains for reconciliation. Git UI passed 115 tests, Settings Editor 20, and settings content 15 plus doctests. |
| P1 | [#46541](https://github.com/zed-industries/zed/pull/46541) | `c31b2b0dc7180247b2981eb084594efaf11ee396` | v1.11 notes | Absent | `crates/git_ui`, `crates/project` | `reimplement` | #59884, Wave 1 complete | Tasks 5.2-5.3 multibuffers and hunk actions | `landed` | [#109](https://github.com/shenghsi/flint/pull/109), [#111](https://github.com/shenghsi/flint/pull/111) | Tasks 5.2-5.3 landed separate staged HEAD-to-index and unstaged index-to-worktree views plus exact-range stage, unstage, restore, and bulk controls using Flint's existing `ProjectDiff`, `GitStore`, and error-notification paths. The hunk-action adaptation also incorporates the relevant restore behavior from later upstream [#60639](https://github.com/zed-industries/zed/pull/60639), while requiring confirmation for Restore All. It skips conflicting and binary text hunks, handles repeated lines, adjacent hunks, partial staging, deletions, split views, and index failures, and excludes upstream collaboration, agent UI, protocol, optimistic-index, and broad buffer-diff refactors. Git UI passed 128 tests; the affected editor split tests passed 40 with 1 ignored; project, buffer-diff, and filesystem regressions, formatting, clippy, and a signed local bundle also passed. |
| P2 | [#59043](https://github.com/zed-industries/zed/pull/59043) | `076fd14c88336fca9d2a4093452f3820c27453dd` | v1.9 notes | Absent | `crates/git_ui`, settings crates | `adapt` | #59884 | Task 5.4 view options and settings migration | `landed` | [#113](https://github.com/shenghsi/flint/pull/113) | Replaces the boolean `git_panel.sort_by_path` with an enum `git_panel.sort_by` (`path` | `name`) independent of `group_by`, adds a View Options menu (list/tree, sort by path/name, group by none/status/staging) with `SetSortByPath`/`SetSortByName` actions (`ToggleSortByPath` now toggles path ↔ name), applies the same sort/group/tree ordering to Project Diff while preserving per-buffer fold state across view-option changes, and migrates existing `sort_by_path` values deterministically (`true → path`, `false → name`) via migrator `m_2026_07_27`. The baseline-present compare-with-branch (#57886), dedicated diff (#56152), and split history (#58163) controls were audited and not reimplemented. Git UI passed 131 tests, Settings Editor 20, settings content 15, migrator 84; formatting and clippy passed. |
| P2 | [#57886](https://github.com/zed-industries/zed/pull/57886) | `83c52e38785efbed6c0b9013cae1866f72218921` | v1.6 notes | Present | `crates/git_ui` | `none` | None | `test_branch_diff`; `test_branch_diff_action_matches_existing_item_by_base_ref` | `baseline-present` | [#124](https://github.com/shenghsi/flint/pull/124) | The branch diff still uses the merge-base comparison and reuses an already-open item for the same base ref. |
| P2 | [#56152](https://github.com/zed-industries/zed/pull/56152) | `5d3b9e467e9b789fa4422c4cf9208c497838d43b` | v1.6 notes | Present | `crates/git_ui` | `none` | None | `test_open_or_focus_for_buffer_opens_diff`; `test_open_diff` | `baseline-present` | [#124](https://github.com/shenghsi/flint/pull/124) | Git panel entries still open or focus a dedicated `SoloDiffView` for modified, staged, and untracked files. |
| P2 | [#58163](https://github.com/zed-industries/zed/pull/58163) | `52956d93e4cf219b078a1c9dd1f70ee959ba0089` | v1.6 notes | Present | `crates/git_ui`, `crates/editor` | `none` | None | Commit-view source and split-editor suite | `baseline-present` | [#124](https://github.com/shenghsi/flint/pull/124) | `CommitView` owns a `SplittableEditor`, honors split diff style during history loading, and exposes the inherited split controls; the shared split-editor suite covers split/unsplit behavior. |

## Wave 6: Search and picker modernization

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P2 | [#59604](https://github.com/zed-industries/zed/pull/59604) | `ccf4058b7a6b05207d4f1dd25106ec5fc439cc74` | v1.9 notes | Absent | `crates/picker`, `crates/file_finder`, `crates/search`, `crates/workspace` | `reimplement` | None | Tasks 6.1-6.2 layout, cancellation, and search state | `landed` | [#116](https://github.com/shenghsi/flint/pull/116), [#118](https://github.com/shenghsi/flint/pull/118), [#119](https://github.com/shenghsi/flint/pull/119) | Task 6.1 landed reusable picker previews with hidden, right, and below layouts; visible layout controls; draggable session-local sizing; per-picker layout persistence; cancellable loading; stale-completion rejection; and File Finder integration. Task 6.2 added Text Finder as a view over shared project-search state, with live buffer previews, exact match navigation, query and filter reuse, per-file collapse, query persistence, cancellation, and dense-result safeguards while preserving Project Search. Search passed all 47 tests serially and Picker passed all 5 tests; formatting, scoped clippy, Linux build, macOS bundle verification, and resource checks passed. The merged workspace run passed 4,661 of 4,665 tests: three failures were the existing `editor::hover_links` baseline and the Text Finder action namespace allowlist failure was corrected in the ledger follow-up. |
| P2 | [#59863](https://github.com/zed-industries/zed/pull/59863) | `9448417157a9e690d87213c89ea9913803373b4f` | resolved discovery | Absent | `crates/picker`, `crates/project_symbols` | `reimplement` | #59604 | Task 6.3 project-symbol preview and stale-load rejection | `landed` | [#120](https://github.com/shenghsi/flint/pull/120) | Project Symbols uses Flint's existing picker preview contract, navigates exact symbol ranges, and rejects stale asynchronous preview loads. |
| P2 | [#61069](https://github.com/zed-industries/zed/pull/61069) | `1d2a4b3f7f194184dccadfa7a091d16a06482752` | resolved discovery | Absent | `crates/outline`, `crates/picker` | `reimplement` | #59604 | Task 6.3 local, remote, and unsaved buffer-symbol previews | `landed` | [#120](https://github.com/shenghsi/flint/pull/120) | Buffer Symbols previews exact ranges only when both endpoints map to the same underlying buffer, preserving local, remote, and unsaved-buffer behavior. |
| P2 | [#59838](https://github.com/zed-industries/zed/pull/59838) | `63692b8b4724357fa63d6318b45f3c3fee6f672a` | v1.12 notes | Absent | `crates/search`, `crates/editor`, `crates/project`, `crates/picker` | `reimplement` | Picker and symbol previews | Task 6.4 definition/reference/implementation results | `landed` | [#120](https://github.com/shenghsi/flint/pull/120) | Definitions, references, implementations, and diagnostics share a single-query LSP result picker. `editor.lsp_results_location` and per-action `open_results_in` select picker or multibuffer presentation without duplicate requests; upstream collaboration-specific code was excluded. |
| P2 | [#59912](https://github.com/zed-industries/zed/pull/59912) | `8186af99a347dfa9f9fd5af88da419b97b9727fa` | resolved discovery | Absent | `crates/workspace`, `crates/picker`, picker consumers | `reimplement` | Tasks 6.1-6.4 | Task 6.5 reconstructible picker requests | `landed` | [#120](https://github.com/shenghsi/flint/pull/120) | Flint deliberately excludes upstream's retained live modal. File Finder, Text Finder, Project Symbols, Buffer Symbols, and LSP results instead reconstruct new picker entities from typed requests, current project state, queries, modes, and surviving stable selections. |
| P2 | [#61002](https://github.com/zed-industries/zed/pull/61002) | `4ebc1545d299b1270bc76813fa841357ee711b19` | later fix to #59912 | Absent | `crates/workspace` | `adapt` | #59912 | Task 6.5 Command Palette and Which Key dismissal ordering | `landed` | [#120](https://github.com/shenghsi/flint/pull/120) | Reopen requests queue while a transient Command Palette or Which Key modal is active and execute only after `ModalClosedEvent` confirms its removal. |
| P2 | [#59931](https://github.com/zed-industries/zed/pull/59931) | `94b6d377badf9c2202850b551c4700a54b83895f` | v1.12 notes | Absent | `crates/picker`, finder consumers | `reimplement` | #59604 | Task 6.6a stable multi-selection | `landed` | [#120](https://github.com/shenghsi/flint/pull/120) | Stable project-path identities preserve File Finder and file-level Text Finder selections across filtering, reordering, and asynchronous refresh; vanished entries are discarded and multi-confirm follows deterministic result order. |
| P2 | [#60919](https://github.com/zed-industries/zed/pull/60919) | `90b3aa0b3bd3b453775b11a386907c7ac9acd997` | v1.12 notes | Absent | `crates/picker`, finder consumers | `reimplement` | #59931 | Task 6.6b controls and accessibility | `landed` | [#120](https://github.com/shenghsi/flint/pull/120) | Shared checkboxes, a visible multi-select mode control, secondary-click and keyboard toggles, selected counts, and accessible labels expose the stable selection model without importing unrelated upstream UI. |

Wave 6 completion evidence: integration PR
[#120](https://github.com/shenghsi/flint/pull/120) passed formatting, clippy,
Linux build, Linux and Windows resource checks, and the full 4,682-test
workspace run after stabilizing three baseline `editor::hover_links` tests by
draining fake-filesystem discovery. The affected picker, File Finder, search,
project-symbol, outline, LSP-location, editor, project, workspace, settings
content, and Settings Editor suites passed locally. The debug app built and
signed successfully; the documented debug-only `release/remote_server` gzip
step failed after bundle creation, so the fresh bundle was copied with
`ditto`, and its executable and signature were verified. The app log confirmed
the integrated binary initialized the workspace and rendered its first frame;
interactive UI automation was unavailable because the Mac was accessed over
SSH and exposed no attachable CGWindow.

## Wave 7: Performance and settings compatibility

| Priority | Upstream PR | Final commit | Provenance | Fork ancestry | Flint paths | Strategy | Prerequisites | Required tests | Status | Flint PR | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P2 | [#61275 anchor resolution](https://github.com/zed-industries/zed/pull/61275) | `ac5538b7239196b7413da76a6258bc9ac4a017fe` | v1.13.0-pre notes | Absent | `crates/text` | `adapt` | #58681 baseline retained | `anchor_to_offset_fragmented_buffer` | `implementing` | [#125](https://github.com/shenghsi/flint/pull/125) | Direct offset lookup improved the fragmented-buffer benchmark by 2.42% and 2.72% in two statistically significant A/B runs. |
| P2 | [#61275 line shaping](https://github.com/zed-industries/zed/pull/61275) | `ac5538b7239196b7413da76a6258bc9ac4a017fe` | v1.13.0-pre notes | Absent | `crates/editor` | `audit` | None | Visible-line shaping allocation benchmark | `proposed` | — | Measure the avoided `String` clone independently. |
| P2 | [#61275 operation sorting](https://github.com/zed-industries/zed/pull/61275) | `ac5538b7239196b7413da76a6258bc9ac4a017fe` | v1.13.0-pre notes | Absent | `crates/text` | `audit` | None | Operation-queue insertion benchmark | `proposed` | — | Measure stable versus unstable sorting with unique Lamport timestamps. |
| P2 | [#61275 multibuffer sorting](https://github.com/zed-industries/zed/pull/61275) | `ac5538b7239196b7413da76a6258bc9ac4a017fe` | v1.13.0-pre notes | Absent | `crates/multi_buffer` | `audit` | None | Edited-path sorting benchmark | `proposed` | — | Measure avoiding a `PathKey` clone on each comparison. |
| P2 | [#61275 worktree scanning](https://github.com/zed-industries/zed/pull/61275) | `ac5538b7239196b7413da76a6258bc9ac4a017fe` | v1.13.0-pre notes | Absent | `crates/worktree` | `audit` | None | Deferred-directory scan benchmark | `proposed` | — | Measure replacing repeated vector removal with tombstone assignment. |
| P2 | [#58881](https://github.com/zed-industries/zed/pull/58881) | `253606e8e0396da2d6897c1eb996ea92aece23c4` | v1.9 notes | Absent | crash handler and process launch | `adapt` | Wave 2 process ownership | Task 7.1 startup workload and lifecycle | `proposed` | — | Keep separate from editor hot paths. |
| P2 | [#58681](https://github.com/zed-industries/zed/pull/58681) | `ab2683b04cc6f3563e3a745dd10076746030cac0` | v1.7 notes | Present | `crates/text` | `none` | None | `FragmentBuilder` source; large normalized-text and edit-splitting tests | `baseline-present` | [#124](https://github.com/shenghsi/flint/pull/124) | Text rebuilds append sliced old fragment subtrees as `Tree` chunks rather than copying their leaves, preserving the inherited structural sharing across large edits. |
| P2 | [#59710](https://github.com/zed-industries/zed/pull/59710) | `76e07d5c9ac38930d051c153b21eeb57ba71cbb4` | v1.10 notes | Absent | default settings and formatter behavior | `audit` | Explicit product decision | Task 7.2 default and migration tests | `proposed` | — | Preserve current behavior until separately approved. |
| P2 | [#59860](https://github.com/zed-industries/zed/pull/59860) | `40d20036af34343a09f0ce6a2eb38c9e5a60e9ae` | v1.10 notes | Absent | `crates/settings_ui`, `crates/settings_content`, `crates/agent_threads` | `audit` | None | Task 7.3 exact Agent Threads JSON paths and capability tests | `landed` | [#123](https://github.com/shenghsi/flint/pull/123) | Merged as `8ebaccc3bf7d9b0ba6208a0af68a546d1c69a8a0`; Zed model-provider and MCP settings remain excluded by the product boundary, and every applicable Flint external-agent setting has exact-path coverage. |
| P1 | [#60870](https://github.com/zed-industries/zed/pull/60870) | `b9bfd5722e6520cdb54378c2d8a341edf5981e6d` | v1.10.3 notes | Absent | `crates/node_runtime` | `adapt` | None | `test_npm_info_accepts_npm_12_array_response` plus existing object-response tests | `landed` | [#122](https://github.com/shenghsi/flint/pull/122) | Merged as `627ca8aa694cfab018252c0be90406e7d3359d00`; accepts npm 12's one-element array response while preserving earlier object responses and includes malformed stdout in parser errors. |
| P1 | [#60970](https://github.com/zed-industries/zed/pull/60970) | `a25f19cb2f55baa4cf8638981043ac64af741d62` | v1.12 notes | Absent | `crates/languages` | `adapt` | Review #61126 | Task 7.5 TypeScript 6 and 7 projects | `proposed` | — | Pinning may be only an intermediate fix. |
| P1 | [#61126](https://github.com/zed-industries/zed/pull/61126) | `c7ee116ead3476eaf6f34a8bbb833f628d300959` | v1.13.0-pre notes | Absent | `crates/languages`, extension recommendation | `audit` | #60970 | Task 7.5 final vtsls and tsgo behavior | `proposed` | — | Avoid false invalid-tsserver errors. |
| P2 | [#58259](https://github.com/zed-industries/zed/pull/58259) | `53667331bdaa09d25193d5156393d6169b12d84a` | v1.7 notes | Present | `crates/extension_api`, `crates/extension_host` | `none` | None | v0.8 WIT and current host mapping source | `baseline-present` | [#124](https://github.com/shenghsi/flint/pull/124) | The current extension API exposes only AArch64 and x86-64 and rejects unsupported host architectures. Older versioned WIT definitions retain x86 solely for binary compatibility with extensions built against those APIs. |

Task 7.6 baseline evidence (reviewed 2026-07-28): the eight commits above
remain ancestors of Flint's fork point and were not reimplemented. The
filesystem regression exercises the real OS trash path because #58339 had no
direct coverage. The #58240 and #58533 implementation sites belonged to the
native `agent` and `agent_ui` crates that Flint removed; current Agent Threads
launches external CLIs in terminal tasks and restores them from project-root
session records, with Direct/Tunneled route selection kept explicit. Git's
branch comparison and dedicated file-diff paths have direct tests, while
commit history continues to use the tested shared split editor. Text's
`FragmentBuilder` retains sliced SumTree subtrees, and the current v0.8
extension platform interface has no 32-bit x86 variant.

Task 7.3 audit evidence (reviewed 2026-07-28): Agent Threads registers
Codex, Claude, and Pi. Each has Settings Editor controls for its initialization
command and visibility, while panel-wide controls cover the thread limit, plan
usage, completion notifications, session reopening, and dock position. The
exact-path acceptance table covers all eleven fields. Credential commands and
plan usage are gated by `credential_policy` and `supports_plan_usage`; Pi has
neither capability, and its omission is tested. Remote credential UI is
available only for Tunneled routes, and credential launch tests prove that
Direct routes retain the ambient executable while Tunneled routes use the
pinned Flint-managed executable. The SSH route selector and persistence live
with remote connection settings rather than per-agent settings. Zed's native
language-model provider and MCP configuration pages have no Flint Agent
Threads equivalent and remain excluded by the product boundary.

## Wave 8: Final audit and recurring maintenance

Wave 8 adds newly discovered PRs before implementation and closes every seeded
entry with a terminal classification.

Completion requires:

- reconciliation of all v1.6-v1.12 stable and preview release-note PRs;
- no unclassified P0-P2 entry;
- a macOS, Linux, Windows, local, Direct, and Tunneled compatibility matrix;
- an upstream baseline recorded by commit and release tags; and
- an owner and cadence for recurring stable, preview, and later-main review.
