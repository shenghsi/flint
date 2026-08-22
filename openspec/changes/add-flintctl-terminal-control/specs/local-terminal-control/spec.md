## Purpose

Provide a stable, local, versioned command surface that lets processes in Flint terminals inspect and operate live terminals in the same workspace without exposing cross-workspace or remote-host access.

## ADDED Requirements

### Requirement: Flint provides a stable local control CLI
Flint SHALL install `flintctl` with `status`, `thread`, and `terminal` command groups. `flintctl` SHALL accept only the current noun-first command hierarchy. `flintctl status --json` SHALL report the running Flint version, protocol version, release channel, and supported command capabilities.

#### Scenario: Client checks available operations
- **WHEN** a caller runs `flintctl status --json` against a running compatible Flint instance
- **THEN** standard output contains a JSON status result with the application version, protocol version, release channel, and supported capabilities

#### Scenario: Client cannot find a matching Flint instance
- **WHEN** no control endpoint for the client's release channel is available
- **THEN** the CLI exits with a nonzero status and reports that Flint is not running or that the release channel does not match

#### Scenario: Caller uses an old flat command
- **WHEN** a caller invokes `flintctl retie-thread` or `flintctl create-thread`
- **THEN** the CLI rejects the unsupported command and directs the caller to the current noun-first hierarchy

### Requirement: Terminal identities are stable for one Flint process
Each live, controllable PTY terminal SHALL have an opaque terminal ID that is independent of pane position, tab position, process ID, and internal entity identity. Flint SHALL NOT reuse an ID during one application process, and SHALL NOT promise that an ID survives an application restart.

#### Scenario: Terminal moves in the workspace
- **WHEN** a live terminal moves to another pane or workspace location
- **THEN** its terminal ID remains unchanged and its reported location is updated

#### Scenario: Terminal is released
- **WHEN** a terminal entity or its view is released
- **THEN** its ID no longer resolves and a later terminal does not receive that ID in the same Flint process

#### Scenario: Terminal is not PTY-backed
- **WHEN** a display-only terminal exists
- **THEN** it does not receive a controllable terminal ID and does not appear in terminal control results

### Requirement: Terminal control enforces the caller and workspace boundary
Flint SHALL accept terminal commands only from a live local PTY terminal that Flint controls. A caller SHALL access only controllable terminals in the caller's workspace. Flint SHALL derive caller identity from operating-system process identity and SHALL NOT treat a client-supplied token or an ordinary terminal's working directory as identity.

#### Scenario: Ordinary Flint terminal calls flintctl
- **WHEN** the peer process ancestry contains the root process of a live local Flint PTY terminal
- **THEN** Flint resolves that terminal as the caller

#### Scenario: Registered Agent Thread uses a delegated command process
- **WHEN** peer ancestry does not identify a terminal but the existing constrained Agent Thread fallback identifies a live Agent Thread
- **THEN** Flint resolves the registered terminal for that Agent Thread as the caller

#### Scenario: External process calls flintctl
- **WHEN** a same-user process is outside a live Flint terminal and is not resolved by the Agent Thread fallback
- **THEN** Flint rejects the request with `caller-not-recognized`

#### Scenario: Caller targets another workspace
- **WHEN** a recognized caller requests a terminal that belongs to another workspace
- **THEN** Flint rejects the request with `terminal-outside-workspace`

#### Scenario: Caller registry entry is not ready
- **WHEN** Flint can identify a possible caller but its terminal registration is not complete
- **THEN** Flint returns a retryable not-ready response

### Requirement: Callers can identify and list controllable terminals
`flintctl terminal current` SHALL return the caller's terminal. `flintctl terminal list` SHALL return live controllable terminals in the caller's workspace, sorted by creation sequence, and SHALL exclude the caller by default unless `--all` is given.

#### Scenario: Caller requests its current terminal
- **WHEN** a recognized caller runs `flintctl terminal current`
- **THEN** the result includes its terminal ID, title, nullable working directory, Agent Thread state, and exited state

#### Scenario: Caller lists peer terminals
- **WHEN** a recognized caller runs `flintctl terminal list`
- **THEN** the result contains other live controllable terminals in the same workspace in creation order and does not expose pane positions

#### Scenario: Caller includes itself
- **WHEN** a recognized caller runs `flintctl terminal list --all`
- **THEN** the result also contains the caller's terminal

### Requirement: Callers can read bounded terminal snapshots
`flintctl terminal read` SHALL return plain text from `visible`, `recent`, or `recent-unwrapped` terminal content. The default source SHALL be `recent`, the default line count SHALL be 120, and the line count and response byte size SHALL have hard limits.

#### Scenario: Caller reads recent output
- **WHEN** a caller reads a live target without selecting a source or line count
- **THEN** Flint returns at most 120 lines from the current grid and available primary-screen scrollback

#### Scenario: Caller reads unwrapped output
- **WHEN** a caller selects `recent-unwrapped`
- **THEN** Flint joins soft-wrapped display rows into logical lines in the returned available history

#### Scenario: Requested content exceeds a limit
- **WHEN** terminal content exceeds the configured line or response byte limit
- **THEN** Flint truncates text at a valid UTF-8 boundary and reports `truncated: true`

#### Scenario: Target uses the alternate screen
- **WHEN** the target is on the alternate screen
- **THEN** the result reports alternate-screen use and does not imply that rows absent from host scrollback can be recovered

### Requirement: Callers can send validated text
`flintctl terminal send-text` SHALL write the supplied text as terminal input without adding Enter or bracketed-paste framing. Flint SHALL reject NUL bytes, oversized input, exited targets, and targets that are no longer PTY terminals.

#### Scenario: Caller sends text
- **WHEN** a caller sends valid text to a live PTY target
- **THEN** Flint writes exactly that text as terminal input without an added Enter key

#### Scenario: Text input is invalid
- **WHEN** input contains a NUL byte or exceeds the request limit
- **THEN** Flint rejects the full request and writes no input

#### Scenario: Target has exited
- **WHEN** a caller sends text to an exited target
- **THEN** Flint writes no input and returns `terminal-exited`

### Requirement: Callers can send validated terminal keys
`flintctl terminal send-key` SHALL accept a documented set of key names and modifiers and SHALL use the same terminal key behavior as UI keyboard input. Flint SHALL validate the full key list before it writes any input.

#### Scenario: Caller sends supported keys
- **WHEN** a caller sends supported keys such as `enter`, `escape`, `ctrl-c`, or `alt-left`
- **THEN** Flint writes the mapped terminal input in request order with the target's current terminal modes applied

#### Scenario: One key is invalid
- **WHEN** any key in a multi-key request is invalid
- **THEN** Flint returns `invalid-key` and writes none of the keys

### Requirement: Callers can run a command as one input operation
`flintctl terminal run` SHALL validate the command, write its text, and write Enter as one non-interleavable control operation. The command SHALL be input to the target terminal and SHALL NOT start a separate shell or assert that the target is at a shell prompt.

#### Scenario: Caller runs a valid command
- **WHEN** a caller runs a valid command in a live PTY target
- **THEN** the command text and Enter are written without bytes from another control request interleaving within that operation

#### Scenario: Target is not ready for shell input
- **WHEN** the target is running an interactive application instead of waiting at a shell prompt
- **THEN** Flint still sends the command as terminal input and makes no shell-prompt guarantee

### Requirement: Callers can wait for terminal output
`flintctl terminal wait-output` SHALL wait for either a literal string or a valid Rust regular expression in a selected bounded snapshot. It SHALL search existing output before waiting for changes, require a protocol timeout, return the matching final snapshot, and stop when the pattern matches, the timeout expires, the target exits or is released, or the client disconnects.

#### Scenario: Existing output matches
- **WHEN** the selected snapshot already contains the requested literal or regular expression
- **THEN** Flint returns the matching snapshot without waiting for new output

#### Scenario: Later output matches
- **WHEN** existing output does not match and later target output matches before the timeout
- **THEN** Flint returns the final bounded snapshot that contains the match

#### Scenario: Pattern is invalid
- **WHEN** a caller supplies an invalid regular expression
- **THEN** Flint returns `invalid-pattern` without starting a wait

#### Scenario: Wait times out
- **WHEN** the pattern does not match before the required timeout
- **THEN** Flint ends the wait and returns `timeout`

#### Scenario: Target is replaced
- **WHEN** the original target is released and another terminal replaces it in the same UI location during a wait
- **THEN** the replacement terminal cannot satisfy the wait

#### Scenario: Client disconnects
- **WHEN** the client connection closes during a wait
- **THEN** Flint cancels the pending wait and its terminal observation

### Requirement: The control protocol is versioned and bounded
Every current request and response SHALL include a protocol version. Flint SHALL reject an unsupported required major version, treat minor-version fields as additive, cap request and response sizes, and return operation-specific success results or machine-readable error codes for expected failures.

#### Scenario: Client requires an unsupported major version
- **WHEN** a request requires a protocol major version that the server does not support
- **THEN** Flint rejects the request with a typed protocol error

#### Scenario: Response contains an additive field
- **WHEN** a response includes a field added in a compatible minor version
- **THEN** an older compatible client can ignore the unknown field

#### Scenario: Explicit terminal ID is stale
- **WHEN** a request names a terminal ID that is no longer registered
- **THEN** Flint returns `terminal-not-found` as a hard error and the CLI does not retry it

#### Scenario: Retryable registration race occurs
- **WHEN** the server returns not-ready while resolving the caller
- **THEN** `flintctl` applies bounded retry backoff and reports `caller-not-recognized` if all retries end without a match

### Requirement: Long-lived requests use disconnect-aware framing
Clients SHALL send one bounded length-prefixed request per connection and keep the connection open until the response. The server SHALL detect client disconnection while a wait is pending. The server SHALL NOT accept legacy EOF-framed requests.

#### Scenario: Current client waits for output
- **WHEN** a current-protocol client sends a terminal output wait
- **THEN** the connection remains usable to detect disconnection until Flint returns the one response

#### Scenario: Client sends a legacy framed request
- **WHEN** a client sends an EOF-framed request
- **THEN** Flint rejects the request before command dispatch

### Requirement: Terminal control remains local and release-channel scoped
Terminal control SHALL expose only terminals whose PTY process runs on the same machine as the Flint control server. Control endpoints and discovery SHALL remain user-scoped and release-channel scoped, and the Unix socket SHALL retain `0600` permissions.

#### Scenario: Workspace has a remote PTY terminal
- **WHEN** a terminal shell runs through a remote server
- **THEN** the local control server does not expose that terminal for control

#### Scenario: Remote workspace has a local-route terminal
- **WHEN** a terminal in a remote project uses Flint's local route and its PTY runs beside the local control server
- **THEN** local `flintctl` can control it subject to the normal caller and workspace boundary

#### Scenario: Stable client discovers a Nightly endpoint
- **WHEN** a Stable client can see a Nightly or development control endpoint
- **THEN** it does not connect to that endpoint by accident

### Requirement: Flint packages only flintctl as its control command
Flint SHALL package `flintctl` on macOS, Linux, and Windows and SHALL stop building and packaging the `flint-agent-control` executable. `flintctl` SHALL NOT locate or start Flint.

#### Scenario: User installs or updates Flint
- **WHEN** a supported Flint package is installed
- **THEN** the package contains `flintctl` and does not contain `flint-agent-control`

#### Scenario: Existing script invokes the removed executable
- **WHEN** a script invokes `flint-agent-control` after updating to this release
- **THEN** the command is unavailable and the script must migrate to `flintctl`

#### Scenario: flintctl runs without Flint
- **WHEN** no matching control endpoint exists
- **THEN** `flintctl` reports the connection failure and does not start Flint
