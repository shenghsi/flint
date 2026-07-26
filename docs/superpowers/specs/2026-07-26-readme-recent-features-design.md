# README Recent Features Design

**Date:** 2026-07-26
**Status:** Approved

## Goal

Update the README so its product overview reflects user-facing features added
since the README's last feature update, without turning the document into a
release log.

## Changes

- Name Codex, Claude Code, and Pi wherever the README describes Flint's
  supported coding agents.
- Clarify that the Agent Threads panel discovers and resumes sessions from the
  machine where they ran, including remote hosts.
- Add local cross-agent handoff as an Agent Threads capability, including the
  preview step before a new target thread starts.
- Mention that restored remote workspaces can be reopened and that Flint marks
  SSH projects configured to use the tunneled agent route.

## Exclusions

Do not document version bumps, internal refactors, storage-path changes, tests,
or implementation details. Do not add a version-specific "What's new" section
or rewrite the README's overall structure.

## Verification

Review every statement against the merged feature behavior, search for stale
references that imply only Codex and Claude Code are supported, and inspect the
final Markdown diff for consistent wording and formatting.
