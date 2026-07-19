# Codex Remote Credential Menu and Sign-Out Design

**Date:** 2026-07-19  
**Status:** Approved for implementation

## Problem

The Agent Threads new-thread menu offers separate sign-in, sign-in status,
sign-out, and provider-management actions for Codex. A Flint-managed Codex
launch already starts the official pinned CLI, and that CLI begins its own
authentication flow when the remote credential is missing.

The separate sign-in and status actions are redundant. The status action also
cannot prove that the stored credential is accepted or that Through-Flint
networking works; a real Codex request is the definitive check. Both actions
currently invoke the configured ambient `codex` command rather than resolving
Flint's managed installation, so they fail on a remote host where only the
Flint-managed binary exists.

The provider action opens an API-key management page. It does not revoke a
ChatGPT/OAuth credential stored by Codex on the remote host, so presenting it
as a Codex credential action is misleading.

## Behavior

- Remove **Sign in to Codex on remote** from the remote Codex new-thread menu.
- Remove **Check Codex sign-in** from that menu.
- Remove **Revoke Codex credential at provider…** from that menu.
- Keep **Sign out Codex on remote…**.
- When the project route is **Through Flint**, sign-out must use the pinned
  Flint-managed Codex executable and retain the Through-Flint route guarantee.
- When the project route is **Not through Flint**, sign-out continues to use
  the configured Codex command.
- Keep all Claude remote credential actions unchanged.
- Do not change authentication storage.

Users who need to authenticate Codex select **New — Flint-managed Codex**. If
the remote credential is absent, the official CLI presents its normal sign-in
flow in the new Agent Thread.

## Implementation

Add a small menu policy in `crates/agent_threads/src/panel.rs` that offers the
standalone remote sign-in, sign-in status, and provider-management entries for
agent kinds other than Codex. Remote sign-out remains available for every agent
kind.

Extend the credential-command launch path in `crates/agent_threads/src/store.rs`
so Codex sign-out follows the selected remote-agent route. Under Through Flint,
reuse the existing managed-agent preparation path to find or provision the
pinned official CLI, build the logout command with that executable, apply the
self-update suppression policy, and launch it with Through Flint as the
required route. Existing managed-agent progress, confirmation, and duplicate
provisioning behavior remain in effect if the executable is not ready. Under
Not through Flint, preserve the current configured-command behavior. Claude
credential actions remain unchanged.

If the route changes while managed Codex is being prepared, abort sign-out with
the existing route-change error instead of launching under a different route.

## Testing

A focused unit test will prove that:

- Codex offers none of the standalone remote sign-in, sign-in status, or
  provider-management entries;
- Codex still offers remote sign-out;
- Claude still offers all existing credential actions.
- Through-Flint Codex sign-out replaces only the executable with the managed
  path, retains the logout arguments and configured environment, applies
  self-update suppression, and requires the Through-Flint route.
- Not-through-Flint Codex sign-out uses the configured command without managed
  provisioning.

The full `agent_threads` suite, formatting check, and Flint clippy check run
before delivery.

## Acceptance Criteria

- A remote Codex menu contains no **Sign in to Codex on remote** entry.
- It contains no **Check Codex sign-in** entry.
- It contains no **Revoke Codex credential at provider…** entry.
- **Sign out Codex on remote…** remains available.
- Through-Flint Codex sign-out runs the pinned managed executable through
  Flint, even if the remote host has its own Codex installation or internet
  access.
- Not-through-Flint Codex sign-out uses the configured command.
- A remote Claude menu retains all existing credential actions.
- Launching Flint-managed Codex remains unchanged.
