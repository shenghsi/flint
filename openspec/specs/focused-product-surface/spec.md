## ADDED Requirements

### Requirement: Removed product surfaces are not exposed
The application SHALL NOT expose ACP, native Zed agent/chat, native model
provider, Copilot, edit prediction, collaboration, call, debugger, or Zed cloud
product surfaces in the default user interface.

#### Scenario: Default menus omit removed surfaces
- **WHEN** a user opens the application menus in a fresh workspace
- **THEN** the menus do not include ACP registry, agent chat, collaboration,
  calls, debugger, Copilot, edit prediction, or Zed cloud product entries

#### Scenario: Removed panels are not registered as workspace panels
- **WHEN** a workspace is initialized
- **THEN** collaboration, call, debugger, native agent, and ACP-backed panels are
  not added to any dock or center workspace area

### Requirement: Core review and writing surfaces remain available
The application SHALL retain terminal, project navigation, editor, git review,
diff, search, settings, theme, keymap, and Markdown surfaces.

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
