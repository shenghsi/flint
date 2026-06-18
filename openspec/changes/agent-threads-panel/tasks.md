## 1. Remove Existing Terminal Thread Organizer

- [ ] 1.1 Delete `crates/terminal_view/src/terminal_threads.rs` and remove its `mod`/`init(cx)` call in `crates/terminal_view/src/terminal_view.rs`
- [ ] 1.2 Delete `crates/settings_content/src/terminal_threads.rs` and remove its `mod`/`pub use`/field from `crates/settings_content/src/settings_content.rs`
- [ ] 1.3 Remove the 4 terminal-thread menu items from `crates/flint/src/flint/app_menus.rs`
- [ ] 1.4 Remove the `"terminal_threads"` block from `assets/settings/default.json`

## 2. Scaffold `agent_threads` Crate

- [ ] 2.1 Create `crates/agent_threads` with `[lib] path = "src/agent_threads.rs"`, add it to the workspace `Cargo.toml`
- [ ] 2.2 Add `crates/settings_content/src/agent_threads.rs` (codex/claude command content + `max_visible_threads_per_agent`, default 5) and register it in `settings_content.rs`
- [ ] 2.3 Add the `"agent_threads"` block to `assets/settings/default.json`
- [ ] 2.4 Add `agent_threads::{Toggle, ToggleFocus}` to `crates/flint_actions/src/lib.rs`

## 3. Agent Registry & Live Store

- [ ] 3.1 Define `AgentKindDefinition` (id, label, icon, default command, optional `Arc<dyn AgentHistoryProvider>`, `resume_options: Vec<ResumeOption>`) and build the code-level registry (Codex, Claude) in `init()`
- [ ] 3.2 Implement `AgentThreadStore`: live-thread bookkeeping keyed by kind id, tracking `resumed_session_id` for dedup, title/attention/last-activity updates from `TerminalView`/`TerminalEvent`
- [ ] 3.3 Implement new-thread and resume-thread spawning via `TerminalPanel::add_center_terminal_view`, tagging resumed threads with their session id

## 4. History Providers

- [ ] 4.1 Implement the Claude history provider: read `$CLAUDE_CONFIG_DIR`/`~/.claude/history.jsonl`, group by `sessionId` keeping the max-timestamp entry, filter by worktree-root match
- [ ] 4.2 Implement the Codex history provider: list rollout files under `$CODEX_HOME`/`~/.codex/sessions` capped to the most recent ~200, read each file's first line for `cwd`/`id`/`timestamp`, join titles from `session_index.jsonl`
- [ ] 4.3 Implement the two-tier dedup (exact id match; same-kind/same-project/launch-time heuristic) when building the merged live+historical view
- [ ] 4.4 Add `fs.watch`-based refresh for both providers' source files/directories (debounced), avoiding polling

## 5. Panel UI

- [ ] 5.1 Implement `AgentThreadsPanel` (`Panel` impl, default dock `Left`, valid `Left | Right`), registered in `crates/flint/src/flint.rs` `initialize_panels`
- [ ] 5.2 Render one section per registry entry: icon + label + count + `Disclosure` fold/unfold + "+" button wired to the new-thread action
- [ ] 5.3 Render merged rows capped to `max_visible_threads_per_agent`, with a trailing "Show more" row that lifts the cap for that section
- [ ] 5.4 Wire live-row click to focus the existing terminal tab; historical-row left-click to default resume
- [ ] 5.5 Add the resume-with-options context menu (right-click plus an on-hover affordance) listing the kind's `resume_options`
- [ ] 5.6 Add a per-section empty state ("No Codex threads yet")

## 6. Tests

- [ ] 6.1 Unit tests for the Claude history provider's parsing/grouping/filtering against a fixture `history.jsonl`
- [ ] 6.2 Unit tests for the Codex history provider's join/cap/cwd-filter against fixture rollout files + `session_index.jsonl`
- [ ] 6.3 Unit tests for both dedup rules, including the documented multi-concurrent-thread edge case
- [ ] 6.4 Panel integration tests (FakeFs/echo commands, mirroring the removed `terminal_thread_*` tests): live focus, cap + "Show more", fold/unfold, default resume command line, resume-with-options command line
