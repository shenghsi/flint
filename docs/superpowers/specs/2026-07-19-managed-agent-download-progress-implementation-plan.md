# Managed Agent Download Progress Implementation Plan

## Goal

Make `New — Flint-managed Codex` confirm uncached official CLI downloads,
display byte progress, and coalesce repeated launch gestures into one verified
download, one remote installation, and one Agent Thread.

## Constraints

- Keep managed provisioning explicit; do not change ordinary Agent Thread
  launch behavior.
- Preserve official-source, size, digest, transactional-install, rollback, and
  absolute-path guarantees.
- Reserve single-flight state before the first asynchronous cache check.
- Do not represent unknown total length as a percentage.
- Use GPUI executor timers in tests if time must be advanced.

## Task 1: Report Verified Artifact Download Progress

Modify `crates/agent_threads/src/artifact_cache.rs`.

1. Add a download-progress value containing downloaded bytes and optional total
   bytes.
2. Add an acquisition observer that reports monotonic progress after bytes are
   written and always reports the exact final count.
3. Add a read-only cache query that distinguishes a verified cached source from
   a required HTTP download without starting a request.
4. Retain the existing digest and atomic-commit path.

Write failing tests first for known-length progress, unknown-length progress,
no HTTP request during the cache query, and exact final totals. Run:

```sh
cargo test -p agent_threads artifact_cache --lib
```

## Task 2: Share Artifact Acquisition Across Launch Actions

Modify `crates/agent_threads/src/artifact_cache.rs`,
`crates/agent_threads/src/managed_agent.rs`, and
`crates/agent_threads/src/store.rs`.

1. Let `CachedAgentArtifactSource` use an application-shared
   `Arc<AgentArtifactCache>`.
2. Lazily create and retain that cache in `AgentThreadStore` using the app's
   HTTP client.
3. Forward cache progress as managed-provisioning events.
4. Keep source-digest acquisition locking inside the shared cache so distinct
   remotes reuse one local transfer.

Write a failing test that starts two acquisitions through separate sources and
asserts one HTTP request and two successful consumers.

## Task 3: Add Single-Flight Managed Provisioning State

Modify `crates/agent_threads/src/store.rs` and, if needed for the focused state
model, add one `managed_agent_progress.rs` logical component.

1. Define a hashable key from remote identity, agent ID, pinned version, and
   platform.
2. Track checking, confirmation, download, verification, upload, installation,
   and launch phases.
3. Reserve synchronously and reject repeated begin requests with the current
   state.
4. Remove state on cancel, success, or failure.
5. Ensure only the owner of a reservation can launch a thread.

Write failing unit tests for duplicate begin, independent remotes, monotonic
phase updates, owner-only completion, and retry after cleanup.

## Task 4: Build the Native Progress Notification

Add the focused GPUI view to `crates/agent_threads/src/managed_agent_progress.rs`
and register the module in `crates/agent_threads/src/agent_threads.rs`.

1. Use `NotificationFrame` and Flint's themed `ProgressBar` for known totals.
2. Show percentage and human-readable downloaded/total bytes.
3. Use a spinner and transferred-byte count when total length is unknown.
4. Replace byte progress with concise verification, upload, and install states.
5. Keep the notification persistent until a terminal outcome.

Write failing state/render-helper tests for percentage calculation, byte
formatting, unknown totals, phase labels, and monotonic state updates.

## Task 5: Wire Confirmation and Repeated-Click Behavior

Modify `crates/agent_threads/src/store.rs` and
`crates/agent_threads/src/panel.rs`.

1. Reserve the operation before spawning asynchronous work.
2. Query the cache. If download is required, prompt with pinned agent and
   version before any request.
3. Cancel without network or remote work when declined.
4. Stream progress events into the shared notification entity.
5. On a repeated click, re-show the existing notification and return without
   adding a download, installation, or thread consumer.
6. After verified installation, launch one absolute-path thread and clear the
   reservation.
7. Replace the progress notification with an actionable error on failure.

Write a GPUI regression test proving cancellation makes no request and repeated
actions during a pending response yield one request and one launch owner. Use
`cx.background_executor().timer(...)` for any tracked delay.

## Task 6: Validate and Package

Run:

```sh
cargo fmt --all -- --check
cargo test -p agent_threads --lib
./script/clippy -p agent_threads
./script/bundle-tmp-app
```

If the known debug bundling tail fails after producing the fresh bundle, retain
the previous `/tmp/Flint-Local.app`, complete the documented safe copy fallback,
and verify the app signature. Manually validate one uncached confirmation,
visible progress, repeated-click coalescing, remote install, and one Codex
thread.
