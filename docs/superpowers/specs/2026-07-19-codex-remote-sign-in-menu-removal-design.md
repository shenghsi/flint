# Codex Remote Credential Menu Simplification Design

**Date:** 2026-07-19  
**Status:** Approved for implementation

## Problem

The Agent Threads new-thread menu offers **Sign in to Codex on remote** and
**Revoke Codex credential at provider…** even when Flint-managed Codex is
available. A Flint-managed Codex launch already starts the official pinned CLI,
and that CLI begins its own authentication flow when the remote credential is
missing.

The separate sign-in action is redundant and can be less reliable: it invokes
the configured ambient `codex` command rather than resolving Flint's managed
installation, so it fails on a remote host where only the Flint-managed binary
exists.

The provider action opens an API-key management page. It does not revoke a
ChatGPT/OAuth credential stored by Codex on the remote host, so presenting it
as a Codex credential action is misleading.

## Behavior

- Remove **Sign in to Codex on remote** from the remote Codex new-thread menu.
- Remove **Revoke Codex credential at provider…** from that menu.
- Keep **Check Codex sign-in**.
- Keep **Sign out Codex on remote…**.
- Keep all Claude remote credential actions unchanged.
- Do not change credential commands, managed-agent provisioning, routing, or
  authentication storage.

Users who need to authenticate Codex select **New — Flint-managed Codex**. If
the remote credential is absent, the official CLI presents its normal sign-in
flow in the new Agent Thread.

## Implementation

Add a small menu policy in `crates/agent_threads/src/panel.rs` that offers the
standalone remote sign-in and provider-management entries for agent kinds other
than Codex. Apply that policy only around those two entries; sign-in status and
remote sign-out remain inside the existing remote-project block.

## Testing

A focused unit test will prove that:

- Codex offers neither the standalone remote sign-in entry nor the provider
  management entry;
- Codex still offers sign-in status and remote sign-out; and
- Claude still offers all existing credential actions.

The full `agent_threads` suite, formatting check, and Flint clippy check run
before delivery.

## Acceptance Criteria

- A remote Codex menu contains no **Sign in to Codex on remote** entry.
- It contains no **Revoke Codex credential at provider…** entry.
- **Check Codex sign-in** and **Sign out Codex on remote…** remain available.
- A remote Claude menu retains all existing credential actions.
- Launching Flint-managed Codex remains unchanged.
