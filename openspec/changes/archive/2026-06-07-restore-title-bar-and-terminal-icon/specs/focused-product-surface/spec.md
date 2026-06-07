## MODIFIED Requirements

### Requirement: Core review and writing surfaces remain available
The application SHALL retain terminal, project navigation, editor, git review,
diff, search, settings, theme, keymap, Markdown, and title bar surfaces.
The title bar SHALL render with visible content (at minimum the project name)
for every open project. The terminal panel icon SHALL be visible in the
status bar by default.

#### Scenario: Title bar renders with project name
- **WHEN** a user opens a project
- **THEN** the title bar is visible at the top of the window showing the project name

#### Scenario: Title bar renders without a git repository
- **WHEN** a user opens a project that is not a git repository
- **THEN** the title bar is still visible and displays the project name

#### Scenario: Terminal icon is visible in status bar by default
- **WHEN** a user opens a project with default settings
- **THEN** the terminal icon appears in the bottom status bar and can be clicked
  to toggle the terminal panel
