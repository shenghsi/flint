## ADDED Requirements

### Requirement: Agent Thread control uses the flintctl thread group
The application SHALL provide `flintctl thread retie --worktree <path>` and `flintctl thread create --worktree <current|new> --agent <agent> --prompt <prompt>` as the only Agent Thread control command forms, with the existing Agent Thread-only authorization and behavior.

#### Scenario: Agent Thread moves to an existing worktree
- **WHEN** a live Agent Thread invokes `flintctl thread retie --worktree <path>`
- **THEN** the application moves that calling Agent Thread to the specified existing worktree under the same authorization rules as the former command

#### Scenario: Agent Thread creates a sibling thread
- **WHEN** a live Agent Thread invokes `flintctl thread create` with a valid worktree mode, agent, and prompt
- **THEN** the application creates the requested sibling Agent Thread with the existing create-thread behavior

#### Scenario: Ordinary terminal invokes a thread command
- **WHEN** a recognized ordinary Flint terminal that is not a registered Agent Thread invokes a thread command
- **THEN** Flint rejects the request with `caller-not-agent-thread`

#### Scenario: Agent Thread uses an old flat command
- **WHEN** an Agent Thread invokes `retie-thread` or `create-thread` without the `thread` command group
- **THEN** `flintctl` rejects the unsupported command form

### Requirement: Each launched version installs its current Agent Thread instructions
On every application launch, Flint SHALL write the current release-channel-scoped executable marker and SHALL synchronize the Flint-managed block in each supported, installed agent's global instruction file. The managed block SHALL contain the commands and instruction text from the running Flint version.

#### Scenario: Current version launches for the first time
- **WHEN** Flint launches and a supported installed agent has no Flint-managed instruction block
- **THEN** Flint writes the current managed block with stable boundaries, its block version, current marker discovery, and `flintctl thread retie --worktree <path>`

#### Scenario: A newer Flint version launches
- **WHEN** the installed managed block has a different version or content from the running Flint version
- **THEN** Flint replaces that managed block with the running version's current commands and instructions

#### Scenario: No workspace or instruction prompt is open
- **WHEN** Flint launches without opening an Agent Threads workspace or showing an instruction prompt
- **THEN** Flint still synchronizes the current managed instruction blocks for supported installed agents

#### Scenario: Agent control is disabled in settings
- **WHEN** Flint launches while Agent Thread control is disabled
- **THEN** Flint still synchronizes the installed version's managed commands and instructions for supported installed agents

#### Scenario: Flint closes
- **WHEN** the running Flint instance closes
- **THEN** its managed instruction block remains installed for the next Flint session, and Flint does not change content outside that marked block

#### Scenario: Current marker refers to an older installation
- **WHEN** Flint launches and the release-channel-scoped marker has an older executable path or content
- **THEN** Flint replaces the marker with the running version's current `flintctl` location and metadata

### Requirement: Instruction synchronization preserves user content
Flint SHALL identify the block as Flint-owned instructions that apply only to Flint-launched Agent Threads. The block SHALL remain installed across Flint sessions. Flint SHALL replace or remove only its managed instruction block and SHALL preserve all content outside the stable managed-block boundaries. Flint SHALL migrate a known exact unmarked block written by an earlier Flint version before it installs the current marked block.

#### Scenario: User has content around a managed block
- **WHEN** Flint refreshes a managed block in a global instruction file that also contains user-authored text
- **THEN** the user-authored text before and after the managed block remains unchanged

#### Scenario: File contains a known earlier Flint block without boundaries
- **WHEN** Flint finds an exact unmarked block from an earlier Flint version
- **THEN** Flint replaces that block with one current marked block and does not leave the old commands in the file

#### Scenario: Similar user-authored text is not a known Flint block
- **WHEN** a global instruction file contains similar text that does not match a managed block or a known exact earlier Flint block
- **THEN** Flint preserves that text and installs the current managed block separately

### Requirement: Agent Thread remote route boundaries remain unchanged
The new command name and protocol surface SHALL NOT change the Direct or Tunneled remote launch, executable, credential, or traffic boundaries for Agent Threads.

#### Scenario: Direct remote Agent Thread uses control commands
- **WHEN** a Direct remote Agent Thread is configured
- **THEN** it uses only the configured ambient remote executable and gains no Flint-managed launch or credential control

#### Scenario: Tunneled remote Agent Thread uses control commands
- **WHEN** a Tunneled remote Agent Thread is configured
- **THEN** it uses only the pinned Flint-managed remote executable and routes its traffic through local Flint under the existing boundary
