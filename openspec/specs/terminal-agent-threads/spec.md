## Requirements

### Requirement: Terminal-backed agent thread launchers exist
The application SHALL provide launch actions for agent kinds in a code-level
registry. Codex and Claude SHALL be registered by default. Plain shell
threads are out of scope for this registry.

#### Scenario: User starts a Codex thread
- **WHEN** the user invokes the new Codex thread action or the Codex
  section's "+" button
- **THEN** the application opens a center terminal session running the
  configured Codex command in the current project context

#### Scenario: User starts a Claude thread
- **WHEN** the user invokes the new Claude thread action or the Claude
  section's "+" button
- **THEN** the application opens a center terminal session running the
  configured Claude command in the current project context

### Requirement: Terminal threads are organized by project and activity
The application SHALL provide a docked sidebar panel, scoped to the current
workspace's project, that organizes terminal-backed agent threads by agent
kind.

#### Scenario: Panel scoped to current project
- **WHEN** the panel is open in a workspace window
- **THEN** it shows only threads whose working directory matches one of that
  workspace's worktree roots, grouped into one section per registered agent
  kind

#### Scenario: Thread status changes
- **WHEN** a thread title changes or the terminal emits a bell
- **THEN** the panel updates the displayed title or attention state for that
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
The panel SHALL allow users to focus a live (already-open) terminal thread
without spawning a duplicate process. This is distinct from resuming a
historical thread, which starts a new terminal session.

#### Scenario: User clicks a live thread
- **WHEN** the user clicks a live Codex or Claude thread row in the panel
- **THEN** the corresponding open terminal item is focused instead of
  starting a new terminal process

### Requirement: Historical agent threads can be resumed
The application SHALL read each registered agent kind's persisted session
history from disk, scoped to the current project, and SHALL allow resuming a
historical thread in a new center terminal session.

#### Scenario: User resumes a past Claude conversation
- **WHEN** the user clicks a historical Claude thread row for the current
  project
- **THEN** the application opens a center terminal session running the
  configured Claude command with its resume flag and that session's id

#### Scenario: User resumes a past Codex conversation
- **WHEN** the user clicks a historical Codex thread row for the current
  project
- **THEN** the application opens a center terminal session running the
  configured Codex command with its resume subcommand and that session's id

#### Scenario: A resumed or newly active thread is not duplicated
- **WHEN** a historical thread is resumed, or a brand-new thread becomes
  active for an agent kind and project that already has a live thread of
  that kind
- **THEN** the panel shows it once, as a live thread, not as a separate
  historical entry

#### Scenario: Remote project's history is read from the remote host
- **WHEN** the current workspace's project is a remote (SSH) connection
- **THEN** historical threads are read from that remote host's persisted
  session data, not the local machine's

#### Scenario: Remote history cannot be read
- **WHEN** the current workspace's project is a remote (SSH) connection and
  resolving its environment or session data fails
- **THEN** the affected agent kind's section indicates that history could
  not be scanned, distinct from showing no history

### Requirement: Agent thread sections are capped and foldable
Each agent kind's section SHALL display at most a configurable number of
threads (default 5), SHALL offer a control to reveal the remaining threads,
and SHALL be independently collapsible.

#### Scenario: More threads exist than the visible cap
- **WHEN** an agent kind has more matching threads than the configured cap
- **THEN** the section shows the most recent threads up to the cap and a
  "Show more" control that reveals the rest when activated

#### Scenario: User collapses a section
- **WHEN** the user toggles a section's fold control
- **THEN** that section's thread list is hidden while its header, count, and
  "+" control remain visible

### Requirement: Users can resume with additional CLI options
The application SHALL allow resuming a historical thread with agent-specific
additional command-line options, defined per registered agent kind.

#### Scenario: User resumes Claude bypassing permission prompts
- **WHEN** the user selects a "resume with options" entry for a historical
  Claude thread
- **THEN** the application opens a center terminal session running the
  configured Claude resume command with the corresponding additional flag

#### Scenario: User resumes Codex bypassing approvals and sandboxing
- **WHEN** the user selects a "resume with options" entry for a historical
  Codex thread
- **THEN** the application opens a center terminal session running the
  configured Codex resume command with the corresponding additional flag
</content>
