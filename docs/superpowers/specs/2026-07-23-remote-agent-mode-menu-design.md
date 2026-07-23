# Remote Agent Mode Menu Design

**Date:** 2026-07-23
**Status:** Approved for implementation

## Problem

The Agent Threads new-thread dropdown exposes explicit
**New — Flint-managed** entries and remote sign-out actions while an SSH
connection uses Direct agent routing. Direct mode is intended to use only the
remote server's ambient agent commands. Flint-managed agents are reserved for
Tunneled mode.

Pi also supports the same `agent_threads.<kind>.hidden` setting and panel
filtering as Codex and Claude, but the Settings UI does not expose the
corresponding **Hide Pi** toggle.

## Behavior

For a remote SSH project using Direct agent routing:

- new and resumed threads use only the configured ambient commands on the
  remote server;
- the dropdown contains no explicit Flint-managed launch entry; and
- the dropdown contains no Flint-managed credential action, including remote
  sign-out.

For a remote SSH project using Tunneled agent routing:

- every ordinary new-thread and resume entry point resolves the pinned
  Flint-managed agent binary on the remote server;
- the managed agent's network traffic continues through the tunnel provided by
  local Flint;
- Codex and Claude retain their remote sign-out actions, which resolve the same
  managed binary and require the Tunneled route; and
- the dropdown contains no separate Flint-managed launch entry because all
  ordinary launch entries are already managed.

Local and route-less workspaces do not expose remote credential actions or
managed remote launch entries.

The Settings UI presents **Hide Codex**, **Hide Claude**, and **Hide Pi**
together. **Hide Pi** reads and writes `agent_threads.pi.hidden` and uses the
same user-settings scope as the existing controls.

## Implementation

Remove the obsolete explicit Flint-managed dropdown row and its panel-only
label/status helpers. Keep managed provisioning behind the store's
route-authoritative ordinary launch path.

Gate the remote credential menu on both a remote workspace and Tunneled
routing. Continue checking whether the selected agent defines a credential
policy, so Pi does not acquire unsupported sign-out behavior.

Add the Pi visibility setting to the existing Agent Threads settings section
beside the Codex and Claude controls. Do not change the underlying setting
schema or panel filtering because both already support Pi.

The launch and transport implementation remains authoritative:

- Direct selects configured ambient commands and never enters managed
  provisioning.
- Tunneled selects managed provisioning, requires the selected route to remain
  Tunneled until launch, and applies Flint's existing remote egress tunnel.

## Error Handling

This change adds no fallback between modes. Managed preparation or tunnel
failure in Tunneled mode continues to surface through the existing workspace
and progress notifications. It must never fall back to an ambient executable
or direct remote internet access. Direct mode does not attempt managed
preparation.

## Testing

Tests will verify that:

- remote credential actions are visible only for Tunneled workspaces;
- Direct, local, and route-less menus expose no explicit managed launch row;
- Direct new-thread routing selects configured ambient commands;
- Tunneled new-thread, resume, and credential routing selects managed
  provisioning and retains the Tunneled route requirement;
- Pi has no credential action because it defines no credential policy; and
- the Settings UI exposes all three hide controls with the exact Codex,
  Claude, and Pi JSON paths.

Run the focused `agent_threads` and `settings_ui` tests, their relevant crate
test suites, `cargo fmt --all -- --check`, and `./script/clippy` for the
affected crates before delivery.

## Acceptance Criteria

- Direct remote menus show only ambient-agent launch and resume choices.
- Direct launches never provision or run Flint-managed agents.
- Tunneled launches and resumes use Flint-managed agents on the remote host and
  route their traffic through local Flint.
- Tunneled Codex and Claude menus retain remote sign-out without a separate
  managed launch row. The remote sign-out dropdown option uses the same small
  font as the other options.
- The Settings UI shows a working **Hide Pi** toggle beside the existing
  Codex and Claude toggles.
