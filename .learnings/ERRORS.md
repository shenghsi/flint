## [ERR-20260620-001] bundle-mac

**Logged**: 2026-06-20T18:51:00+08:00
**Priority**: medium
**Status**: pending, is it fixed?
**Area**: infra

### Summary

`cargo-bundle` panics when the repository-wide `CARGO_TERM_COLOR=always` setting forces ANSI output.

### Error

```text
called `Result::unwrap()` on an `Err` value: Error(Term(ColorOutOfRange), ...)
```

### Context

- Command: `./script/bundle-mac aarch64-apple-darwin`
- The script left `crates/flint/Cargo.toml` rewritten and a backup file because cleanup occurs after `cargo bundle`.

### Suggested Fix

Make the temporary manifest rewrite cleanup trap-safe. The panic also occurs with
`CARGO_TERM_COLOR=never`, `NO_COLOR=1`, and `TERM=dumb`, so the bundle parser needs separate
investigation.

### Metadata

- Reproducible: yes
- Related Files: script/bundle-mac, crates/flint/Cargo.toml

---
## [ERR-20260628-001] agent-reach Exa lookup

**Logged**: 2026-06-28T00:00:00+08:00
**Priority**: low
**Status**: pending
**Area**: infra

### Summary
The agent-reach Exa MCP server is not configured in mcporter.

### Error
```
[mcporter] Unknown MCP server 'exa'.
```

### Context
- Attempted an Exa code-context lookup through `mcporter call`.
- Direct local and GitHub searches remain available fallbacks.

### Suggested Fix
Configure the Exa MCP server or update the skill's documented fallback order.

### Metadata
- Reproducible: yes
- Related Files: /Users/shxi/.agents/skills/agent-reach/SKILL.md

---
## [ERR-20260628-002] apply_patch context mismatch

**Logged**: 2026-06-28T00:00:00+08:00
**Priority**: low
**Status**: resolved
**Area**: tests

### Summary
An exact-context patch missed an existing test initializer because its field order differed.

### Error
```
apply_patch verification failed: Failed to find expected lines
```

### Context
- Adding `show_plan_usage` to `AgentThreadSettingsContent` required updating an explicit fixture.

### Suggested Fix
Inspect the initializer before applying a narrow field insertion.

### Metadata
- Reproducible: no
- Related Files: crates/agent_threads/src/panel.rs

---
## [ERR-20260628-003] Linux cross-target check

**Logged**: 2026-06-28T00:00:00+08:00
**Priority**: low
**Status**: pending
**Area**: infra

### Summary
The installed Linux Rust target cannot be checked because its musl C compiler is absent.

### Error
```
failed to find tool "x86_64-linux-musl-gcc"
```

### Context
- Command: `cargo check -p agent_threads --target x86_64-unknown-linux-musl`
- Rust target is installed; the native cross-linker is not.

### Suggested Fix
Install the musl cross compiler before using this target for local portability checks.

### Metadata
- Reproducible: yes
- Related Files: crates/agent_threads/src/plan_usage.rs

---
