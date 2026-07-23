# Agent Thread Integration Rules Design

**Date:** 2026-07-23
**Status:** Approved for implementation

## Problem

Adding Pi covered the agent registry, command defaults, panel filtering, history,
managed releases, and remote execution, but missed the corresponding
**Hide Pi** Settings Editor control. It also exposed an obsolete explicit
Flint-managed launch row in Direct mode, making a pre-existing routing-policy
mistake affect another agent.

Future Agent Threads integrations need a short cross-crate rule that catches
these parity and routing traps without becoming a file-by-file architecture
map.

## Placement

Add the guidance to the repository-root `.rules`. The invariant spans agent
registration, settings content, the Settings Editor, panel behavior, and remote
routing, so no single crate-level rule has the correct scope.

## Rule

Add an **Adding Agent Threads coding agents** section with these requirements:

- Audit a new agent end-to-end against existing agents: settings and defaults,
  Settings Editor controls, panel visibility, actions, history and resume, and
  remote behavior. Represent every intentional omission as an explicit,
  tested capability.
- Do not assume that adding a settings schema or default makes the setting
  user-visible. Add and test every applicable per-agent Settings Editor
  control and its exact JSON path, especially `hidden`.
- Preserve the remote route boundary. Direct uses only the configured ambient
  executable on the remote and exposes no Flint-managed launch or credential
  controls. Tunneled uses only the pinned Flint-managed executable on the
  remote and routes its traffic through local Flint. Test both routes for every
  new agent.
- Gate provider-specific UI, including credentials and plan usage, on explicit
  capabilities rather than registry membership.

## Rules-Hygiene Check

The guidance is non-obvious because settings content, the Settings Editor, and
remote menu policy are separate surfaces. It is repeatedly encountered because
the Pi work exposed both a missing per-agent settings control and a remote menu
policy leak, while credentials and plan usage each require capability gating.
It is actionable because it specifies the required parity audit, route
invariant, capability model, and regression tests.

The rule avoids volatile file paths and implementation maps. It stays narrowly
scoped to Agent Threads coding-agent additions.

## Verification

- Confirm the new section appears once in the root `.rules`.
- Confirm it does not contradict the existing Rules Hygiene section.
- Run `git diff --check`.
- Update the existing pull request with a dedicated `.rules` commit.
