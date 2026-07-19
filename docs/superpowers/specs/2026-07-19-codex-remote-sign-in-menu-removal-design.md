# Codex Remote Sign-In Menu Removal Design

**Date:** 2026-07-19  
**Status:** Approved for implementation

## Problem

The Agent Threads new-thread menu offers **Sign in to Codex on remote** even
when Flint-managed Codex is available. A Flint-managed Codex launch already
starts the official pinned CLI, and that CLI begins its own authentication flow
when the remote credential is missing.

The separate sign-in action is redundant and can be less reliable: it invokes
the configured ambient `codex` command rather than resolving Flint's managed
installation, so it fails on a remote host where only the Flint-managed binary
exists.

## Behavior

- Remove **Sign in to Codex on remote** from the remote Codex new-thread menu.
- Keep **Check Codex sign-in**.
- Keep **Sign out Codex on remote…**.
- Keep **Revoke Codex credential at provider…**.
- Keep **Sign in to Claude on remote** unchanged.
- Do not change credential commands, managed-agent provisioning, routing, or
  authentication storage.

Users who need to authenticate Codex select **New — Flint-managed Codex**. If
the remote credential is absent, the official CLI presents its normal sign-in
flow in the new Agent Thread.

## Implementation

Add a small menu policy in `crates/agent_threads/src/panel.rs` that offers the
standalone remote sign-in entry for agent kinds other than Codex. Apply that
policy only around the existing sign-in entry; the other credential-management
entries remain inside the existing remote-project block.

## Testing

A focused unit test will prove that:

- Codex does not offer the standalone remote sign-in entry; and
- Claude still offers it.

The full `agent_threads` suite, formatting check, and Flint clippy check run
before delivery.

## Acceptance Criteria

- A remote Codex menu contains no **Sign in to Codex on remote** entry.
- The other three Codex credential-management entries remain available.
- A remote Claude menu still contains **Sign in to Claude on remote**.
- Launching Flint-managed Codex remains unchanged.
