## ADDED Requirements

### Requirement: Terminals open as center workspace items
User-facing terminal creation actions SHALL open terminal sessions as center
workspace items by default.

#### Scenario: User creates a new terminal
- **WHEN** the user invokes the default new terminal action
- **THEN** a terminal opens in the active center pane rather than in the bottom
  dock

#### Scenario: User creates a terminal from an empty workspace
- **WHEN** the user opens a workspace and creates the first terminal
- **THEN** the terminal becomes primary workspace content in the center area

### Requirement: Bottom terminal panel is not the primary terminal model
The application SHALL NOT present the bottom terminal panel as the default
terminal experience.

#### Scenario: Default terminal entry point avoids bottom dock
- **WHEN** a user opens the primary terminal command from menus or command
  palette
- **THEN** the command opens or focuses a center terminal item instead of
  toggling a bottom terminal panel

### Requirement: Center terminals support normal workspace composition
Center terminal items SHALL support tabs, splits, focus, zoom, persistence, and
restoration in the same workspace composition model as other center items.

#### Scenario: Terminal is split beside a diff
- **WHEN** a user splits the workspace with a terminal and a diff view
- **THEN** both items remain visible and independently focusable as center
  workspace content

#### Scenario: Terminal layout is restored
- **WHEN** a workspace containing center terminal items is restored
- **THEN** the terminal item metadata and layout are restored without requiring
  the bottom terminal panel to be opened

### Requirement: Terminal status is visible in workspace chrome
Terminal items SHALL expose title and attention state so users can identify
running agent sessions.

#### Scenario: Terminal emits a title update
- **WHEN** the process running inside a terminal updates the terminal title
- **THEN** the visible terminal tab or thread entry reflects the updated title

#### Scenario: Terminal emits a bell
- **WHEN** a background terminal emits a bell character
- **THEN** the terminal is marked as needing attention without stealing focus
