# Show and Deduplicate Managed Agent Downloads

## Status

Approved in conversation on 2026-07-19.

Design owner: Codex.

## Problem

Selecting `New — Flint-managed Codex` immediately begins acquiring the pinned
official Codex artifact but presents no confirmation or visible progress. Each
selection constructs a separate artifact cache whose acquisition lock is local
to that instance. Repeated clicks therefore start concurrent downloads of the
same source digest and can proceed into concurrent remote installation work.

The action appears inert while consuming bandwidth and disk space. It also
allows several user gestures to race toward several Agent Threads even though
the user intended one launch.

## User Experience

When the pinned executable is already present and verified in Flint's local
artifact cache, Flint skips download confirmation and proceeds with the
single-flight remote installation.

When a local download is required, Flint asks:

> Flint needs to download the official Codex CLI v0.144.6 locally and upload it
> to this remote host.

The actions are `Download and launch` and `Cancel`. Cancellation performs no
HTTP request and creates no Agent Thread.

After confirmation, a persistent notification displays:

- `Downloading official Codex CLI v0.144.6`;
- a determinate progress bar when the response supplies `Content-Length`;
- the percentage and transferred bytes, for example
  `37% · 18.4 MB / 49.7 MB`; and
- an indeterminate spinner plus transferred bytes when the total length is not
  available. Flint does not display a fabricated percentage.

Progress is monotonic and updates are throttled so reading each network chunk
does not cause an unnecessary render. After the download completes, the same
notification reports `Verifying Codex CLI`, `Uploading to remote`, and
`Installing Codex CLI` without retaining a misleading byte progress bar.

While work is active, the context-menu entry identifies the current operation,
including the percentage when known. Selecting it re-shows the same persistent
notification with its latest state. It does not start another download,
installation, or thread. A successful operation dismisses the progress
notification and launches exactly one Agent Thread. A failure replaces it with
an actionable error and allows a later retry.

## Coordination

`AgentThreadStore` owns a managed-provisioning coordinator shared across
workspaces. It keys active work by:

- remote connection identity;
- agent ID;
- pinned version; and
- remote platform target.

The coordinator reserves the key before asynchronously checking the cache or
showing confirmation. This closes the interval in which two rapid clicks could
both decide that no work is active. The reservation records these phases:

1. checking the local cache;
2. awaiting download confirmation;
3. downloading;
4. verifying the local artifact;
5. uploading;
6. installing and verifying the remote executable; and
7. launching.

A second request observes the active state and returns without registering a
second completion consumer. Only the request that created the reservation may
launch a thread.

The coordinator removes the reservation after cancellation, success, failure,
or loss of the associated connection or workspace. Cleanup does not delete a
previous verified cache entry or managed installation. A retry begins from the
last committed valid state.

## Artifact Cache

Artifact acquisition is shared at application scope rather than reconstructed
with an independent mutex for every action. Source-digest coordination remains
separate from remote provisioning coordination: two different remotes may share
one local download, while each remote still performs its own verified upload
and installation.

The download path reports byte progress after successfully writing each chunk.
The report contains downloaded bytes and an optional total derived from the
validated `Content-Length`. The existing one-gibibyte maximum, official redirect
policy, source digest verification, executable normalization, executable digest
verification, partial-file cleanup, and atomic cache commit remain unchanged.

Progress reporting must not allow UI lifetime to control artifact integrity. If
the initiating notification or workspace disappears, its provisioning request
is cancelled. A shared source download is cancelled only after its last consumer
is gone. In every case, the cache never commits a partial or unverified file.

## Remote Installation

The existing transactional installer remains authoritative. It uses the
verified cached executable, uploads to a private staging directory, verifies the
remote digest and version, and commits atomically. The coordinator exposes
coarse upload and installation phases but does not weaken rollback behavior.

The managed executable is launched by its absolute versioned path. This change
does not make `codex` ambient on the remote `PATH`, alter the Through-Flint
network route, or make ordinary `New Codex thread` automatically provision an
agent.

## Error Handling

- Download rejection, redirect-policy failure, size overflow, short or corrupt
  content, and digest failure remove the partial file and show the causal error.
- Upload, remote digest, permission, version, or atomic-commit failure preserves
  the prior valid installation and shows the failed phase.
- A repeated click never cancels, restarts, or adds a launch consumer to active
  work.
- Coordinator state is cleared after terminal outcomes so a later explicit
  click can retry.

## Tests

Add deterministic tests at the cache, coordinator, and GPUI action seams:

- an uncached artifact requires confirmation before the first HTTP request;
- cancelling confirmation performs no HTTP request or remote operation;
- repeated clicks during download make exactly one HTTP request and launch
  exactly one thread;
- two remotes acquiring the same release share one source download while
  retaining independent installations;
- known-length progress is monotonic and reaches the exact total and 100%;
- unknown-length progress reports transferred bytes without a percentage;
- the active menu label and persistent notification reflect the current phase;
- a verified cached executable skips confirmation and download;
- download and installation failures clear active state and permit retry; and
- successful provisioning retains existing digest, version, rollback, and
  absolute-path assertions.

Run focused `agent_threads` tests, formatting, and workspace clippy. Package a
fresh `/tmp/Flint-Local.app` and manually verify one confirmed download, repeated
click behavior, progress updates, remote installation, and one resulting Codex
thread.
