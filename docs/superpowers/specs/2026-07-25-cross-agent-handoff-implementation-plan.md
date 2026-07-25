# Cross-Agent Handoff Implementation Plan

**Date:** 2026-07-25
**Status:** Draft
**Design:** `2026-07-25-cross-agent-handoff-design.md`

This plan sequences the approved design into reviewable pull requests. Each
phase is independently mergeable, testable, and ordered so earlier phases carry
no user-facing surface or disclosure risk until the security model lands.

The classifier facts below were verified against real on-disk sessions (Codex
0.145.0, Claude 2.1.219, Pi 0.81.1); the observed record-type distributions are
recorded as the fixture targets each phase must reproduce.

## Verified format facts (fixture targets)

### Codex rollout (`response_item` canonical)

Observed record mix in one real rollout (~228 records): `response_item/reasoning`
63, `event_msg/token_count` 48, `response_item/function_call` 47,
`response_item/function_call_output` 47, `response_item/message` 11,
`event_msg/agent_message` 6, `event_msg/user_message` 1, plus singletons
`session_meta`, `world_state`, `turn_context`, `event_msg/task_started`,
`event_msg/task_complete`.

- Canonical stream: `response_item` (`message`, `function_call`/`_output`,
  `custom_tool_call`/`_output`).
- Emit content only from `response_item`; `event_msg/{user_message,agent_message}`
  duplicate it and are consulted only for lifecycle/errors.
- Exclude `reasoning` and `token_count` (together the majority of records).
- `custom_tool_call*` did not appear in this sample but is documented; handle
  defensively with a fixture.

### Claude transcript (`type` whitelist)

Observed top-level `type` mix: `assistant` 89, `user` 54 (+3 `isMeta`),
`system` 28, `attachment` 20, `last-prompt` 18, `mode` 15, `ai-title` 15,
`permission-mode` 15, `queue-operation` 13, `file-history-snapshot` 11,
`file-history-delta` 1. Assistant blocks: `tool_use` 42, `text` 28,
`thinking` 24. User records: 42 are `tool_result`-only, 19 carry text.

- Whitelist `user`/`assistant`; coalesce all other top-level types as noise;
  keep `file-history-*` as checkpoints.
- `tool_result`-only user records (the majority) classify as tool results.
- Assistant `thinking` blocks excluded; `text` is content; `tool_use` is a call.
- Never open `<session>/subagents/agent-*.jsonl` (`isSidechain: true`).

### Pi session (parent-chain tree)

Observed: `parentId` present on 7 of 8 lines; types `session`, `message`
(user/assistant), `model_change`, `thinking_level_change`.

- Build `id -> entry`, take last valid entry as leaf, walk `parentId` to root.
- Coalesce `model_change`/`thinking_level_change` as noise; keep compaction and
  branch-summary as checkpoints.
- Tools: `toolCall{arguments}` and separate `toolResult` messages.

## Phase 1 — Host-side extraction (no UI, no disclosure surface)

Goal: `agent_history` can turn a validated locator into a bounded, redacted,
Markdown excerpt. Pure library work, fully unit-testable, zero product surface.

Crate: `crates/agent_history`.

1. Add `TranscriptLocator { source_path, session_id, working_dir, FileIdentity }`
   and have each `HistoryProvider::refresh` populate one per `IndexedSession`
   (extend the persisted index row; bump `SCHEMA_VERSION`).
2. Add the normalized model: `TranscriptTurn` (user text / assistant text /
   tool call+result pair / checkpoint / coalesced-noise) and `TranscriptExcerpt`
   (turns, `degraded`, `malformed_count`, `unknown_count`,
   `possibly_incomplete`).
3. Add a `classify` method per provider (`claude.rs`, `codex.rs`, `pi.rs`)
   producing `Vec<RawEvent>` per the verified facts above, incrementing
   diagnostics for unknown records rather than dropping silently.
4. Add one shared `select_and_render(events, budget) -> TranscriptExcerpt`:
   first-user-turn head, newest-first tail within byte budget restored to
   chronological order, call/result pairing (prefer unresolved/failed),
   structural tool summaries (`name`, command/path, exit, `is_error`, output
   tail), checkpoint preservation, single coalesced noise marker, one total
   document cap, char-boundary-safe truncation. No raw source path in output.
5. Add trait method
   `extract_transcript(host, locator, budget) -> Result<TranscriptExcerpt>`:
   validate the locator against the current index, one-shot `HistoryFs::load`,
   parse line-delimited JSON (torn final line dropped), classify, select,
   render. Bump `PARSER_VERSION`.

Tests (fixtures committed under the crate):
- Codex: `response_item` vs `event_msg` de-duplication; both tool-call forms;
  `reasoning`/`token_count` excluded; `developer` role not user.
- Claude: `type` whitelist drops the ~10 metadata types; `tool_result`-only
  user rows classify as results; `thinking` excluded; subagent files never read.
- Pi: leaf-to-root parent-chain reconstruction discards a sibling branch;
  checkpoints preserved.
- Selection: pair never split; unresolved/failed preferred; total cap honored;
  raw path absent from rendered output.
- Degradation: unknown records set `degraded` without consuming budget; zero
  surviving turns returns the refusal sentinel.

## Phase 2 — RPC and host wiring

Goal: extraction is reachable for both local and remote projects on the correct
host, request/response (not streaming).

Crates: `crates/proto`, `crates/remote_server`, and the client caller.

1. `flint.proto`: add `ExtractAgentTranscript` (project_id, kind,
   normalized_history_root, session_id/locator fields, budget) and
   `AgentTranscriptExcerpt` (rendered Markdown, diagnostics, `possibly_incomplete`).
   Follow the `StreamAgentThreadHistory`/`AgentThreadHistorySnapshot` field
   style at `flint.proto:490`.
2. `proto.rs`: register both in the entity-message list (near `:283`) and add
   the request/response pair to the non-streaming list (the same list holding
   `ReadRemoteFile`), not the stream pairing at `:472`.
3. `remote_server/src/headless_project.rs`: add
   `handle_extract_agent_transcript` via `add_request_handler` (mirroring
   `handle_stream_agent_thread_history` registration at `:244`), calling
   Phase 1 host-side.
4. Client: a request/response caller that uses the local `agent_history` path
   for local projects and `.request(ExtractAgentTranscript)` for remote (gated
   by the same capability that gates history), never a client-side
   `ReadRemoteFile` on the transcript.

Tests: a `remote_editing_tests.rs` case (alongside the existing history stream
test at `:2678`) asserting extraction runs host-side and returns an excerpt for
a remote project; a local-path test asserting no RPC is issued locally.

## Phase 3 — Handoff document, redaction, and consent

Goal: write the disposable handoff document safely on the target's host with the
disclosure model, behind an explicit preview + confirm.

Crate: `crates/agent_threads` (plus a small host op for remote writes).

1. Document writer: `.flint/handoffs/<random-or-hash>.md`, atomic write, private
   permissions, alongside a generated `.flint/handoffs/.gitignore` containing
   `*`. Written on the target's host (host op for remote; local `fs` for local).
   TTL/explicit cleanup.
2. Default contents: source metadata, `git diff --stat`-style changed-file names
   (no raw diff), the Phase 1 excerpt (no raw tool-result bodies), diagnostics.
   Raw diff / raw bodies gated behind an opt-in flag on the request.
3. Preview + consent UI: show the exact document and a "send this context to
   `<target provider>`" confirmation; nothing is written to a launch-visible
   location and no target launches until confirmed.
4. Refusal path: a `degraded`/zero-turn excerpt surfaces the refusal rather than
   writing a misleading document.

Tests: `.gitignore` created and document absent from `git status`; default
document contains no raw diff or tool bodies, opt-in includes them; refusal on
zero surviving turns; remote document written host-side.

## Phase 4 — Launch seam and Codex identity discovery

Goal: launch the target seeded to read the document, and make fresh-Codex
handoff work.

Crate: `crates/agent_threads`.

1. `InitialPromptStrategy` per kind in the launch layer: append the bootstrap
   prompt to argv after all flags; do not add a field to `SpawnInTerminal`
   (`store.rs:1557` keeps translating a finished `AgentLaunchCommand`).
   Bootstrap prompt frames the excerpt as quoted untrusted data. Degrade to
   "open terminal, show prompt for manual paste" when a wrapper command can't
   take a positional prompt.
2. Session-identity strategy replacing the `session_id_flag`-is-everything
   model: `AssignedByFlag` (Claude, Pi), `DiscoverFromHistory` (Codex),
   `Unavailable`.
3. Codex discovery: snapshot known rollout ids at launch; on refresh/handoff,
   match new rollout files by `session_meta.cwd` + post-launch timestamp,
   excluding ids bound to other live terminals; exactly one binds
   automatically, multiple show a picker, none retries briefly then explains.
4. Panel entry point: a "Hand off to…" action on an `AgentThreadRow` offering
   target kinds other than the current one; disabled with an explanation when
   identity is `Unavailable` and undiscoverable.

Tests: strategy appends after flags and degrades on wrapper commands; Codex
discovery binds a single candidate, pickers on multiple, explains none, excludes
already-bound ids; the panel action hides the current kind and disables when
unavailable.

## Cross-cutting

- Every phase runs `cargo fmt --all -- --check`, Flint's `script/clippy` for
  affected crates, and the touched crates' test suites; Phase 2+ also runs the
  remote-server library tests and the Linux musl remote-server build.
- Each PR title is imperative, crate-scoped where clear (e.g.
  `agent_history: Add transcript extraction`), and carries a `Release Notes:`
  section (`- N/A` through Phase 2; `- Added ...` from Phase 3 when a user
  surface appears).
- Live validation from the design's *Live Validation* section runs after
  Phase 4 against a fresh `/tmp/Flint-Local.app`.

## Open items to resolve during implementation

- Confirm `custom_tool_call*` field names against a rollout that contains them
  (absent from the sampled session) before finalizing the Codex fixture.
- Confirm the exact capability flag that gates remote history, to reuse it for
  extraction rather than introducing a parallel gate.
- Decide the concrete byte budget constants against a few real excerpts so the
  rendered document lands in a useful size band; the design fixes the algorithm,
  not the numbers.
