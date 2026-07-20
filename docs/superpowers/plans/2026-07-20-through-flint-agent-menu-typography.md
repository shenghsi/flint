# Through-Flint Agent Menu Typography Implementation Plan

## Goal

Render every actionable row in the Through-Flint Codex and Claude new-thread
dropdown with `LabelSize::Small`, without changing Not-through-Flint or global
context-menu typography.

## Implementation

1. Add a focused policy test in `crates/agent_threads/src/panel.rs` asserting
   that Through-Flint remote credential entries use the small label size while
   Not-through-Flint entries retain the standard renderer.
2. Run the focused test and confirm it fails before changing the renderer.
3. Add the smallest route-aware rendering policy and use a custom small-label
   entry for Through-Flint sign-out. Preserve the existing confirmation and
   logout callback. Leave the existing standard entry in place for
   Not-through-Flint.
4. Run the focused test, the `agent_threads` crate tests, formatting checks,
   and clippy for the affected crate.
5. Build `/tmp/Flint-Local.app` and handle the known debug bundling fallback if
   the script produces a fresh bundle but exits before copying it.

## Acceptance Criteria

- The Through-Flint Codex dropdown uses the small label size for every row.
- The Through-Flint Claude dropdown uses the small label size for every row.
- Sign-out still prompts for confirmation and launches the same remote logout
  command.
- Not-through-Flint dropdown typography is unchanged.
- No global `ContextMenu` typography changes are introduced.
