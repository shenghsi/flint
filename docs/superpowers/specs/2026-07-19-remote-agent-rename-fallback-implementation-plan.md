# Remote Agent Rename Fallback Implementation Plan

## Goal

Allow the remote server to commit verified managed-agent directories on Linux
filesystems that reject `RENAME_NOREPLACE`, without introducing remote shell
commands or permitting normal destination replacement.

## Constraints

- Keep `renameat2(..., RENAME_NOREPLACE)` as the preferred Linux operation.
- Fall back only for `EINVAL`, `ENOSYS`, and `EOPNOTSUPP`.
- Limit the fallback to directory sources.
- Treat every existing destination, including a dangling symlink, as
  `AlreadyExists`.
- Propagate permission, missing-path, cross-device, and other errors unchanged.
- Preserve managed-agent staging, rollback, and cleanup behavior.

## Task 1: Reproduce the Unsupported Primitive

Modify `crates/remote_server/src/headless_project.rs` tests.

1. Inject the observed `EINVAL` result at the no-overwrite rename decision
   seam.
2. Assert that a staged directory is committed when the destination is absent.
3. Run the focused test and confirm it fails because no compatibility fallback
   exists.

## Task 2: Implement and Bound the Fallback

Modify `crates/remote_server/src/headless_project.rs`.

1. Route the Linux syscall result through a focused compatibility helper.
2. On the three unsupported-operation errors, inspect the source without
   following symlinks.
3. Reject non-directory sources.
4. Inspect the destination without following symlinks and reject any existing
   entry.
5. Plain-rename the sibling directory when the destination is absent.
6. Add regression cases for an existing directory, dangling symlink, file
   source, and unrelated errors.

Run:

```sh
cargo test -p remote_server remote_management_tests --lib
cargo test -p agent_threads managed_agent --lib
```

## Task 3: Validate and Package

Run:

```sh
cargo fmt --all -- --check
cargo test -p remote_server --lib
cargo test -p agent_threads --lib
./script/clippy -p remote_server
./script/clippy -p agent_threads
./script/bundle-tmp-app
```

If the known Dugite download or debug release-tail fails after producing the
fresh bundle, retain the prior `/tmp/Flint-Local.app`, reuse its identical
pinned Git binary, apply the standard development profile and ad-hoc signature,
and verify the replacement before installing it.

Reconnect the ParaStor remote so the debug remote server is rebuilt and
uploaded, then retry `New — Flint-managed Codex`. Success means the staged
directory commits, the absolute-path Codex thread launches once, and a later
launch reuses the verified installation.
