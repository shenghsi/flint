# Remote Agent Credential Menu and Sign-Out Design

**Date:** 2026-07-19
**Status:** Implemented and automatically verified

## Problem

The Agent Threads new-thread menu offers separate sign-in, sign-in status,
sign-out, and provider-management actions for Codex and Claude. A Flint-managed
agent launch already starts the official pinned CLI, and that CLI begins its own
authentication flow when the remote credential is missing.

The separate sign-in and status actions are redundant. The status action also
cannot prove that the stored credential is accepted or that Through-Flint
networking works; a real agent request is the definitive check. Both actions
currently invoke the configured ambient agent command rather than resolving
Flint's managed installation, so they fail on a remote host where only the
Flint-managed binary exists.

The provider action opens a web management page rather than changing the
credential stored by the CLI on the remote host. Presenting it beside remote
credential actions is therefore misleading.

## Behavior

- Remove the remote sign-in, sign-in status, and provider-management entries
  from the Codex and Claude new-thread menus.
- Keep **Sign out Codex on remote…** and **Sign out Claude on remote…**.
- When the project route is **Through Flint**, sign-out must use the pinned
  Flint-managed executable for that agent and retain the Through-Flint route
  guarantee.
- When the project route is **Direct**, sign-out continues to use
  the configured command for that agent.
- Do not change authentication storage.

Users who need to authenticate select **New — Flint-managed Codex** or
**New — Flint-managed Claude**. If the remote credential is absent, the
official CLI presents its normal sign-in flow in the new Agent Thread.

## Implementation

Add a small menu policy in `crates/agent_threads/src/panel.rs` that offers only
remote sign-out for the registered Codex and Claude agent kinds.

Extend the credential-command launch path in `crates/agent_threads/src/store.rs`
so sign-out follows the selected remote-agent route. Under Through Flint,
reuse the existing managed-agent preparation path to find or provision the
pinned official CLI, build the logout command with that executable, apply the
self-update suppression policy, and launch it with Through Flint as the
required route. Existing managed-agent progress, confirmation, and duplicate
provisioning behavior remain in effect if the executable is not ready. Under
Direct, preserve the current configured-command behavior.

If the route changes while a managed agent is being prepared, abort sign-out
with the existing route-change error instead of launching under a different
route.

## Testing

A focused unit test will prove that:

- Codex and Claude offer none of the standalone remote sign-in, sign-in status,
  or provider-management entries;
- both agents still offer remote sign-out;
- Through-Flint sign-out replaces only the executable with the corresponding
  managed path, retains the logout arguments and configured environment,
  applies self-update suppression, and requires the Through-Flint route; and
- Not-through-Flint sign-out uses the configured command without managed
  provisioning.

The automated tests do not launch either provider CLI or contact its service.
The full `agent_threads` suite, formatting check, and Flint clippy check run
before delivery. Live remote validation covers Codex only in this iteration;
live Claude validation is deferred until the user opens the separate Claude
test remote.

## Acceptance Criteria

- Remote Codex and Claude menus contain no sign-in, sign-in status, or
  provider-management entries.
- Remote sign-out remains available for both agents.
- Through-Flint sign-out runs the corresponding pinned managed executable
  through Flint, even if the remote host has its own agent installation or
  internet access.
- Not-through-Flint sign-out uses the configured command for the selected
  agent.
- Launching Flint-managed Codex remains unchanged.
- Launching Flint-managed Claude remains unchanged.
- No live Claude process or provider request is used for validation in this
  iteration.

## Implementation Verification

Implemented on 2026-07-19. The remote credential menus now expose only
sign-out for Codex and Claude. Through-Flint sign-out prepares the pinned
managed executable and requires the Through-Flint route; Not-through-Flint
sign-out retains the configured command.

Verification completed with:

- `cargo test -p agent_threads credential_command` — 3 passed;
- `cargo test -p agent_threads codex_and_claude_remote_credential_menus_only_offer_sign_out` — 1 passed;
- `cargo test -p agent_threads` — 151 passed;
- `cargo fmt --all -- --check` — passed; and
- `./script/clippy -p agent_threads` — passed with warnings denied.

These automated tests construct Claude command values but do not launch Claude
or contact Anthropic. Live Claude validation remains deferred to the separate
remote selected by the user.
