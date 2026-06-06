## ADDED Requirements

### Requirement: Terminal-backed agent thread launchers exist
The application SHALL provide launch actions for Codex, Claude, and plain shell
terminal threads.

#### Scenario: User starts a Codex thread
- **WHEN** the user invokes the new Codex thread action
- **THEN** the application opens a center terminal session running the configured
  Codex command in the current project context

#### Scenario: User starts a Claude thread
- **WHEN** the user invokes the new Claude thread action
- **THEN** the application opens a center terminal session running the configured
  Claude command in the current project context

#### Scenario: User starts a shell thread
- **WHEN** the user invokes the new shell thread action
- **THEN** the application opens a center terminal session using the configured
  shell in the current project context

### Requirement: Terminal threads are organized by project and activity
The application SHALL provide a thread organizer for terminal-backed agent and
shell sessions.

#### Scenario: Multiple projects have terminal threads
- **WHEN** terminal threads exist for more than one project
- **THEN** the organizer groups or scopes threads so the user can identify which
  project each thread belongs to

#### Scenario: Thread status changes
- **WHEN** a thread title changes or the terminal emits a bell
- **THEN** the organizer updates the displayed title or attention state for that
  thread

### Requirement: Terminal threads do not use ACP
Terminal-backed Codex and Claude threads SHALL NOT depend on ACP, ACP registry
installation, ACP session state, or ACP server settings.

#### Scenario: ACP configuration is absent
- **WHEN** no ACP registry, ACP server, or external-agent settings exist
- **THEN** configured Codex and Claude terminal thread launchers still work

### Requirement: CLI ownership boundaries are preserved
The application SHALL allow the CLI or TUI running in the terminal to own
authentication, model selection, subscriptions, tool configuration, skills,
instructions, and MCP configuration.

#### Scenario: Agent CLI prompts for login
- **WHEN** a launched agent CLI requires authentication
- **THEN** the authentication prompt remains inside the terminal session and the
  application does not require native provider credentials

### Requirement: Existing threads can be refocused
The thread organizer SHALL allow users to focus an existing terminal thread
without spawning a duplicate process.

#### Scenario: User selects an existing thread
- **WHEN** the user selects an existing Codex or Claude thread from the
  organizer
- **THEN** the corresponding terminal item is focused instead of starting a new
  terminal process
