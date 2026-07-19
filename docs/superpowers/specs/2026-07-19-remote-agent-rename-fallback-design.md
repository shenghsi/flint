# Support Managed Agent Installation on Filesystems Without `RENAME_NOREPLACE`

## Status

Approved in conversation on 2026-07-19.

Design owner: Codex.

## Problem

Flint uploads a pinned managed-agent executable into a private staging
directory, verifies its digest and version on the remote host, and commits the
directory with a no-overwrite rename. On Linux, the remote server implements
that operation with `renameat2(..., RENAME_NOREPLACE)`.

The tested remote host runs Linux 3.10 and stores the user's home directory on
ParaStor. ParaStor rejects `RENAME_NOREPLACE` with `EINVAL` even though the
source and destination are on the same device and the destination is absent.
The verified upload therefore fails only at the final commit step.

## Compatibility Behavior

The Linux no-overwrite rename remains the preferred operation. Flint uses a
compatibility fallback only when it fails with an error that specifically
indicates an unsupported primitive:

- `EINVAL`;
- `ENOSYS`; or
- `EOPNOTSUPP`.

The fallback applies only when the source is a directory, which is the managed
agent install and rollback case. Flint checks the destination with
`symlink_metadata` so a dangling symlink also counts as existing. If anything
already occupies the destination, the operation returns `AlreadyExists`
without modifying either path. Otherwise Flint performs a same-filesystem
plain directory rename.

File renames do not use the fallback because a plain POSIX file rename can
silently replace an existing file. Permission, missing-path, cross-device, and
all other errors also propagate unchanged.

## Transactional Installation

The compatibility path preserves the existing managed-agent transaction:

1. upload into a private sibling staging directory;
2. verify remote digest, executable permission, and pinned version;
3. if a prior installation exists, rename it to a unique rollback directory;
4. rename the verified staging directory to the versioned destination;
5. remove the rollback only after the new installation commits; and
6. restore the rollback if the new commit fails.

All managed paths are sibling directories on the same remote filesystem. The
application-level coordinator prevents duplicate provisioning from one Flint
process. On filesystems without an atomic no-replace primitive, a destination
created between the existence check and plain rename remains a platform
limitation. A competing valid managed installation is non-empty, so POSIX
directory rename will reject replacing it; Flint never deliberately removes a
destination to make the fallback succeed.

## Error Handling

The remote server retains causal path context in errors. An existing
destination is reported as an existing-destination failure. If the fallback
rename itself fails, the original installation transaction cleans the staging
directory and restores any prior installation using the same compatibility
path.

No remote shell command is introduced. This avoids shell startup modules,
locale configuration, `PATH`, and command-availability dependencies.

## Tests

Add deterministic remote-server tests that inject the observed unsupported
rename result and assert that:

- an absent destination accepts a staged directory through the fallback;
- an existing directory or dangling symlink is preserved and rejected;
- a file source does not fall back;
- permission and cross-device errors remain unchanged; and
- the existing managed-agent rollback tests remain green.

Run the focused remote-management and managed-agent tests, formatting, and
clippy. Package a fresh `/tmp/Flint-Local.app`, reconnect so Flint uploads the
new debug remote server, and retry `New — Flint-managed Codex` on the ParaStor
host.
