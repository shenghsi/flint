# Upstream Domain-Wave Integration Implementation Plan

**Date:** 2026-07-27
**Status:** Approved
**Design:** `docs/superpowers/specs/2026-07-27-upstream-domain-wave-design.md`
**Upstream fork point:** `6e9465a4288c332208643892e23b9d35d7be5c79`
**Initial target:** Zed stable v1.12.0 plus explicitly selected v1.13.0-pre safety fixes

## Objective

Integrate the approved P0-P2 Zed backlog as ordered, independently reviewable
domain waves without merging or rebasing Flint onto Zed.

This is a program plan, not one change. Each numbered implementation task below
normally produces its own feature branch and pull request. A task may be closed
without code only when its investigation proves the behavior already exists,
is inapplicable to Flint, or has been superseded; the ledger must record that
result and its evidence.

## Program Rules

1. Start every task from current `origin/main`, not from the documentation
   branch or the previous task branch.
2. Prove that the upstream commit is absent from the fork-point ancestry.
3. Inspect the complete upstream PR and later corrections before writing code.
4. Reproduce the missing behavior in current Flint.
5. Write the failing regression or acceptance test.
6. Port the behavior using Flint's current architecture.
7. Run focused tests, `cargo fmt --all -- --check`, and `./script/clippy`.
8. Build `/tmp/Flint-Local.app` for user-visible or macOS-sensitive changes.
9. Open a focused pull request with an imperative title and final
   `Release Notes:` section.
10. Update the upstream ledger in the same PR with the verified result.
11. Do not begin a dependent task until its prerequisite PR is merged.

Do not use the presence of a change in release notes as evidence that it is
missing. The initial ancestry audit already proved that Zed PRs #58339, #58240,
#58533, #57886, #56152, #58163, #58681, and #58259 are present at Flint's fork
point.

## Standard Investigation Checklist

Run this checklist at the start of every task:

```sh
git fetch origin
git switch main
git pull --ff-only
git status --short
```

Inspect the upstream change in a temporary Zed clone:

```sh
git show --stat <upstream-commit>
git show <upstream-commit> -- <relevant-paths>
git log --oneline <upstream-commit>..origin/main -- <relevant-paths>
```

Compare affected paths with Flint:

```sh
git diff --name-only \
  6e9465a4288c332208643892e23b9d35d7be5c79..main \
  -- <relevant-paths>
```

The upstream clone and refs must remain outside the Flint worktree. Do not add
an upstream remote to a task branch as part of implementation.

## Wave 0: Establish the Ledger

### Task 0.1: Create the PR-level upstream ledger

**Branch:** `docs/zed-upstream-ledger`

**Files:**

- Create: `docs/superpowers/zed-upstream-ledger.md`
- Reference:
  `docs/superpowers/specs/2026-07-27-upstream-domain-wave-design.md`
- Reference:
  `docs/superpowers/specs/2026-07-27-upstream-domain-wave-implementation-plan.md`

**Steps:**

1. Create a ledger table with these fields:
   wave, priority, upstream PR, final commit, release provenance, ancestry
   result, affected Flint paths, integration classification, prerequisites,
   tests, status, Flint PR, and notes.
2. Seed every PR named in this plan.
3. Mark the eight ancestry-confirmed changes `baseline-present`.
4. Mark Zed cloud, collaboration, accounts, billing, telemetry, hosted edit
   prediction, and merchandising categories `excluded-by-product-boundary`.
5. Add definitions for `proposed`, `investigating`, `implementing`, `landed`,
   `deferred`, `superseded`, `baseline-present`, and `excluded`.
6. Add a rule that a status can change to `landed` only after CI and review.
7. Run:

   ```sh
   git diff --check
   ```

8. Open a documentation PR titled `Document Zed upstream integration ledger`.

**Wave gate:** No implementation task starts until the ledger PR is merged.

## Wave 1: Safety and Data Integrity

### Task 1.1: Prevent repeated-line hunk staging corruption

**Upstream:** Zed PR #60584, commit
`eb962794a33cbd414c6a343a8bb1136c94fe6901`

**Classification:** Adaptation.

**Branch:** `fix/git-repeated-hunk-staging`

**Starting files:**

- `crates/buffer_diff/src/buffer_diff.rs`
- `crates/git_ui/src/project_diff.rs`
- Existing Git and buffer-diff tests near the changed implementation

**Steps:**

1. Inspect the upstream canonicalization algorithm and every later change to
   ambiguous hunk placement.
2. Add a regression fixture with two or more identical changed regions where
   the selected hunk is not the first valid match.
3. Assert that staging changes only the selected hunk.
4. Assert that unstaging restores only that hunk.
5. Assert that the resulting index and working-tree diff remain parseable.
6. Port the canonicalization behavior without replacing Flint's surrounding
   diff abstractions.
7. Run:

   ```sh
   cargo test -p buffer_diff
   cargo test -p git_ui
   cargo fmt --all -- --check
   ./script/clippy
   ```

8. Open a PR titled `git: Prevent repeated-hunk staging corruption`.

### Task 1.2: Preserve Git hooks and long-running operations

**Upstream:** Zed PR #61185, commit
`2a983bca8616c6d8ad111667a5ed6064cc3cbb61`

**Classification:** Adaptation.

**Depends on:** Task 1.1.

**Branch:** `fix/git-hooks-and-operation-timeouts`

**Starting files:**

- `crates/git/src/repository.rs`
- `crates/git/src/remote.rs`
- `crates/git_ui/src/git_panel.rs`
- Git integration-test support in `crates/git`

**Steps:**

1. Identify where Flint adds `--no-verify`, configures askpass, and applies
   transport timeouts.
2. Add a repository test with a rejecting `commit-msg` hook.
3. Assert that a normal commit runs the hook, does not create a commit, and
   returns the hook's stderr.
4. Preserve an explicit no-verify path only if Flint already exposes one; do
   not add new UI in this task.
5. Add a controlled slow Git operation test that exceeds the old timeout while
   remaining responsive to task cancellation.
6. Separate connection establishment timeout from operation duration.
7. Propagate the real Git or hook failure to the Git panel.
8. Run:

   ```sh
   cargo test -p git
   cargo test -p git_ui
   cargo fmt --all -- --check
   ./script/clippy
   ```

9. Open a PR titled `git: Run commit hooks and preserve slow operations`.

### Task 1.3: Protect manually created worktrees during thread archival

**Upstream:** Zed PR #58275, commit
`29622911de00305340012f798f17c04a274f4b31`

**Classification:** Investigate, then adapt or exclude.

**Branch:** `fix/agent-thread-worktree-ownership`

**Starting files:**

- `crates/agent_threads/src/`
- `crates/git_ui/src/worktree_service.rs`
- `crates/git_ui/src/worktree_picker.rs`
- `crates/git/src/repository.rs`

**Steps:**

1. Determine whether any reachable Flint thread archival path deletes
   worktrees.
2. Identify the durable ownership signal that distinguishes a
   Flint-created disposable worktree from a manual worktree. Do not infer
   ownership from naming alone.
3. If no reachable path deletes worktrees, add the evidence to the ledger and
   close this task as excluded.
4. Otherwise, add tests for:
   - archiving a thread with a Flint-owned disposable worktree;
   - archiving a thread associated with a manual worktree; and
   - a restored thread whose ownership metadata is unavailable.
5. Permit deletion only for explicitly owned disposable worktrees.
6. Surface deletion failures to the UI and leave the worktree registered.
7. Run:

   ```sh
   cargo test -p agent_threads
   cargo test -p git_ui worktree
   cargo fmt --all -- --check
   ./script/clippy
   ```

8. For a code change, open a PR titled
   `agent_threads: Preserve manually created worktrees`.

### Wave 1 completion

1. Run all tests added by Tasks 1.1-1.3.
2. Confirm the ledger contains no unclassified Wave 1 entry.
3. Record Wave 1 as complete before beginning Wave 5.

## Wave 2: Core Stability and Resource Management

### Task 2.1: Update the filesystem watcher correctness stack

**Upstream:**

- #58994 `e569b955f55b1a2cee026a084eb16578c6e2e30a`
- #59045 `bae8065e2fbc0331cdd2240ea249a25ab162c77b`
- #59714 `37679b98a558a0c8a46b46761b677494fdbfb011`
- #59560 `3723eef7f673300cc6818ae7a327fc6a30952068`

**Classification:** Dependency audit followed by adaptation.

**Branch:** `fix/filesystem-watcher-reliability`

**Starting files:**

- `crates/fs/src/fs_watcher.rs`
- `crates/fs/src/fs.rs`
- Workspace dependencies in `Cargo.toml` and `Cargo.lock`
- Filesystem watcher integration tests

**Steps:**

1. Compare Flint's current `notify` revision with all four upstream PRs.
2. Inspect later upstream watcher fixes before selecting a dependency revision.
3. Add GPUI-executor-controlled tests for:
   - watch registration and removal completing without hanging;
   - case-only rename events;
   - events whose reported casing differs from the registered path; and
   - burst events not monopolizing the reader thread.
4. Update the dependency and adapt path normalization and dispatch behavior as
   one tested stack.
5. Do not add polling as a fallback for a native watcher failure.
6. Run:

   ```sh
   cargo test -p fs
   cargo test -p project worktree
   cargo fmt --all -- --check
   ./script/clippy
   ```

7. Open a PR titled `fs: Improve filesystem watcher reliability`.

### Task 2.2: Bound incoming language-server messages

**Upstream:** Zed PR #58867, commit
`7f6f93c089e5ed50342e2c4288a71545ddaf4f5d`

**Classification:** Cherry-pick candidate after dependency review.

**Branch:** `fix/bound-lsp-message-queue`

**Starting files:**

- `crates/lsp/src/lsp.rs`
- `crates/project/src/lsp_store.rs`
- Existing language-server test support

**Steps:**

1. Add a synthetic language server that floods notifications while the
   foreground executor is paused.
2. Assert the pending queue remains bounded and the server task applies
   backpressure rather than dropping protocol messages silently.
3. Port the bounded-channel behavior.
4. Assert shutdown completes when the sender is blocked.
5. Run:

   ```sh
   cargo test -p lsp
   cargo test -p project lsp
   cargo fmt --all -- --check
   ./script/clippy
   ```

6. Open a PR titled `lsp: Bound incoming message queues`.

### Task 2.3: Classify the removed native-agent diagnostics fix

**Upstream:** Zed PR #61176 and its release-branch cherry-picks.

**Classification:** Expected product-boundary exclusion.

**Depends on:** Task 2.2.

**Branch:** No code branch unless investigation finds a reachable consumer.

**Starting files:**

- `crates/project/src/lsp_store.rs`
- `crates/project/src/project.rs`
- `crates/agent_threads/src/`

**Steps:**

1. Confirm that Flint's removed native `agent` and `agent_ui` crates owned the
   upstream diagnostics call.
2. Search Agent Threads for any path that waits for workspace diagnostics.
3. If none exists, mark #61176 `excluded-by-product-boundary` with source
   evidence.
4. If a reachable Flint consumer exists, create a separate design before
   adapting the timeout; do not restore the removed native-agent crates.

### Task 2.4: Fix Linux PTY descriptor leaks

**Upstream:** Zed PR #58683, commit
`7854e4535ddb8fee8f0d72b46bbf98c28a1f4463`

**Classification:** Adaptation.

**Branch:** `fix/linux-terminal-descriptor-leak`

**Starting files:**

- `crates/terminal/src/pty_info.rs`
- `crates/terminal/src/terminal.rs`
- Linux terminal tests

**Steps:**

1. Add a Linux-only repeated PTY process-tracking test.
2. Count open descriptors before and after enough iterations to expose growth.
3. Port the upstream ownership fix.
4. Assert clipboard and new-process operations still work after the stress
   loop.
5. Run Linux:

   ```sh
   cargo test -p terminal
   cargo fmt --all -- --check
   ./script/clippy
   ```

6. Open a PR titled `terminal: Fix Linux PTY descriptor leak`.

### Task 2.5: Fix macOS process-spawn and reaper leaks

**Upstream:**

- #59128 `e1bfcf85db56f75a2f6d67143aad2da22c3d2240`
- #59156 `d4cc8d240965e1b3c86b1132df2278e4d01333f6`
- inspect #59358 before choosing the final async-process integration

**Classification:** Adaptation.

**Branch:** `fix/macos-process-reaping`

**Starting files:**

- `crates/gpui/src/platform.rs`
- `crates/gpui_macos/src/`
- `crates/util/src/command.rs`
- Workspace async-process patch configuration

**Steps:**

1. Inspect whether Flint still uses the code path fixed by the temporary
   async-process patch or already contains its replacement.
2. Add macOS-only tests for failed spawn cleanup and repeated child exit.
3. Assert descriptor count and unreaped-child count return to baseline.
4. Port only the final upstream ownership model; do not introduce a temporary
   patch that a later upstream PR removed.
5. Run:

   ```sh
   cargo test -p gpui
   cargo test -p util
   cargo fmt --all -- --check
   ./script/clippy
   ./script/bundle-tmp-app
   ```

6. Verify `/tmp/Flint-Local.app` is fresh even if the bundling script exits
   during its known debug remote-server step.
7. Open a PR titled `gpui: Fix macOS child-process cleanup`.

### Task 2.6: Reap Windows process trees

**Upstream:** Zed PR #58885, commit
`c642b422deaf6119aad2943ea22ec3074f39ef3c`

**Classification:** Adaptation.

**Branch:** `fix/windows-process-tree-cleanup`

**Starting files:**

- `crates/util/src/command.rs`
- `crates/gpui_windows/src/`
- `crates/terminal/src/`
- Agent-server and debugger process launchers

**Steps:**

1. Inventory every helper process launched for terminals, external agents,
   MCP servers, language servers, and debug adapters.
2. Add a Windows test helper that spawns a child and grandchild process.
3. Assert dropping the owning task terminates the entire process tree.
4. Port the Windows Job Object ownership behavior behind the platform
   abstraction.
5. Ensure intentionally detached processes retain explicit ownership.
6. Run on Windows:

   ```powershell
   cargo test -p util
   cargo test -p terminal
   cargo test -p agent_servers
   cargo fmt --all -- --check
   ./script/clippy
   ```

7. Open a PR titled `util: Reap spawned process trees on Windows`.

### Wave 2 completion

Run the Wave 2 stress tests on macOS, Linux, and Windows. Record platform
deferrals separately; do not report the whole wave complete while an applicable
platform remains unclassified.

## Wave 3: Remote Development and Terminal Reliability

### Task 3.1: Preserve changed paths in remote worktree events

**Upstream:** Zed PR #58157, commit
`36a3a2a784fb51f7e58bc6f90e9e0202587ad4bb`

**Classification:** Adaptation.

**Branch:** `fix/remote-updated-entry-paths`

**Starting files:**

- `crates/proto/proto/remote_project.proto`
- `crates/project/src/worktree_store.rs`
- `crates/remote_server/src/headless_project.rs`
- `crates/remote_server/src/remote_editing_tests.rs`

**Steps:**

1. Confirm whether the current protocol drops paths or only fails to apply
   them.
2. Add protocol round-trip and remote editing tests for create, modify, rename,
   and delete events.
3. Add a capability or protocol-version fallback if older remote servers
   cannot provide paths.
4. Preserve changed paths end to end.
5. Run:

   ```sh
   cargo test -p proto
   cargo test -p project remote
   cargo test -p remote_server remote_editing
   cargo fmt --all -- --check
   ./script/clippy
   ```

6. Open a PR titled `remote: Preserve changed paths in worktree events`.

### Task 3.2: Keep local and remote project identities distinct

**Upstream:**

- #59272 `338530f2e9b36ea05cc27db96e8f2270bd32cd12`
- #53953 `2838ea3f59458fc550d844e78fb4fec8eaf39fa3`

**Classification:** Adaptation.

**Depends on:** Task 3.1.

**Branch:** `fix/local-remote-project-identity`

**Starting files:**

- `crates/recent_projects/src/recent_projects.rs`
- `crates/recent_projects/src/sidebar_recent_projects.rs`
- `crates/recent_projects/src/remote_connections.rs`
- `crates/workspace/src/`

**Steps:**

1. Define project identity as route plus host identity plus normalized remote
   path, rather than display path alone.
2. Add tests where local and remote projects share the same visible checkout
   path.
3. Add tests switching between local and remote project groups in one window.
4. Assert no ghost project remains and both recent entries remain available.
5. Preserve existing recent-project persistence through a deterministic
   migration if its key changes.
6. Run:

   ```sh
   cargo test -p recent_projects
   cargo test -p workspace recent
   cargo fmt --all -- --check
   ./script/clippy
   ```

7. Open a PR titled `recent_projects: Distinguish local and remote checkouts`.

### Task 3.3: Validate remote trust paths with the host path style

**Upstream:** Zed PR #60139, commit
`7b128f9263396555041d3c416ba75cf7554fe1a4`

**Classification:** Adaptation.

**Branch:** `fix/remote-trust-path-style`

**Starting files:**

- `crates/project/src/trusted_worktrees.rs`
- `crates/project/tests/integration/trusted_worktrees.rs`
- `crates/recent_projects/src/remote_connections.rs`
- Remote path-style types in `crates/util`

**Steps:**

1. Add client-Windows/remote-POSIX and client-POSIX/remote-Windows tests.
2. Assert valid parent trust scopes are accepted according to the remote host.
3. Assert mixed-style traversal and malformed roots are rejected.
4. Use the remote path-style value already negotiated by the project
   connection.
5. Run:

   ```sh
   cargo test -p project trusted_worktrees
   cargo test -p recent_projects remote
   cargo fmt --all -- --check
   ./script/clippy
   ```

6. Open a PR titled `project: Validate trust paths using the remote host`.

### Task 3.4: Create remote worktrees from remote default branches

**Upstream:** Zed PR #59134, commit
`503292376ed04fca814c8b4533b38f90863675fb`

**Classification:** Adaptation.

**Branch:** `fix/remote-default-branch-worktrees`

**Starting files:**

- `crates/git_ui/src/worktree_picker.rs`
- `crates/git_ui/src/worktree_service.rs`
- `crates/git/src/repository.rs`
- Remote Git test support

**Steps:**

1. Add a remote repository fixture whose default branch exists only as
   `origin/main`.
2. Assert the worktree picker offers creation from that branch.
3. Assert the created local branch tracks the intended remote branch.
4. Test both Direct and Tunneled projects; the route must not change worktree
   semantics.
5. Run:

   ```sh
   cargo test -p git_ui worktree
   cargo test -p git worktree
   cargo fmt --all -- --check
   ./script/clippy
   ```

6. Open a PR titled `git_ui: Create remote worktrees from default branches`.

### Task 3.5: Avoid SSH watcher failures for symlinked Git configuration

**Upstream:** Zed PR #57049, commit
`5a7d414a23938c5efb674d0c2948813e37448eea`

**Classification:** Adaptation.

**Branch:** `fix/ssh-symlinked-gitconfig-watch`

**Starting files:**

- `crates/fs/src/fs_watcher.rs`
- `crates/remote/src/transport/ssh.rs`
- SSH connection tests

**Steps:**

1. Add a test with `.gitconfig` symlinked to a poll-watched or virtual
   filesystem target.
2. Assert connection setup does not wait on an invalid parent watch.
3. Preserve updates to the symlink target through the supported polling path.
4. Run:

   ```sh
   cargo test -p fs
   cargo test -p remote ssh
   cargo fmt --all -- --check
   ./script/clippy
   ```

5. Open a PR titled `remote: Handle symlinked Git configuration over SSH`.

### Task 3.6: Restore remote editor and terminal integrations

**Upstream:**

- #59999 `0deb6c0deaa91d12bafae3b76d41c965bd4d7615`
- #56487 `0b458e53a5b52fb205f8420db3f12315e9268915`
- #52537 `776585038e56672e2bb5ee48899c79c654aeaba2`

**Classification:** Three separate adaptations and PRs.

#### Task 3.6a: Resolve code lens remotely

**Files:**

- `crates/project/src/lsp_store/code_lens.rs`
- `crates/project/tests/integration/lsp_store.rs`

Add an integration test that registers, resolves, and invokes code lens through
a remote project. Open `project: Resolve code lens in remote projects`.

#### Task 3.6b: Synchronize language-only extensions

**Files:**

- `crates/extension_host/src/`
- `crates/extension/src/`
- Remote extension synchronization tests

Test an extension that contributes a language without a language server. Keep
the `zed_extension_api` and `zed:extension/*` interfaces unchanged. Open
`extension_host: Synchronize language-only remote extensions`.

#### Task 3.6c: Open remote terminal paths outside worktrees

**Files:**

- `crates/terminal_view/src/terminal_path_like_target.rs`
- `crates/terminal_view/src/terminal_view.rs`

Test remote absolute paths inside and outside loaded worktrees, including
non-ASCII rows and columns. Open
`terminal_view: Open remote paths outside worktrees`.

For each subtask run its crate tests, formatting, and clippy. Test Direct and
Tunneled routes explicitly.

### Wave 3 completion

Run the remote route matrix for all Wave 3 behaviors. Confirm that Direct never
uses Flint-managed agent provisioning and Tunneled never uses the ambient
remote executable.

## Wave 4: Agent Threads

Before each task, determine whether the upstream behavior belongs to Zed's
native agent panel, terminal infrastructure, or external terminal agents. Port
only behavior that has a reachable Flint consumer.

### Task 4.1: Search terminal agent threads

**Upstream:** Zed PR #60292, commit
`4aa8ad9742b1ee948d64429a5814d9b9a861350a`

**Classification:** Reimplementation against Flint Agent Threads.

**Branch:** `feature/agent-thread-search`

**Starting files:**

- `crates/agent_threads/src/panel.rs`
- `crates/agent_threads/src/store.rs`
- `crates/terminal_view/src/terminal_view.rs`
- Existing terminal search actions and tests

**Steps:**

1. Reuse terminal search behavior rather than adding a second search engine.
2. Add tests for opening search from a live local thread, restored thread, and
   remote thread.
3. Preserve the active result when terminal output arrives.
4. Add actions and keybindings without overriding existing terminal search.
5. Test hidden agent kinds and narrow panel layouts.
6. Run:

   ```sh
   cargo test -p agent_threads search
   cargo test -p terminal_view search
   cargo fmt --all -- --check
   ./script/clippy
   ./script/bundle-tmp-app
   ```

7. Open a PR titled `agent_threads: Search terminal threads`.

### Task 4.2: Add terminal-thread initialization commands

**Upstream:** Zed PR #59374, commit
`fd5d42dd55fcc185c27920e88aab21c077be5738`

**Classification:** Reimplementation.

**Branch:** `feature/agent-thread-init-command`

**Starting files:**

- `crates/settings_content/src/agent_threads.rs`
- `crates/settings_ui/src/`
- `crates/agent_threads/src/`
- `assets/settings/default.json`

**Steps:**

1. Decide whether initialization is global or per agent. Prefer per-agent
   capability because Direct agents use ambient remote executables.
2. Define the exact setting path and default.
3. Add schema and Settings Editor tests for that path.
4. Add a failing test proving the command runs after PTY startup and before the
   agent command.
5. Test shell failure, cancellation, local execution, Direct remote execution,
   and Tunneled execution.
6. Propagate initialization failure to the thread UI and do not launch the
   agent afterward.
7. Run:

   ```sh
   cargo test -p settings_content agent_threads
   cargo test -p settings_ui agent_threads
   cargo test -p agent_threads init
   cargo fmt --all -- --check
   ./script/clippy
   ./script/bundle-tmp-app
   ```

8. Open a PR titled `agent_threads: Add initialization commands`.

### Task 4.3: Request attention when agent work completes

**Upstream:** Zed PR #58779, commit
`905e955a702707cd81a2e5bae9b381a7a9c7f614`

**Classification:** Adapt GPUI capability; integrate with Flint notifications.

**Branch:** `feature/agent-window-attention`

**Starting files:**

- `crates/gpui/src/platform.rs`
- `crates/gpui_macos/src/`
- `crates/gpui_linux/src/`
- `crates/gpui_windows/src/`
- `crates/agent_threads/src/`

**Steps:**

1. Audit Flint's existing Agent Threads desktop notification behavior.
2. Add a platform abstraction and test implementation for attention requests.
3. Request attention only when the relevant Flint window is not active.
4. Avoid duplicate desktop notification and attention events for one
   completion.
5. Test completion, failure, cancellation, restored sessions, and multiple
   windows.
6. Run platform tests, `cargo test -p agent_threads`, formatting, clippy, and
   the local app bundle.
7. Open a PR titled `agent_threads: Request attention when work completes`.

### Task 4.4: Audit quit-time thread persistence

**Upstream:** Zed PR #58962, commit
`620ceaaaca40b346736660f12eefce38e235cb59`, plus review follow-ups.

**Classification:** Investigate, then adapt or exclude.

**Branch:** `fix/agent-thread-quit-persistence`

**Starting files:**

- `crates/agent_threads/src/store.rs`
- `crates/agent_threads/src/panel.rs`
- `crates/agent_history/src/`
- `crates/terminal_view/src/persistence.rs`

**Steps:**

1. Distinguish Zed native-agent database flushing from Flint external-agent
   terminal persistence.
2. Reproduce quitting while an external agent emits output.
3. Assert the next launch restores the thread without a duplicate or ghost
   terminal.
4. If current Agent Threads already satisfy the behavior, record it as
   superseded by Flint's history index and session restoration work.
5. Otherwise, persist only Flint-owned metadata; do not copy native-agent
   database behavior into external agent histories.
6. Open a code PR only if the regression fails.

### Task 4.5: Group threads for bare-checkout worktrees

**Upstream:** Zed PR #59968, commit
`ea87b0579464067eb45a1c1a1f2c1bdb80af7e1f`

**Classification:** Adaptation.

**Branch:** `fix/agent-thread-bare-worktree-grouping`

**Starting files:**

- `crates/agent_threads/src/store.rs`
- `crates/agent_threads/src/panel.rs`
- `crates/agent_history/src/`

**Steps:**

1. Add fixtures for a normal checkout, linked worktree, and bare checkout.
2. Assert live and historical threads resolve to one stable project group.
3. Use stable Git/worktree identity rather than path-shape heuristics.
4. Test local, Direct, and Tunneled projects.
5. Run Agent Threads and Agent History tests, formatting, and clippy.
6. Open a PR titled `agent_threads: Group bare-checkout sessions correctly`.

### Task 4.6: Preserve coding-agent file references on terminal drop

**Upstream:** Zed PR #57747, commit
`7e0f63412c60008f9dae7fcf65fc6ab6d7e0f957`

**Classification:** Adaptation.

**Branch:** `fix/terminal-file-drop-escaping`

**Starting files:**

- `crates/terminal_view/src/terminal_view.rs`
- Terminal drag-and-drop tests

**Steps:**

1. Add tests for spaces, quotes, backslashes, non-ASCII paths, and multiple
   files.
2. Assert the inserted shell text refers to the original file and remains
   recognizable by Codex, Claude, and Pi command-line interfaces.
3. Keep escaping platform- and shell-aware.
4. Run terminal view tests, formatting, clippy, and the local app bundle.
5. Open a PR titled `terminal_view: Preserve file references on drop`.

### Task 4.7: Open terminal links while mouse reporting is active

**Upstream:** Zed PR #60067, commit
`f5c975162cf217f2c9cd1a2c1192eb2bb4653cdc`

**Classification:** Adaptation.

**Branch:** `feature/terminal-links-in-mouse-mode`

**Starting files:**

- `crates/terminal/src/terminal_settings.rs`
- `crates/terminal/src/mappings/mouse.rs`
- `crates/terminal_view/src/terminal_view.rs`
- `crates/settings_content/src/terminal.rs`

**Steps:**

1. Add tests for Cmd/Ctrl-click with mouse reporting on and off.
2. Add and expose `terminal.open_links_in_mouse_mode` only if it is absent.
3. Verify the exact Settings Editor path.
4. Ensure ordinary clicks still reach terminal applications.
5. Test links in Vim, Claude, OpenCode, and a plain shell.
6. Run terminal, terminal view, settings, formatting, clippy, and bundle
   checks.
7. Open a PR titled `terminal: Open links while mouse reporting is active`.

### Wave 4 completion

Audit every supported agent against settings/defaults, Settings Editor,
visibility, actions, history/resume, local behavior, Direct, and Tunneled
behavior. Record every omission as an explicit capability result.

## Wave 5: Git Workflow

Wave 5 starts only after Wave 1 is complete.

### Task 5.1: Group staged and unstaged changes

**Upstream:** Zed PR #59884, commit
`65e1c5af258d4c80036467d583691f3f9ded0897`

**Classification:** Adaptation.

**Branch:** `feature/git-panel-staging-groups`

**Starting files:**

- `crates/git_ui/src/git_panel.rs`
- `crates/git_ui/src/git_panel_settings.rs`
- `crates/settings_content/src/`
- `crates/settings_ui/src/`

**Steps:**

1. Add panel tests for staged, unstaged, partially staged, untracked, and
   conflicted files.
2. Add staging as an explicit grouping mode.
3. Add stage/unstage section controls and keyboard navigation.
4. Persist the grouping selection.
5. Expose and test the exact Settings Editor path.
6. Run Git UI, settings, formatting, clippy, and bundle checks.
7. Open a PR titled `git_ui: Group staged and unstaged changes`.

### Task 5.2: Add staged and unstaged multibuffers

**Upstream:** Zed PR #46541, commit
`c31b2b0dc7180247b2981eb084594efaf11ee396`

**Classification:** Reimplementation.

**Depends on:** Task 5.1.

**Branch:** `feature/git-staged-unstaged-multibuffers`

**Starting files:**

- `crates/git_ui/src/multi_diff_view.rs`
- `crates/git_ui/src/project_diff.rs`
- `crates/git_ui/src/text_diff_view.rs`
- `crates/git_ui/src/git_ui.rs`
- `crates/workspace/src/`

**Steps:**

1. Add actions for viewing all staged and all unstaged changes.
2. Add tests for view construction, refresh, partially staged files, file
   deletion, rename, and repository switching.
3. Build the staged view on the index diff and the unstaged view on the
   worktree diff without duplicating diff computation.
4. Preserve stable selections and scroll anchors across refresh.
5. Add action registration and command-palette coverage.
6. Run Wave 1 Git safety regressions with the new view tests.
7. Open a PR titled `git_ui: Add staged and unstaged change views`.

### Task 5.3: Add hunk actions to staging multibuffers

**Depends on:** Task 5.2.

**Branch:** `feature/git-multibuffer-hunk-actions`

**Starting files:**

- `crates/git_ui/src/multi_diff_view.rs`
- `crates/git_ui/src/text_diff_view.rs`
- `crates/git_ui/src/git_panel.rs`

**Steps:**

1. Add stage, unstage, restore, and restore-all actions.
2. Test repeated identical lines, adjacent hunks, partially staged files,
   binary files, conflicts, and deleted files.
3. Require confirmation for destructive restore-all behavior.
4. Propagate repository and index errors to the UI.
5. Run every Wave 1 regression.
6. Open a PR titled `git_ui: Add hunk actions to change views`.

### Task 5.4: Add Git panel view options and migrate settings

**Upstream:** Zed PR #59043, commit
`076fd14c88336fca9d2a4093452f3820c27453dd`

**Classification:** Adaptation.

**Depends on:** Task 5.1.

**Branch:** `feature/git-panel-view-options`

**Starting files:**

- `crates/git_ui/src/git_panel.rs`
- `crates/git_ui/src/git_panel_settings.rs`
- `crates/settings_content/src/`
- `crates/settings_ui/src/`
- `assets/settings/default.json`

**Steps:**

1. Audit the baseline-present compare-with-branch, dedicated diff, and split
   history controls so they are not reimplemented.
2. Add list/tree, sort-by-path/name, group-by-none/status/staging controls.
3. Migrate `git_panel.sort_by_path` deterministically if Flint still accepts
   it.
4. Add serialization, Settings Editor, panel rendering, and keyboard tests.
5. Open a PR titled `git_ui: Add Git panel view options`.

### Wave 5 completion

Run all Git UI tests plus Wave 1 regressions against local and remote
repositories. Manually verify staging, unstaging, restoring, branch comparison,
single-file diffs, and split history diffs.

## Wave 6: Search and Picker Modernization

Tasks 6.1 and 6.2 landed independently. Deliver the remaining Tasks 6.3
through 6.6 together on `feature/search-picker-modernization` in one
integration PR titled `search: Modernize symbol and LSP pickers`. Keep
reviewable feature slices as separate commits within that branch, then run the
Wave 6 completion gate against their integrated state.

### Task 6.1: Add a resizable picker with File Finder preview

**Upstream:** Zed PR #59604, commit
`ccf4058b7a6b05207d4f1dd25106ec5fc439cc74`

**Classification:** Reimplementation.

**Branch:** `feature/file-finder-preview`

**Starting files:**

- `crates/picker/src/picker.rs`
- `crates/picker/src/head.rs`
- `crates/file_finder/src/file_finder.rs`
- `crates/file_finder/src/file_finder_tests.rs`
- `crates/workspace/src/`

**Steps:**

1. Add picker layout tests for right-side, bottom, hidden, and resized preview
   placement.
2. Introduce only the reusable layout needed by File Finder.
3. Add cancellable File Finder preview loading.
4. Prevent stale preview completion from replacing the current selection.
5. Preserve existing File Finder actions, query behavior, and keybindings.
6. Persist size only if the existing workspace persistence model has an
   appropriate scope.
7. Run picker and File Finder tests, formatting, clippy, and bundle checks.
8. Open a PR titled `file_finder: Add resizable file previews`.

### Task 6.2: Add Text Finder with live preview

**Depends on:** Task 6.1.

**Branch:** `feature/text-finder-preview`

**Starting files:**

- `crates/search/src/`
- `crates/picker/src/`
- `crates/workspace/src/`
- Existing project-search tests

**Steps:**

1. Define Text Finder as another view over project-search state.
2. Add tests for query, regex, include/exclude filters, ignored files,
   cancellation, and opening at the matched row and column.
3. Share search state without duplicating the search engine.
4. Add preview rendering and result-group collapse behavior.
5. Preserve the existing project-search UI and actions.
6. Open a PR titled `search: Add Text Finder with live preview`.

### Task 6.3: Add symbol picker previews

**Depends on:** Task 6.1.

**Integrated branch:** `feature/search-picker-modernization`

**Starting files:**

- Symbol and outline picker implementations in `crates/editor` and
  `crates/workspace`
- `crates/picker/src/`

**Steps:**

1. Resolve the final upstream project-symbol and buffer-symbol preview PRs.
2. Add preview navigation tests for local, remote, and unsaved buffers.
3. Reuse the picker preview contract from Task 6.1.
4. Commit the symbol-preview slice independently on the integrated branch.

### Task 6.4: Add LSP result pickers

**Upstream:** Zed PR #59838, commit
`63692b8b4724357fa63d6318b45f3c3fee6f672a`

**Classification:** Reimplementation.

**Depends on:** Tasks 6.1 and 6.3.

**Integrated branch:** `feature/search-picker-modernization`

**Starting files:**

- `crates/search/src/`
- `crates/editor/src/`
- `crates/project/src/lsp_store.rs`
- `crates/picker/src/`

**Steps:**

1. Add `lsp_results_location` and per-action `open_results_in` only after
   defining migration and defaults.
2. Test definitions, references, implementations, multiple servers, duplicate
   locations, missing files, and remote results.
3. Ensure the picker does not trigger duplicate LSP queries while navigating.
4. Expose exact Settings Editor paths.
5. Commit the LSP-result slice independently on the integrated branch.

### Task 6.5: Reopen the last picker

**Depends on:** Tasks 6.1-6.4.

**Integrated branch:** `feature/search-picker-modernization`

**Starting files:**

- `crates/workspace/src/workspace.rs`
- `crates/picker/src/`
- Action and keymap definitions

**Steps:**

1. Store a reconstructible picker request, not a live view or stale entity.
2. Add tests for File Finder, Text Finder, symbol, and LSP pickers.
3. Clear history when its project or required capability disappears.
4. Commit the picker-reconstruction slice independently on the integrated
   branch.

### Task 6.6: Add picker multi-select

**Upstream:**

- #59931 `94b6d377badf9c2202850b551c4700a54b83895f`
- #60919 `90b3aa0b3bd3b453775b11a386907c7ac9acd997`

**Classification:** Reimplementation of two upstream PRs in the integrated
Wave 6 PR.

#### Task 6.6a: Add the picker selection model

**Integrated branch:** `feature/search-picker-modernization`

Add a selection collection with deterministic keyboard and pointer behavior.
Test selection across filtering, reordering, async refresh, disabled entries,
and picker close. Add File Finder and Text Finder consumers. Commit the
selection-model slice independently on the integrated branch.

#### Task 6.6b: Add multi-select controls and accessibility

**Integrated branch:** `feature/search-picker-modernization`

Add checkboxes, the mode button, shortcuts, focus behavior, and accessible
labels. Test narrow layouts and keyboard-only use. Commit the controls slice
independently on the integrated branch.

### Wave 6 completion

Run picker, File Finder, search, editor, project, and workspace tests. Build the
app and manually verify resizing, preview cancellation, search-state sharing,
LSP navigation, reopening, and multi-select. Open the integrated PR titled
`search: Modernize symbol and LSP pickers`.

## Wave 7: Performance and Settings Compatibility

### Task 7.1: Port measured hot-path improvements

**Upstream:**

- #61275 `ac5538b7239196b7413da76a6258bc9ac4a017fe`
- #58881 `253606e8e0396da2d6897c1eb996ea92aece23c4`

**Classification:** Split by independently measurable behavior.

**Steps:**

1. Decompose #61275 into separate candidate entries for anchor resolution,
   line shaping, and worktree scanning.
2. For each candidate, add or reuse a benchmark that exercises the changed
   path.
3. Port only improvements that produce a repeatable benefit without reducing
   correctness coverage.
4. Handle crash-handler background spawning as a separate PR because it affects
   startup and process lifecycle.
5. Confirm structural sharing from #58681 is baseline-present and retain it in
   the benchmark baseline.
6. Use one branch and PR per measurable subsystem, with titles such as
   `editor: Reduce anchor resolution overhead`.

### Task 7.2: Decide the format-on-save default

**Upstream:** Zed PR #59710, commit
`76e07d5c9ac38930d051c153b21eeb57ba71cbb4`

**Classification:** Product decision before code.

**Branch:** `design/format-on-save-default`

**Starting files:**

- `assets/settings/default.json`
- `crates/settings_content/src/language.rs`
- Formatter and settings tests

**Steps:**

1. Inventory Flint's current global and per-language defaults.
2. Measure how many official-formatter language defaults would retain
   formatting.
3. Document compatibility options:
   preserve Flint behavior, adopt Zed behavior, or migrate only new users.
4. Obtain explicit approval before changing the default.
5. If approved, add default and migration tests before editing settings.
6. Deliver the decision and implementation as separate PRs when behavior
   changes.

### Task 7.3: Audit external-agent Settings Editor coverage

**Upstream:** Zed PR #59860, commit
`40d20036af34343a09f0ce6a2eb38c9e5a60e9ae`

**Classification:** Native provider and MCP portions are excluded; external
agent controls require a Flint-specific audit.

**Branch:** `test/agent-thread-settings-coverage`

**Starting files:**

- `crates/settings_ui/src/pages.rs`
- `crates/settings_ui/src/pages/`
- `crates/settings_content/src/agent_threads.rs`
- `crates/agent_threads/src/`

**Steps:**

1. Record the removed Zed language-model provider and MCP settings as
   `excluded-by-product-boundary`.
2. Inventory every currently user-editable external agent, credential field,
   route control, and plan-usage control.
3. Define explicit capabilities for each control.
4. Add Settings Editor tests for every applicable exact JSON path.
5. Keep Direct remote agents free of Flint-managed launch and credential
   controls.
6. Gate credentials and plan usage by capability, not registry membership.
7. Add only missing Flint controls or coverage; do not restore Zed provider or
   MCP pages.
8. Open a PR titled `settings_ui: Complete Agent Threads settings coverage`
   only if the audit finds a gap.

### Task 7.4: Fix npm 12 language-server installation

**Upstream:** Zed PR #60870, commit
`b9bfd5722e6520cdb54378c2d8a341edf5981e6d`

**Classification:** Cherry-pick candidate.

**Branch:** `fix/npm-12-language-server-install`

**Starting files:**

- `crates/node_runtime/src/`
- Node runtime tests

**Steps:**

1. Add npm 12 output fixtures that reproduce deserialization failure.
2. Preserve compatibility with earlier npm output.
3. Port the parsing fix.
4. Run:

   ```sh
   cargo test -p node_runtime
   cargo fmt --all -- --check
   ./script/clippy
   ```

5. Open a PR titled `node_runtime: Support npm 12 output`.

### Task 7.5: Define TypeScript 7 behavior

**Upstream:**

- #60970 `a25f19cb2f55baa4cf8638981043ac64af741d62`
- inspect later vtsls correction #61126 and the final `tsgo` extension guidance

**Classification:** Dependency and compatibility decision.

**Branch:** `fix/typescript-7-language-server`

**Starting files:**

- `crates/languages/src/typescript.rs`
- TypeScript language-server installation tests
- Extension recommendations

**Steps:**

1. Test TypeScript 6 and 7 projects with Flint's current vtsls and
   typescript-language-server setup.
2. Determine whether pinning, tsgo recommendation, or both matches the final
   upstream behavior.
3. Avoid showing a false invalid-tsserver error.
4. Keep the external extension API unchanged.
5. Open a PR titled `languages: Handle TypeScript 7 projects`.

### Task 7.6: Close baseline-present compatibility entries

**Branch:** `test/upstream-baseline-compatibility`

Add or identify regression coverage for:

- safe symlink trashing;
- local-only sandbox terminal temporary directories;
- remote agent-terminal restoration;
- compare with branch;
- dedicated Git panel file diffs;
- split commit-history diffs;
- structural sharing for large text edits; and
- extension-managed x86 download removal.

Do not reimplement these changes. Update their ledger entries with the current
test or source evidence. Open a test PR only where an important baseline
behavior lacks coverage.

### Wave 7 completion

Record a decision for every settings compatibility entry. No setting is left
in `investigating`, and every performance port includes a reproducible
measurement.

## Wave 8: Final Audit and Recurring Maintenance

### Task 8.1: Reconcile all v1.6-v1.12 release entries

**Branch:** `docs/zed-v1.12-reconciliation`

**Files:**

- `docs/superpowers/zed-upstream-ledger.md`
- Add a dated reconciliation report under `docs/superpowers/specs/`

**Steps:**

1. Export stable and preview release-note PRs.
2. Deduplicate by upstream PR.
3. Prove fork-point ancestry for each candidate.
4. Classify remaining editor, language, debugger, platform, and extension
   entries.
5. Record every item as landed, deferred, superseded, baseline-present, or
   excluded.
6. Open a documentation PR titled `Reconcile Zed v1.12 upstream changes`.

### Task 8.2: Run the program compatibility matrix

**Branch:** `test/zed-v1.12-compatibility-matrix`

Run or add automated coverage for:

- macOS, Linux, and Windows;
- local workspaces;
- Direct remote workspaces;
- Tunneled remote workspaces;
- Git safety and staging;
- Agent Threads launch, history, resume, and completion;
- File/Text Finder and LSP result pickers;
- settings migration; and
- extension loading and remote synchronization.

Build `/tmp/Flint-Local.app` and perform the user-visible smoke suite. Open
focused test PRs for missing automation rather than one large mixed PR.

### Task 8.3: Establish recurring upstream review

**Branch:** `docs/zed-upstream-review-process`

**Files:**

- `docs/superpowers/zed-upstream-ledger.md`
- Appropriate contributor or maintenance documentation

**Steps:**

1. Record the completed upstream baseline by Zed commit and tags.
2. Assign an owner and review cadence.
3. Define stable/preview deduplication by PR.
4. Require immediate triage of safety, corruption, security, hang, and data-loss
   fixes.
5. Require later-main correction review before implementation.
6. Keep intentional product-boundary exclusions explicit.
7. Open a documentation PR titled `Document recurring Zed upstream review`.

## Final Program Definition of Done

The P0-P2 program is complete when:

- every seeded ledger entry has a terminal classification;
- all applicable Wave 1 safety changes are landed and remain covered;
- platform resource fixes are landed or explicitly deferred per platform;
- Direct and Tunneled remote route tests pass;
- every Agent Threads addition has an explicit provider capability result;
- Git workflow changes pass the Wave 1 regression suite;
- picker features are independently usable and preserve existing actions;
- every settings change has a migration or an explicit preserve-current
  decision;
- performance changes have reproducible measurements;
- the v1.6-v1.12 reconciliation has no unclassified P0-P2 item; and
- the recurring upstream review process has a named owner and baseline.
