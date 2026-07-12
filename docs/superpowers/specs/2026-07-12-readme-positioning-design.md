# README positioning and local agent directories

## Scope

Keep repository-specific agent tooling and OpenSpec data on each contributor's machine, relocate the product screenshot into the repository's assets, and clarify Flint's position between conventional IDEs and terminal emulators.

## Repository changes

- Ignore `/openspec/`, `/.codex/`, `/.claude/`, `/.learnings/`, and `/.agents/` at the repository root.
- Remove those directories from Git's index without deleting their local contents.
- Move the root `screenshot.png` to `assets/screenshots/flint-workspace.png` and track it as a README asset.

## README changes

- Show the workspace screenshot directly after the introduction with descriptive alternative text.
- Explain that Flint combines IDE-grade editing, language tooling, Git, diffs, and extensions with terminal-native command-line workflows.
- Compare Flint with conventional IDEs and terminal emulators using generic categories rather than named competitors.
- Retain the existing feature details, removed product surfaces, installation instructions, development links, licensing, and acknowledgements.

## Verification

- Confirm all five local directories are ignored and absent from the Git index.
- Confirm all local directory contents remain on disk.
- Confirm the screenshot's README path resolves to a tracked image.
- Review the rendered Markdown structure and final Git diff.
