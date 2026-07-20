# Through-Flint Agent Menu Typography Design

## Problem

The new-thread dropdown for a Through-Flint remote project renders its launch
rows with `LabelSize::Small`, while the remote sign-out row uses the context
menu's default label size. The mixed typography is visible for both Codex and
Claude.

## Decision

All actionable rows in the Through-Flint new-thread dropdown use
`LabelSize::Small`. This includes:

- `New thread`
- the agent's permissive launch option
- `Sign out <agent> on remote…`

Codex and Claude use the same rendering path. The sign-out behavior,
confirmation prompt, and credential command remain unchanged.

## Scope

The change is local to the Agent Threads panel's Through-Flint menu. It does
not change the default `ContextMenu` typography and does not alter
Not-through-Flint menus.

The existing launch-option rows already use the intended size. The
Through-Flint sign-out row will use the same small-label rendering contract,
while the Not-through-Flint sign-out row will retain the standard context-menu
entry renderer.

## Testing

A focused regression test will exercise the route-dependent menu typography
policy: Through-Flint credential rows select the small-label renderer and
Not-through-Flint credential rows retain the standard renderer. Existing Agent
Threads panel tests will cover the unchanged launch and credential actions.

The implementation will then be checked with the focused `agent_threads` test,
the full crate test suite, formatting, and clippy for the affected crate.
