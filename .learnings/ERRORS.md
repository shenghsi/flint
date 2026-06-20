## [ERR-20260620-001] bundle-mac

**Logged**: 2026-06-20T18:51:00+08:00
**Priority**: medium
**Status**: pending
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
