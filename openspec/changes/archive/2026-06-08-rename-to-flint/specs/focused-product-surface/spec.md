## MODIFIED Requirements

### Requirement: Core review and writing surfaces remain available
The application SHALL retain terminal, project navigation, editor, git review,
diff, search, settings, theme, keymap, Markdown, and title bar surfaces.
The title bar SHALL render with visible content (at minimum the project name)
for every open project. The terminal panel icon SHALL be visible in the
status bar by default. The application SHALL identify as "Flint" in all
user-visible surfaces (title bar, menus, about dialog, URL scheme) while
preserving extension compatibility with the upstream Zed extension ecosystem.

#### Scenario: Core workspace opens with retained surfaces
- **WHEN** a user opens a project
- **THEN** the user can open terminal sessions, browse project files, view diffs,
  inspect git status, search content, edit files, and open Markdown files

#### Scenario: App identity shows Flint branding
- **WHEN** a user opens the application
- **THEN** the app menu shows "About Flint", "Quit Flint", and "Hide Flint"
- **AND** the binary is named `flint`
- **AND** config is stored in platform-appropriate Flint directories

#### Scenario: URL scheme uses flint://
- **WHEN** a `flint://` URL is opened by the OS
- **THEN** the Flint application handles it and opens the corresponding resource

#### Scenario: Existing Zed extensions load without modification
- **WHEN** a user installs an extension from the Zed extension registry
- **THEN** the extension loads and functions correctly without recompilation
- **AND** the WIT namespace `zed:extension` is preserved unchanged
