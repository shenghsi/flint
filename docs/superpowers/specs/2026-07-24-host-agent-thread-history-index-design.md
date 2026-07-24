# Host-Owned Agent Thread History Index Design

**Date:** 2026-07-24
**Status:** Draft for written-spec review

No implementation is authorized by this design review alone.

Design owner: Codex.

## Problem

Agent Threads history preparation is client-driven. For a remote project, the
client traverses the agent's history directory and proxies each filesystem
operation to `flint-remote-server`.

Codex illustrates the cost:

1. The client recursively lists `sessions/YYYY/MM/DD/`.
2. It sorts every discovered rollout path and keeps the 200 newest files.
3. It requests metadata for each selected file.
4. On a cold cache miss, it also downloads each selected file.
5. It separately reads Codex's title indexes.

The 200-file limit bounds file parsing, but directory discovery still visits
every year, month, and day. A warm client cache avoids downloading unchanged
rollouts, but it still requests metadata for each selected file. A fresh client
has no cache from another device.

The resulting remote cost is proportional to the number of date directories
plus the number of selected session files. Each operation carries network
latency even though the source files and `flint-remote-server` are on the same
host.

Moving the current per-file parse cache to the remote host would not solve the
problem. The client would still discover and validate files one RPC at a time.

## Goals

- Perform history discovery, metadata validation, and parsing on the host that
  owns the agent history.
- Render a valid persisted thread snapshot without waiting for a refresh.
- Reduce a warm remote history load to one request stream and no per-session
  filesystem RPCs.
- Share one host cache between local Flint and remote Flint clients running as
  the same host user.
- Preserve the existing history, filtering, ordering, title, resume-directory,
  and scan-limit semantics for Codex, Claude, and Pi.
- Honor agent history directory overrides such as `CODEX_HOME`,
  `CLAUDE_CONFIG_DIR`, and `PI_CODING_AGENT_DIR`.
- Refresh lazily and incrementally without requiring an always-running watcher.
- Preserve current behavior when connected to an older remote server.
- Keep history retrieval independent of the Direct or Tunneled agent process
  route.

## Non-goals

- Keeping indexes continuously warm with filesystem watchers.
- Adding a cross-project history user interface.
- Removing the legacy client-side scanner in this change.
- Changing agent history formats or asking providers to write Flint metadata.
- Changing session resume commands, session identifiers, or title selection.
- Changing the Direct or Tunneled launch policy.
- Indexing beyond the existing per-provider history limits.
- Providing a general remote filesystem indexing service.

## Accepted Approach

Introduce a shared, host-local history index service. The local Flint
application calls it directly for local projects. `flint-remote-server` exposes
the same service through a capability-gated streaming RPC for remote projects.

The service persists normalized session summaries and the file identities
needed for incremental refresh. It never persists raw session transcripts.

Each request behaves as follows:

1. Resolve the agent kind, history root, and project roots.
2. Load and validate the persisted index.
3. If a valid index exists, immediately emit a filtered `Cached` snapshot.
4. Start or join one host-local refresh for that agent kind and history root.
5. Discover source files using host-local filesystem operations.
6. Reuse summaries whose source identity is unchanged.
7. Parse new or changed files and remove deleted files.
8. Atomically persist the refreshed index.
9. Emit a filtered `Fresh` snapshot and end the stream.

If no valid index exists, the service does not emit an empty cached snapshot.
It performs the host-local refresh and emits one `Fresh` snapshot when ready.

This is stale-while-revalidate behavior without a time-to-live. Every request
validates the index in the background, while a valid persisted snapshot keeps
the panel responsive.

## Alternatives Considered

### Keep the scanner on the client and add batch filesystem RPCs

The client could request recursive directory listings, metadata batches, or
multiple files in fewer RPCs.

This would reduce round trips, but it would expose a broad remote filesystem
API, continue transferring provider source data, and leave parsing and cache
ownership on each client. A new device would still download and parse the same
history.

### Scan on the host without persistence

A dedicated RPC could run the current scanner on `flint-remote-server` and
return the resulting thread list.

This removes network chatter and is simpler than a persisted index, but every
panel activation and every newly connected client would rediscover and reparse
history. It does not provide an immediate warm result or share completed work
across processes.

### Move the existing per-file cache to the host

The remote host could store the current `PersistedCacheFile` while leaving
directory discovery and staleness checks on the client.

This does not change the client-driven request topology. The client would still
issue one remote directory request per tree level and one metadata request per
selected file.

### Store one index per project

Each project could have a separate index containing only its sessions.

This makes current filtering straightforward, but duplicates sessions that
span roots, complicates configuration-root changes, and requires multiple
files for a multi-root workspace. A host-wide logical index with query-time
filtering is simpler and supports a future cross-project view without changing
the persisted model.

### Keep the index always warm with watchers

Local and remote processes could watch every agent history root and refresh on
filesystem changes.

This adds lifecycle, recursive-watch, reconnect, and platform-specific
behavior. The remote server is not guaranteed to run when an agent writes
history. Lazy refresh provides the required performance and correctness first.
Watchers can later trigger the same refresh service without changing its data
model.

## Architecture

### Shared `agent_history` crate

Create a small `agent_history` crate with a descriptive library root instead of
placing indexing code in `agent_threads` or `remote_server`.

It owns:

- supported history kinds;
- the filesystem abstraction used by both host-local and legacy remote scans;
- provider-specific source discovery and parsing;
- normalized indexed-session records;
- existing scan-limit and title-selection semantics;
- persisted index schema and validation;
- incremental refresh;
- project-root filtering;
- in-process single-flight refresh coordination;
- cross-process cache coordination; and
- atomic, private cache persistence.

It depends on filesystem, serialization, time, and collection primitives. It
does not depend on GPUI views, projects, workspaces, RPC clients, terminal
launching, agent provisioning, or route settings.

This keeps `remote_server` from depending on the UI-oriented `agent_threads`
crate and gives local and remote history retrieval one implementation.

### `agent_threads`

The existing `agent_threads` crate owns application integration:

- resolving the history root from the project environment;
- collecting visible project worktree roots;
- selecting local service or remote RPC access;
- converting snapshots into `HistoricalThread` values;
- applying streamed cached and fresh snapshots to panel state;
- choosing the legacy scanner when the server lacks the capability;
- retaining the legacy scanner as a failure fallback; and
- building resume commands from the selected historical thread.

The history provider registry remains the source of agent-facing resume
behavior. Provider parsing moves into the shared crate and is reused by the
host index and the legacy `RemoteHistoryFs` adapter. The current client-local
per-file cache remains available only to that legacy path until the fallback is
removed.

### `remote_server`

`flint-remote-server` owns the remote adapter:

- advertising the host index capability;
- validating and serving history stream requests;
- passing its local filesystem and cache root to `agent_history`;
- streaming cached and fresh snapshots; and
- cancelling request delivery when the client disconnects without cancelling a
  refresh still needed by another requester.

The remote handler uses local filesystem operations. It does not call the
existing `ListRemoteDirectory`, `GetPathMetadata`, or `ReadRemoteFile` handlers.

### `proto`

The protocol owns only the wire contract. The persisted JSON schema remains an
implementation detail of `agent_history`; clients never download `index.json`
directly.

## Cache Identity and Location

The cache is global to one host user, but it is partitioned by:

- agent kind; and
- normalized resolved history root.

`GetAgentThreadHistory { kind }` alone is incorrect because two projects can
resolve different values for `CODEX_HOME` or another provider override.

Use a dedicated host-wide root:

```text
~/.flint/cache/agent_threads/<kind>/<history-root-key>/index.json
```

`history-root-key` is a stable hash of the host path style and normalized
absolute history root. The index also stores the unhashed normalized root and
rejects a file whose stored identity does not match the request.

This path intentionally does not derive from `paths::data_dir()`.
`flint-remote-server` stores its application data under `~/.flint/remote`,
while a local Flint process uses its platform application-data directory. A
dedicated home-relative cache allows both processes to share the same index.

Cache directories and files are private to the host user. On POSIX hosts,
directories use mode `0700` and files use mode `0600`. Windows uses the
current user's access controls.

Titles and project paths are potentially sensitive. They do not leave the host
except through the authenticated remote project connection that requested
them.

## Persisted Schema

The index uses a versioned JSON envelope. A schema or parser version mismatch
is a cache miss; there is no migration.

Conceptually, the envelope contains:

```text
schema_version
parser_version
agent_kind
normalized_history_root
generation
generated_at
provider_source_state
indexed_sessions
```

`provider_source_state` contains relative source paths, modified time, length,
and the parsed summary needed to avoid loading unchanged files. It also tracks
provider-level title sources such as Codex's `session_index.jsonl` and
`history.jsonl`.

`indexed_sessions` contains normalized records with:

- session ID;
- resolved title and provider fallback title;
- one or more working directories;
- last activity time; and
- the provider source identity needed to rebuild deterministic results.

The persisted format retains multiple working directories. Claude sessions can
change directories, and the current scanner matches any recorded working
directory before choosing the requested project root for resume.

The public snapshot remains a compact projection:

```text
session_id
title
project_root
last_activity_at
```

The service selects the matching requested project root when producing that
projection. It may emit the same session for different roots in separate
queries without duplicating the persisted session record.

## Provider Semantics

The index must preserve current provider behavior.

### Codex

- Traverse the `sessions/YYYY/MM/DD` hierarchy in descending path order.
- Select at most the newest 200 rollout files globally before project
  filtering.
- Use `session_index.jsonl` titles first, then `history.jsonl`, then the first
  valid user message, then `Codex session`.
- Preserve the session metadata timestamp as `last_activity_at`.
- Rebuild titles from stored rollout summaries when a title source changes;
  unchanged rollout files are not reloaded.

Descending traversal may stop after the 200 newest rollout files have been
identified. This produces the same selected set as sorting every discovered
path and truncating it.

### Claude

- Merge the global `history.jsonl` source with project history files.
- Select at most the newest 200 project-history files per project directory.
- Preserve every working directory recorded in a project history file.
- Keep the newest record for a session according to current merge behavior.
- Select the requested matching project root as the resume directory.

### Pi

- Select at most the newest 200 session files per encoded project directory.
- Preserve the session ID, recorded project root, title, and maximum activity
  timestamp.
- Keep the newest record when multiple files describe the same session.

## Incremental Refresh

Incremental refresh means unchanged session files are not reloaded or reparsed.
It does not require constant-time discovery.

The service performs these steps under one refresh operation:

1. Enumerate the provider's candidate directories locally.
2. Apply the provider's ordering and file limits.
3. Read local metadata for selected files.
4. Reuse a persisted summary when relative path, modified time, and length
   match.
5. Load and parse new or changed files.
6. Drop records for files no longer in the selected set.
7. Recompute provider title and merge rules.
8. Compare the materialized index with the previous generation.
9. Persist a new generation atomically when content or source identities
   changed.
10. Avoid rewriting the file when the source state is unchanged.

A missing history root is a valid empty source. A permission error, interrupted
directory traversal, or other I/O failure is not a valid empty source and must
not overwrite a usable index.

An unchanged malformed session file remains a recorded negative parse result so
that later refreshes do not repeatedly load it. A changed malformed file is
retried.

## Concurrency and Persistence

All requests for the same `(agent kind, history root)` within one process join
one refresh task. Each requester receives the result through its own local
subscription or RPC stream.

Local Flint and `flint-remote-server` can run concurrently for the same host
user. They coordinate refresh and persistence with an adjacent host file lock.
Cached reads do not wait for the lock. A refresher:

1. acquires the lock;
2. reloads the latest valid index written by another process;
3. refreshes from that state;
4. writes a temporary file in the same directory;
5. flushes and atomically replaces `index.json`; and
6. releases the lock.

If another process holds the lock, a requester continues displaying its cached
snapshot and joins or waits for the serialized refresh. Cancellation of one
requester does not delete the persisted cache or cancel work observed by
another requester.

The lock and temporary files contain no session contents. Stale temporary files
from a crash are ignored and cleaned during a later successful write.

## Remote Protocol

### Capability negotiation

Extend the already-known `RemoteStarted` message with a repeated capability
field. A supporting server advertises:

```text
agent-thread-history-index-v1
```

Adding a field to an existing protobuf message is backward compatible. An old
server sends no capability, so a new client never sends it an unknown history
request. This avoids relying on an unknown request to produce an error or
timeout.

Capabilities are scoped to the current remote connection and cleared on
disconnect.

### Stream request

Add a stream request equivalent to:

```text
StreamAgentThreadHistory {
    kind
    normalized_history_root
    project_roots[]
}
```

The server validates:

- `kind` names a supported built-in history provider;
- the history root is absolute and uses the remote host's path style;
- project roots are absolute;
- collection sizes and path lengths are bounded; and
- the request belongs to the authenticated remote project connection.

An empty `project_roots` list means no project filter. Current Agent Threads
requests always provide the visible worktree roots; the empty form preserves
the global index model for a future cross-project consumer.

### Stream response

Each response contains:

```text
AgentThreadHistorySnapshot {
    freshness: Cached | Fresh
    generation
    entries[]
}
```

Each entry carries the four public projection fields. Paths are encoded as
remote-host strings and converted using the project's remote path style.
Activity time uses the existing protobuf timestamp representation.

The stream emits:

- `Cached`, then `Fresh`, when a valid index existed;
- only `Fresh` after a successful cold build; or
- an error when no snapshot can be produced.

The server sends a final `Fresh` response even when the generation did not
change. This gives the client an unambiguous refresh completion and closes the
stream without a separate status message.

If refresh fails after `Cached`, the stream ends with that error and does not
emit `Fresh`. The client retains the already delivered snapshot.

## Client and Panel Behavior

For a local project, the panel subscribes directly to the shared history
service.

For a remote project:

1. If the connection advertises `agent-thread-history-index-v1`, use the stream
   request.
2. Apply the first valid snapshot immediately.
3. Replace it with the `Fresh` snapshot when the stream completes.
4. If the server does not advertise the capability, use the legacy scanner.
5. If the indexed path fails before producing any snapshot, log the indexed
   failure and try the legacy scanner once.

When a cached snapshot has already been rendered and its refresh fails, retain
the cached snapshot. Log the refresh failure and allow the next normal refresh
trigger to retry. Do not replace usable history with `Unavailable`.

When neither the indexed path nor the fallback scanner can produce a snapshot,
preserve the current `Unavailable` state and log the combined error.

A cold index keeps the existing loading state until the host-local build or
fallback completes. An empty valid snapshot renders as an empty history list,
not as loading or unavailable.

The current local filesystem watcher can continue to trigger refresh requests.
Remote thread closure continues to trigger its explicit delayed refresh. Both
paths use the same index service and therefore receive cached-then-fresh
behavior.

## Route Boundary

History location and retrieval depend on whether the project is local or
remote, not on the agent process route.

Both Direct and Tunneled remote projects use the same remote project connection
and `flint-remote-server` history capability. The index service:

- does not resolve or install managed agent executables;
- does not acquire an egress lease;
- does not read credential or plan-usage capabilities; and
- does not change resume command construction.

Tests must exercise history retrieval under both route settings and assert
identical snapshots and RPC selection.

## Error Handling

- Missing index: build locally on the host; do not invoke the legacy remote
  scanner merely because the cache is cold.
- Unsupported server capability: use the legacy scanner without sending the new
  request.
- Invalid schema or parser version: treat as a cache miss and rebuild.
- Corrupt index JSON: ignore it and atomically replace it only after a
  successful refresh; do not surface partial entries or overwrite it with a
  failed refresh.
- Missing history directory: persist and return a valid empty index.
- Source permission or traversal failure: retain any valid cached snapshot and
  report refresh failure.
- Malformed individual session: skip that session, retain a negative parse
  identity, and continue.
- Atomic-write failure: return the freshly computed in-memory snapshot to the
  current requester, report the persistence failure, and retry persistence on a
  later refresh.
- Stream disconnect: stop delivering to that client; preserve shared refresh
  work when another requester still observes it.
- Indexed RPC failure before a snapshot: try the legacy scanner once.
- Indexed refresh failure after a cached snapshot: keep cached data and do not
  run a second expensive scan automatically.

## Testing

### Shared index unit tests

- A cold source creates a versioned index and a `Fresh` snapshot.
- A warm source emits `Cached` before refresh completion.
- Unchanged files issue local metadata calls but no loads or parses.
- New and changed files are parsed and appear in the next generation.
- Deleted files disappear after refresh.
- An unchanged malformed file is not loaded repeatedly.
- A corrupt or version-mismatched index is rebuilt without migration.
- Different normalized history roots use different cache files.
- A stored history-root mismatch invalidates a hash-keyed cache file.
- Codex preserves the global 200-file-before-filtering limit and title
  precedence.
- Claude preserves multiple working directories and current merge behavior.
- Pi preserves its per-project-directory 200-file limit.
- A top-level traversal error does not replace a valid index with an empty one.
- Concurrent in-process requests perform one refresh.
- Two service instances sharing a cache directory serialize writers and observe
  the latest complete generation.

### Protocol and remote-server tests

- A supporting server advertises `agent-thread-history-index-v1`.
- An old-style `RemoteStarted` without capabilities selects the legacy path.
- A warm request streams `Cached`, then `Fresh`, then ends.
- A cold request streams only `Fresh`.
- Server-side project-root filtering returns the same entries as the local
  service.
- Claude chooses the matched request root in the public projection.
- A capable remote scan issues no `ListRemoteDirectory`, `GetPathMetadata`, or
  `ReadRemoteFile` RPC per history source file.
- Disconnecting one stream does not cancel a refresh used by another stream.
- Invalid kinds and malformed paths return bounded errors without scanning.

### Agent Threads integration tests

- Local and remote indexed paths produce equivalent `HistoricalThread` values.
- A cached snapshot remains visible while the fresh snapshot is pending.
- A fresh snapshot replaces the cached generation.
- A refresh error after `Cached` retains the cached list.
- A capable-server failure before any snapshot invokes the legacy scanner once.
- An absent capability uses only the legacy scanner.
- A missing server-side index does not invoke the legacy scanner.
- Direct and Tunneled projects select the same history path and results.
- Existing resume commands still use the projected session ID and matched
  project root.

### Performance regression test

Use operation counts rather than wall-clock timing:

- create more than 200 Codex rollouts across many date directories;
- serve them through the remote integration harness;
- warm the host index;
- request the list from a fresh client; and
- assert one history stream request and zero per-file remote filesystem
  requests.

This test captures the architectural improvement without depending on CI
network timing.

## Delivery

Implement this as one vertical pull request with reviewable commits:

1. Add the shared index model, persistence, provider scanners, and unit tests.
2. Add protocol capability negotiation, streaming messages, the remote-server
   adapter, and integration tests.
3. Switch local and capable-remote Agent Threads history to the shared service,
   retain the legacy fallback, and add panel tests.
4. Remove only superseded internal cache code that has no fallback caller.

The pull request must preserve the legacy path for servers without the
capability. Removing that path requires separate compatibility evidence and is
outside this design.

## Acceptance Criteria

- A fresh client connected to a host with a valid index renders history from
  one request stream without per-session filesystem RPCs.
- A host with no index performs one host-local build and returns correct
  history without client-side remote file traversal.
- Local Flint and `flint-remote-server` share the same cache for the same host
  user, agent kind, and history root.
- Cache refresh preserves current Codex, Claude, and Pi results and scan limits.
- History-directory overrides remain isolated and correct.
- Cached history remains usable during refresh and after a refresh-only
  failure.
- Older servers continue to use the existing scanner without receiving an
  unknown request.
- Direct and Tunneled routing produce identical history retrieval behavior.
- The performance regression test proves zero per-file remote filesystem
  requests on the indexed path.

## Review (Claude)

Reviewer: Claude. Verdict: approve the direction, with one change requested
before implementation (defer the cross-process file lock; see concern 1).

### Verified against the codebase

- Server-streaming RPC already exists (`crates/rpc/src/peer.rs`:
  `stream_response_channels`, `terminal_stream_response`; `git.proto` uses
  streaming today). The `Cached`-then-`Fresh` stream rides existing infra, not
  new plumbing.
- `RemoteStarted` is real and currently `message RemoteStarted {}`
  (`crates/proto/proto/flint.proto`, `RemoteStarted -> Ack` in
  `crates/proto/src/proto.rs`). Adding a `repeated` capability field is
  proto3-backward-compatible as claimed.
- No file-locking primitive exists anywhere in the repo (only in-memory
  `Mutex::lock`). The cross-process lock would be a net-new dependency or
  bespoke per-platform code. This informs concern 1.

### Strengths

- The problem diagnosis matches the code precisely (per-level `read_dir` plus
  per-file `metadata` even when warm; per-device cache).
- Keying the cache by `(kind, normalized history root)` rather than `kind`
  alone is the key correctness win: `CODEX_HOME` / `CLAUDE_CONFIG_DIR` /
  `PI_CODING_AGENT_DIR` can resolve differently per project, so a `kind`-only
  key would cross-contaminate projects. The hash-key plus stored unhashed root
  with mismatch rejection is the right defensive shape.
- The `agent_history` crate is the correct dependency boundary: it keeps
  `remote_server` off the GPUI/workspace-heavy `agent_threads` and gives the
  host index and the legacy `RemoteHistoryFs` path one scanner.
- Correct invariants: missing root = valid empty but I/O error != empty (never
  overwrite a good index); negative-parse caching; corrupt-index rebuild;
  atomic temp+rename. Backward-compat, route boundary, and legacy fallback are
  all handled, and the operation-count performance test is the right shape.

### Concerns (ranked)

1. Defer the cross-process file lock. It is the only element with zero
   precedent in the repo, so it means a new dependency plus lifecycle, and
   advisory file locks are unreliable on networked home directories (NFS/SMB)
   which is exactly where one machine is both a local workstation and a remote
   host. Recommendation for v1: rely on atomic-rename plus "reload the latest
   valid index before refreshing." Atomic rename already prevents readers from
   seeing partial files, so the worst case without the lock is redundant
   refresh work, not corruption. The lock only de-duplicates concurrent
   refreshes; add it later if measurement shows it matters. If it stays, name
   the mechanism and specify networked-home fallback behavior.
2. Pin down the `flint-remote-server` process/lifetime model the coordination
   assumes. In-process single-flight only spans one server-process lifetime.
   State whether one server process is shared across all client connections to
   a host (single-flight has cross-connection reach) or whether cross-connection
   sharing falls entirely to the persisted file.
3. State the trust model for the client-supplied `normalized_history_root`. The
   server validates absolute/path-style but cannot cheaply confirm the root is
   the agent's real configured home. That is acceptable under Flint's
   single-user model (the user reads their own files); say so explicitly rather
   than leave it implicit.

### Smaller notes (non-blocking)

- Preserving the 200-file limit is the safe parity choice, but note the
  follow-up the index unlocks: "200 newest globally before project filtering"
  can starve a project whose sessions are older than 200 other-project
  sessions, and a persisted index makes raising the limit nearly free.
- A `parser_version` bump is a fleet-wide cold-rebuild event (every
  host/kind/root re-scans once). Acceptable; worth stating as a conscious cost.
- Local `~/.flint/cache` on macOS deliberately departs from
  `~/Library/Application Support`. It is necessary for host-sharing and
  consistent with `~/.flint/remote`; worth one explicit line acknowledging the
  deviation.
- Delivery staging is sound, but commit 1 (shared crate + scanners + unit
  tests) is self-contained enough to be its own PR if a smaller review surface
  is wanted. Optional.
