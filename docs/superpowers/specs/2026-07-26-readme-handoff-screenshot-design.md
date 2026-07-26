# README Handoff Screenshot Design

**Date:** 2026-07-26
**Status:** Approved

## Goal

Show the cross-agent handoff menu in the README using the supplied screenshot.

## Placement

Add `assets/screenshots/handoff.png` as a standalone Markdown image immediately
after the complete **Added** feature list and before the **Removed** heading.
This keeps the feature bullets uninterrupted while placing the screenshot next
to the handoff description.

Use the alt text:

> Agent Threads menu with Hand off to Codex and Hand off to Pi actions

Keep the supplied PNG unchanged and allow Markdown to render it at its natural
size.

## Verification

- Confirm the PNG decodes successfully and record its dimensions.
- Confirm the README image path resolves with exact filename casing.
- Run `git diff --check` and inspect the rendered Markdown structure.
- Do not include the unrelated modification to `flint-workspace.png`.
