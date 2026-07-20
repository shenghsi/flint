# Claude Platform Through-Flint Egress Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Permit the pinned Claude CLI to reach its current official Console-authentication hostname through Flint's restricted proxy.

**Architecture:** Correct the versioned Claude destination data in `agent_threads.rs`; the existing CONNECT proxy continues enforcing exact hosts and port 443. The existing exact-list regression test provides the complete behavioral seam.

**Tech Stack:** Rust, Flint agent-kind destination policy, Cargo tests.

## Global Constraints

- Replace `console.anthropic.com` with `platform.claude.com`.
- Keep `api.anthropic.com` and `claude.ai` unchanged.
- Do not add wildcard, update, installer, telemetry, or general internet hosts.
- Do not change SSH forwarding, proxy authentication, credentials, or Not-through-Flint behavior.
- Automated tests must not contact Anthropic.

---

### Task 1: Correct the Claude destination policy

**Files:**
- Modify: `crates/agent_threads/src/agent_threads.rs:218`
- Test: `crates/agent_threads/src/agent_threads.rs` test module

**Interfaces:**
- Consumes: `AgentKindDefinition::egress_hosts()` and the proxy's existing exact-host comparison.
- Produces: the exact Claude required-host slice `['api.anthropic.com', 'claude.ai', 'platform.claude.com']`.

- [ ] **Step 1: Update the exact-list regression expectation**

Change only the Claude assertion:

```rust
assert_eq!(
    claude.egress_hosts(),
    ["api.anthropic.com", "claude.ai", "platform.claude.com"]
);
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p agent_threads destination_policy_contains_only_required_model_and_authentication_hosts
```

Expected: FAIL showing actual `console.anthropic.com` versus expected `platform.claude.com`.

- [ ] **Step 3: Replace the stale production hostname**

Set the Claude registry entry to:

```rust
egress_hosts: &["api.anthropic.com", "claude.ai", "platform.claude.com"],
```

- [ ] **Step 4: Run the focused test and verify GREEN**

Run:

```bash
cargo test -p agent_threads destination_policy_contains_only_required_model_and_authentication_hosts
```

Expected: PASS.

- [ ] **Step 5: Commit the fix**

```bash
git add crates/agent_threads/src/agent_threads.rs
git commit -m "Fix Claude platform egress policy"
```

---

### Task 2: Verify and rebuild

**Files:**
- Modify: `docs/superpowers/specs/2026-07-20-claude-platform-egress-fix-design.md`
- Modify: `docs/superpowers/plans/2026-07-20-claude-platform-egress-fix-implementation.md`

**Interfaces:**
- Consumes: corrected Claude destination data.
- Produces: automated verification record and fresh `/tmp/Flint-Local.app` for live Claude validation.

- [ ] **Step 1: Run complete verification**

Run:

```bash
cargo test -p agent_threads
cargo fmt --all -- --check
./script/clippy -p agent_threads
git diff --check
```

Expected: every command succeeds.

- [ ] **Step 2: Build and verify the local app**

Run `./script/bundle-tmp-app`. If its documented debug-build release-path bug
stops before the copy, preserve the existing `/tmp/Flint-Local.app`, copy the
fresh `target/aarch64-apple-darwin/debug/bundle/osx/Flint.app`, compare the
source and copied `Contents/MacOS/flint` SHA-256 digests, and verify the copied
bundle with:

```bash
codesign --verify --deep --strict /tmp/Flint-Local.app
```

- [ ] **Step 3: Record completion**

Set the design status to `Implemented and automatically verified`, record exact
test/check results, and state that the user will perform the live Claude remote
request.

- [ ] **Step 4: Commit the verification record**

```bash
git add docs/superpowers/specs/2026-07-20-claude-platform-egress-fix-design.md docs/superpowers/plans/2026-07-20-claude-platform-egress-fix-implementation.md
git commit -m "Document Claude platform egress verification"
```
