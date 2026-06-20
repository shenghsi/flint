# Crash Analysis: Agent threads store missing at startup

## Crash Summary

- **Error:** `no state of type agent_threads::store::GlobalAgentThreadStore exists`
- **Crash Site:** `AgentThreadStore::global` in `crates/agent_threads/src/store.rs`

## Root Cause

Production startup loaded `AgentThreadsPanel` for each workspace without first calling
`agent_threads::init`. The panel constructor immediately reads the global
`AgentThreadStore`, so the first workspace window panicked during launch.

Tests did not expose the omission because the shared Flint test setup called
`agent_threads::init` directly.

## Reproduction

The regression test initializes Flint through the shared workspace startup path and then
reads the global agent thread store:

`cargo test -p flint test_initialize_workspace_initializes_agent_threads`

Before the fix, it panics with the same missing-global error as the installed application.

## Suggested Fix

Initialize `agent_threads` inside `initialize_workspace`, before workspace observers can
load `AgentThreadsPanel`. Remove the duplicate initialization from test-only setup so tests
exercise the production path.
