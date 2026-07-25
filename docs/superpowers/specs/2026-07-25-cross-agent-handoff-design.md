# Cross-Agent Handoff Design

**Date:** 2026-07-25
**Status:** Approved; pending implementation plan

## Problem

Flint's Agent Threads panel runs Claude, Codex, and Pi as raw CLI processes in
terminals. Each CLI keeps its own private on-disk session history and there is
no shared conversation state between kinds. When one agent runs out of usage
quota part-way through a task, the user's only options today are to wait for the
quota to reset or to switch to a different agent and re-explain the entire task
by hand.

The switch must work even when the source agent is **dead** — quota-locked,
hung, or its terminal already closed. Flint therefore cannot ask the source
agent to summarize itself. The handoff document must be assembled purely from
files already on disk.

The source and target are frequently different providers, so a handoff crosses a
new data-disclosure boundary: content the user shared with provider A (including
diffs, tool output, and anything pasted into the conversation) would flow to
provider B. That boundary, not transcript fidelity, is the dominant risk.

## Scope

This change adds a "hand off to another agent" capability: Flint reconstructs a
bounded, redacted excerpt of a source thread's transcript from the CLI's history
files, writes a handoff document, and launches a new thread of a different kind
seeded to read that document and continue.

It does **not**:

- ask the source agent to produce the summary, or require the source process to
  be alive;
- attempt to translate one CLI's native session format into another's so the
  target can natively resume it (the target starts a fresh session that *reads*
  the handoff, it does not adopt the source's session);
- transfer credentials, provider plan state, or authentication between kinds;
- send the source transcript to any network service;
- guarantee a byte-exact or lossless transcript — the excerpt is a bounded,
  summarized projection;
- change how existing history scanning, resume, or restore behave; or
- add a new persisted store — handoff documents are disposable working files.

## Product Invariants

### Handoff is host-owned and route-correct

Transcript extraction and handoff-document writing run on the host that owns the
session files, exactly like history scanning does today. For a local project
that is the local machine; for a remote project it is `flint-remote-server`.
The client never reads a remote transcript over a raw filesystem RPC, and the
handoff document is written on the host where the **target** agent will run, so
a remote target can read it.

This mirrors the existing boundary in `crates/agent_history`, which performs all
discovery, validation, and parsing host-side and serves only compact snapshots
to the client. New transcript parsing belongs in that crate, not in the legacy
client-side `agent_threads::AgentHistoryProvider` trait, which is only a
fallback path.

### The source is never trusted to be alive

Every step reads persisted files. A dead, quota-locked, or closed source thread
produces the same handoff as a live one. Reading a still-active source is
supported but is a lower-priority case; see *Concurrent Reads*.

### Minimal disclosure by default, explicit consent to send

A handoff crosses providers. The default document contains the least context
that is still useful: task framing, a structural summary of recent work, and a
changed-file list. Raw diffs and raw tool-result bodies are opt-in. The user
sees a preview and explicitly confirms sending the context to the named target
provider before any target process starts.

### Correct-or-refuse, never confidently wrong

Parsing degrades loudly. Unknown records are counted, not silently dropped into
a plausible-looking summary. If no trustworthy conversation turns survive
extraction, Flint refuses the automatic handoff and tells the user, rather than
emitting an empty or misleading document.

## Architecture

### Extraction lives in `crates/agent_history`

`crates/agent_history` already owns per-kind history providers
(`ClaudeHistoryProvider`, `CodexHistoryProvider`, `PiHistoryProvider`), a
host-abstracted filesystem (`HistoryFs`), file-identity change detection
(`FileIdentity` = mtime + length), and a `PARSER_VERSION` cache-invalidation
discipline. Transcript extraction is added here as a new provider capability so
it reuses all of the above and stays on the correct side of the remote boundary.

Extraction is exposed through the same host operation surface that already
serves history snapshots locally and over RPC, so remote extraction is a
capability-gated host call, not a client-side file read.

### Transcript locators, not raw paths

The client identifies a source thread by an **index-issued locator**, never by
handing the host an arbitrary path to read. During its normal refresh, each
provider records for every session a `TranscriptLocator` carrying the source
file path, the expected session id, the working directory, and the file's
`FileIdentity`. Handoff passes the locator back; the host validates it against
the current index before reading. This closes an ambiguity in resolving by
`(project_root, session_id)` alone: Codex deliberately does not deduplicate by
session id (one index row per rollout file), so a session id is not a unique
key there.

## Transcript Model and Per-Kind Classification

The three CLIs use materially different on-disk shapes. Extraction normalizes
each into a shared sequence of logical **turns** (user text, assistant text,
paired tool call+result, and coalesced noise), then applies one shared,
kind-agnostic selection and rendering pass. The per-kind classifiers are the
only kind-specific code.

The classifier details below were validated against real on-disk Codex
0.145.0, Claude 2.1.219, and Pi 0.81.1 sessions (record-type distributions and
block shapes confirmed directly) and must be locked with versioned fixtures; a
classifier change bumps `PARSER_VERSION`.

### Codex

- The canonical user/assistant/tool stream is the `response_item` records:
  `message`, `function_call` / `function_call_output` (using `arguments` /
  `output`), `custom_tool_call` / `custom_tool_call_output` (using `input` /
  `output`), and reasoning.
- `event_msg` records duplicate the canonical text (`user_message`,
  `agent_message`) and are consulted only for lifecycle and error metadata —
  never emitted as content, to avoid double-counting and chronology distortion.
- `message` records include a `developer` role that must not be classified as
  user content.
- Reasoning records may carry encrypted content or summaries and are never
  exported as assistant text.
- High-volume noise records (`reasoning`, `event_msg / token_count`) commonly
  outnumber conversation records several to one in a real rollout, which is why
  selection budgets over normalized turns rather than raw records.
- Records are deduplicated by record or call id where present.

### Claude

- Only the top-level `<session-id>.jsonl` file is read. Subagent transcripts
  under `<session-id>/subagents/agent-*.jsonl` (marked `isSidechain: true`) are
  **never** recursed into; the parent transcript already contains the parent
  Task tool result, which is the correct representation of subagent work.
- Conversation is selected by a **whitelist** of top-level `type`: only `user`
  and `assistant` records carry turns. The current format also emits roughly a
  dozen non-conversation top-level types (`system`, `attachment`, `last-prompt`,
  `mode`, `ai-title`, `permission-mode`, `queue-operation`,
  `file-history-snapshot`, `file-history-delta`). A blacklist of known
  synthetic prefixes would leak all of these, so extraction whitelists
  conversation types and coalesces everything else as noise. `file-history-*`
  records are preserved as checkpoints (Claude's analog to Pi compaction).
- A `type == "user"` record may consist solely of `tool_result` blocks — in
  practice this is the *majority* of user records in a tool-heavy session — and
  is classified as a tool result, not as user text. Only a `user` record
  containing a `text` block (or a plain-string content) is user text.
- Assistant records are classified per content block: `text` is assistant text,
  `tool_use` is a tool call, and `thinking` blocks are excluded.
- `isMeta: true` user records and the existing synthetic-prefix artifacts
  (`<local-command-`, `<command-`, `<bash-`, leading slash) remain excluded,
  matching the title-extraction filter already in `crates/agent_history`.

### Pi

- Pi's JSONL is an append-only tree keyed by `id` / `parentId`, not a linear
  log. The active conversation is the parent chain: build the `id -> entry`
  map, take the last valid entry as the leaf, and walk `parentId` to the root.
  Abandoned branches are discarded.
- Compaction and branch-summary records are semantic checkpoints and are
  preserved as such, not treated as unknown noise.
- Assistant tool blocks use `toolCall` with `arguments`; results are separate
  messages with role `toolResult` carrying `toolCallId`, `toolName`, `content`,
  and `isError`.

### Diagnostics

Every classifier increments malformed and unknown-record counters instead of
silently skipping. The excerpt carries these counts and a derived `degraded`
flag. Zero surviving trustworthy turns is a refusal condition, not an empty
success.

## Selection and Bounding

Selection operates on normalized turns, not raw bytes or raw record counts, so
provider-specific duplication and noise cannot exhaust the budget.

1. Keep the first non-synthetic user turn (the task framing), truncated on a
   UTF-8 character boundary.
2. Select recent logical turns newest-first within the remaining byte budget,
   then restore chronological order.
3. Pair each tool call with its result by call id and treat the pair as one
   indivisible selectable unit. Unresolved and failed calls are preferred —
   they represent the work the target must finish.
4. Summarize tools structurally — name, command or path, exit status, error
   state, and a bounded output tail — rather than taking an arbitrary leading
   slice, so the exit code and trailing error are never dropped.
5. Preserve recognized compaction and branch-summary checkpoints.
6. Coalesce omitted internal records into a single marker
   (for example `[23 internal records omitted]`) that does not consume the turn
   budget.
7. Enforce one total serialized-document cap covering the excerpt and all
   metadata, including any git summary.
8. Keep the absolute path of the raw source transcript in Flint's internal UI
   metadata only. It is never written into the agent-visible document, so the
   target cannot bypass the bounds and redaction by reading the source file.

Rendering is plain Markdown (`**User:**`, `**Agent:**`, `` **Tool `name`:** ``,
`**Result:**`) regardless of source kind, so the handoff reads identically no
matter which CLI produced it.

## Handoff Document and Launch

### The document

The handoff document is written under `.flint/handoffs/` on the target's host.
A `.flint/handoffs/.gitignore` containing `*` is created alongside it, because
the repository's root `.gitignore` does not currently ignore this path and a
secret-bearing document must never appear in `git status` or be committed.

Files are written atomically with private permissions and a random or hashed
name — never the raw session id, which is not guaranteed to be a path-safe
UUID. Documents are removed on a defined TTL or explicit cleanup.

Default contents:

- thread metadata (source kind, title, timestamps);
- a changed-file summary (`git diff --stat`-style names), **not** a raw diff;
- the bounded, structurally summarized transcript excerpt, with **no** raw
  tool-result bodies unless the user opts in;
- diagnostics (`degraded`, omitted counts).

Raw diff and raw tool-result bodies are available only through an explicit
opt-in in the preview.

### The launch

The target thread is launched as a fresh session of the chosen kind, seeded with
a bootstrap prompt instructing it to read the handoff document and continue.
The excerpt is framed in that prompt as quoted, untrusted historical data so a
malicious tool-result captured from the source cannot redirect the target
(prompt-injection defense).

Seeding uses a per-kind `InitialPromptStrategy` in the agent-launch layer that
appends the bootstrap prompt to the argument vector **after** all other flags
are applied. The generic `SpawnInTerminal` task type does not gain an
agent-specific prompt field; the launch layer continues to translate a completed
`AgentLaunchCommand` into a terminal task. All three CLIs accept a trailing
positional prompt today, but a user-configured wrapper command might not, so the
strategy degrades to opening the terminal and showing the prompt for manual
paste when the target does not support it.

## Session Identity and Fresh Codex Threads

Handoff needs a session id to locate the source transcript. Today Flint models
this as a single `session_id_flag`: Claude and Pi are assigned an id at launch,
Codex is not, so a fresh Codex thread's transcript is unlocatable.

This is replaced by a per-kind session-identity strategy:

- **AssignedByFlag** (Claude, Pi): Flint assigns the id at launch and always
  knows it.
- **DiscoverFromHistory** (Codex): Flint learns the id after the CLI writes it.
- **Unavailable**: neither works; handoff is disabled with an explanation.

For Codex discovery, Flint snapshots the set of known rollout identities at
launch. On history refresh or handoff invocation it looks for new rollout files
whose `session_meta.cwd` matches the thread's project root and whose timestamp
is after launch, excluding ids already bound to another live terminal. Exactly
one remaining candidate is bound to the live thread automatically; multiple
candidates present a picker with title and time rather than guessing; none
triggers a brief retry and then an explanation that Codex has not yet persisted
a transcript. This recovers most fresh-Codex handoffs instead of disabling them
outright.

## Concurrent Reads

The primary case is a dead source that is no longer writing, where a one-shot
read is sufficient: loading the file as a string and parsing line-delimited JSON
naturally drops a torn final record, and existing history parsing already
tolerates malformed lines.

For a **live** source still flushing, a one-shot whole-file read can end mid
multi-byte UTF-8 sequence or omit a record that completes moments later. A
fully robust read would capture `(length, mtime)` before reading, read at most
the captured length, parse only through the last complete newline, re-check file
identity afterward, and retry briefly if it changed — returning a valid excerpt
marked `possibly_incomplete` rather than failing. This requires a byte-level
read method that `HistoryFs` does not expose today (`load` returns a whole
`String`).

This design ships the one-shot read for the dead-source case and marks
live-source excerpts `possibly_incomplete`. The bounded byte-level read is a
deliberately deferred follow-up, not an oversight. No read can recover data that
never left the dead process's userspace buffer.

## Secret and Injection Handling

- Handoff crosses a provider boundary; the document is treated as sensitive by
  default (gitignored, private permissions, TTL cleanup, minimal contents).
- Deterministic truncation is not redaction: a truncated tool result can still
  contain a pasted credential. The mitigation is minimizing content by default,
  requiring explicit opt-in for raw bodies, and showing a preview before send —
  not claiming the excerpt is secret-free.
- The transcript excerpt is quoted as untrusted historical data in the bootstrap
  prompt so captured content cannot drive the target agent.

## Testing

### Extraction and classification

- Versioned fixtures for each kind covering: Codex `response_item` vs `event_msg`
  duplication, both Codex tool-call forms, Codex `developer` role and reasoning
  exclusion; Claude tool-result-only user records and subagent-sidechain
  non-recursion; Pi branch/leaf parent-chain reconstruction and compaction
  checkpoints.
- A truncated final record and a mid-UTF-8 truncation each yield a valid excerpt
  (the live case marked `possibly_incomplete`), never a parse panic.
- Zero surviving turns yields a refusal, not an empty document.
- Unknown records increment diagnostics and set `degraded` without consuming the
  turn budget.

### Selection and bounding

- Tool call and result stay paired; unresolved/failed calls are retained
  preferentially.
- The total serialized document respects one cap including metadata.
- The raw transcript path never appears in the agent-visible document.

### Document, launch, and identity

- `.flint/handoffs/.gitignore` is created and a written document does not appear
  in `git status`.
- Default document contains no raw diff and no raw tool-result bodies; opt-in
  includes them.
- Per-kind `InitialPromptStrategy` appends the prompt after all flags; an
  unsupported wrapper command degrades to manual paste.
- Codex identity discovery binds a single post-launch candidate, shows a picker
  for multiple, and explains none; a candidate already bound to another terminal
  is excluded.
- Remote target: extraction runs host-side and the document is written on the
  target's host, readable by a remote target process.

### Regression suites

Run the Agent Threads and `agent_history` library tests, the remote-server
library tests, formatting, Flint's clippy wrapper for affected crates, and the
Linux musl remote-server build used by the debug application bundle.

## Live Validation

Build and install a fresh `/tmp/Flint-Local.app`, preserving the prior bundle.

1. Start a Claude thread, do a few turns that edit files and run tools, then
   hand off to Codex. Confirm the preview shows a structural summary and a
   changed-file list with no raw diff, and that confirming launches a Codex
   thread that reads `.flint/handoffs/` and continues coherently.
2. Confirm the handoff document does not appear in `git status`.
3. Kill the Claude process (simulating quota lockout) and hand off from the now
   dead thread; confirm the handoff still succeeds from disk.
4. From a fresh Codex thread with no Flint-assigned id, confirm identity
   discovery binds the rollout and enables handoff; with two concurrent Codex
   threads in one project, confirm a picker rather than a wrong auto-bind.
5. On a remote (Through Flint) project, confirm extraction and document writing
   happen host-side and a remote target thread reads the document.

## Alternatives Rejected

### Ask the source agent to write its own handoff

Fails the core requirement: a quota-locked or hung source cannot run. It also
consumes source quota that is often the very thing that ran out. Reading from
disk is source-liveness-independent.

### Translate one CLI's session format into another's for native resume

Tool-call schemas and session envelopes differ across CLIs; a faithful
cross-format translation is large, brittle, and drifts with every CLI release.
Codex additionally warns when resuming a session created by a different model.
A read-a-handoff-and-continue model is lower-fidelity but robust and provider-
agnostic.

### Put extraction in the legacy `agent_threads` provider trait

That trait is the client-side fallback path. Building transcript parsing there
would run parsing on the wrong host for remote projects and duplicate logic that
belongs in the host-owned `crates/agent_history`.

### Head-plus-tail byte windows over raw records

Raw byte or record windows are exhausted by Codex duplicate encodings, reasoning
and token-count records, and Claude metadata, and can split a tool call from its
result or preserve an abandoned Pi branch. Turn-aware selection with call/result
pairing is the correct unit.

### Include the raw transcript path so the target can read more

This defeats the bounds and the redaction: the target could read unbounded,
unfiltered, potentially secret-bearing source content directly. The full path
stays in Flint's internal metadata only.

### Disable handoff for fresh Codex threads

Discovering the rollout id from post-launch history recovers most fresh-Codex
handoffs safely. Disabling would remove a large part of the motivating scenario.
