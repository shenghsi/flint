## ADDED Requirements

### Requirement: Core review and writing surfaces remain available
The application SHALL retain terminal, project navigation, editor, git review,
diff, search, settings, theme, keymap, Markdown, and title bar surfaces.
The title bar SHALL render with visible content (at minimum the project name)
for every open project. The terminal panel icon SHALL be visible in the
status bar by default.

#### Scenario: Core workspace opens with retained surfaces
- **WHEN** a user opens a project
- **THEN** the user can open terminal sessions, browse project files, view diffs,
  inspect git status, search content, edit files, and open Markdown files

### Requirement: Product pruning proceeds before dependency pruning
The implementation MUST remove user-facing registration and startup paths for
retired features before removing workspace members and crate dependencies.

#### Scenario: Feature surface is removed before crate deletion
- **WHEN** a retired feature is selected for removal
- **THEN** app startup registration, menu entries, action exposure, settings
  defaults, and panel loading are removed or disabled before the crate is
  removed from the workspace

### Requirement: Retired native AI settings are not required
The application SHALL NOT require native model provider settings, Zed agent
settings, ACP server settings, or MCP settings for its default terminal-agent
workflows.

#### Scenario: User runs terminal agents without native AI setup
- **WHEN** no native model provider, ACP server, or MCP settings are configured
- **THEN** Codex and Claude terminal-thread launch actions can still be used
  through configured terminal commands
