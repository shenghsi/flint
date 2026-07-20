# Claude Platform Through-Flint Egress Fix Design

**Date:** 2026-07-20  
**Status:** Approved for implementation

## Problem

The pinned Claude CLI connects to `platform.claude.com`, but Flint's Claude
Through-Flint destination policy still permits the obsolete
`console.anthropic.com` hostname. Flint's CONNECT proxy therefore returns
`403 Forbidden` and closes the socket. Claude surfaces that policy rejection as
`ERR_SOCKET_CLOSED` and reports that it cannot connect to Anthropic services.

Anthropic's current official network requirements identify:

- `api.anthropic.com` for Claude API requests;
- `claude.ai` for Claude account authentication; and
- `platform.claude.com` for Anthropic Console account authentication.

The active Flint egress design already requires `platform.claude.com` instead
of `console.anthropic.com`; the implementation and its exact-list regression
test drifted from that design.

## Fix

Replace `console.anthropic.com` with `platform.claude.com` in the Claude agent
kind's required egress hosts. Keep the policy exact and restricted; do not add
wildcards, installer/update hosts, telemetry hosts, or general internet access.

No changes are required to the SSH reverse forward, proxy authentication,
Claude credentials, managed CLI installation, self-update suppression, or
Not-through-Flint behavior.

## Testing

Update the existing exact destination-policy test first and observe it fail
against the stale implementation. Then update the policy and verify the focused
test, the complete `agent_threads` suite, formatting, and Flint clippy.

Build a fresh `/tmp/Flint-Local.app` for live validation on the user's Claude
remote. The user performs the live provider request; automated tests do not
contact Anthropic.

## Acceptance Criteria

- Through-Flint Claude permits CONNECT to `platform.claude.com:443`.
- `console.anthropic.com` is no longer in Claude's required destination list.
- The other required Claude hosts remain unchanged.
- The destination policy remains exact-host and port-443-only.
- Codex and Not-through-Flint behavior remain unchanged.
