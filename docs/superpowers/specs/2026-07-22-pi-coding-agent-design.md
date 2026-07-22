# Pi Coding Agent Integration Design

## Summary

Add Pi as a third first-class terminal coding agent beside Codex and Claude in Flint's Agent Threads panel. The integration uses Pi's interactive terminal interface, preserves Pi's provider-neutral model, supports Pi's persisted sessions, and provisions pinned standalone Pi binaries for managed tunneled remote projects.

Pi continues to own provider selection, model selection, and authentication. Flint owns process launch, project-scoped thread discovery, session restoration, remote binary provisioning, and tunneled network policy.

References:

- [Pi coding agent](https://github.com/earendil-works/pi/tree/main/packages/coding-agent)
- [Pi session format](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md)
- [Pi v0.81.1 release](https://github.com/earendil-works/pi/releases/tag/v0.81.1)

## Goals

- Show Pi after Codex and Claude in the Agent Threads panel.
- Launch local and directly routed remote Pi sessions through a configurable command.
- Discover project-scoped Pi history and resume sessions from the panel.
- Assign session IDs to new Pi threads so they can be restored across Flint restarts.
- Preserve existing live-thread deduplication, completion notifications, and close behavior.
- Provision and verify pinned standalone Pi binaries for tunneled remote projects.
- Permit tunneled access to Pi's built-in providers while preserving an explicit host allowlist.
- Keep existing settings compatible.

## Non-goals

- Embedding Pi through its RPC mode or building a native Pi conversation UI.
- Reimplementing Pi's provider, model, thinking-level, extension, or skill interfaces.
- Displaying a Flint quota meter for Pi.
- Providing provider-specific sign-in, sign-out, or credential-management actions for Pi.
- Copying local Pi credentials to a remote host.
- Supporting arbitrary custom-provider or extension network destinations through a tunneled route.
- Adding a Pi-specific permission-bypass launch option.

## Agent Definition and Capabilities

Register an `AgentKindDefinition` with these values:

| Field | Value |
| --- | --- |
| ID | `pi` |
| Label | `Pi` |
| Default command | `pi` |
| Home environment variable | none |
| Home directory | `.pi/agent` |
| Session ID flag | `--session-id` |
| Resume command | `--session <id>` |
| Resume options | none |
| History provider | `PiHistoryProvider` |
| Credential policy | none |
| Plan usage provider | none |

The absence of a home environment variable must be represented explicitly rather than with a fabricated environment variable. History resolution falls back to `$HOME/.pi/agent` locally and on remote hosts.

Provider-specific registry capabilities become optional. Codex and Claude retain their existing credential and plan-usage behavior. Pi is omitted from credential menus and plan-usage requests, avoiding dummy commands and recurring unsupported-provider errors.

Add a `NewPiThread` action and register it on each workspace. The action resolves the `pi` definition and uses the same default-launch path as other agents.

## Panel and Settings

Pi appears after Codex and Claude and uses an `AiPi` icon backed by an SVG derived from Pi's official logo. The icon is a normal repository-native vector asset and follows the existing icon color treatment.

The panel provides Pi with the shared controls that apply to its capabilities:

- Start a new thread.
- Show live and historical project threads.
- Open, resume, and close threads.
- Collapse the section and show additional history.
- Hide the section through settings.

The panel does not render quota text, a credential menu, or a launch-option dropdown for Pi.

Add `pi: Option<AgentThreadCommandContent>` to the settings content and a resolved `pi: AgentLaunchCommand` to `AgentThreadSettings`. A missing block resolves to:

```json
"pi": {
  "command": "pi",
  "args": [],
  "env": {},
  "hidden": false
}
```

Custom command, arguments, environment, working directory, and visibility use the same behavior as the Codex and Claude blocks. Existing settings require no migration.

## New Thread and Resume Flow

For a new Pi thread, Flint generates a UUID and launches:

```text
pi [configured arguments] --session-id <uuid>
```

The generated ID is recorded in live thread metadata immediately. This allows session restoration even before Pi writes its first history entry.

For a historical thread, Flint launches:

```text
pi --session <session-id> [configured resume arguments]
```

with the session's project root as the working directory. Pi has no additional built-in resume option, so no per-thread launch-option state is created.

Pi creates provider credentials on the machine where it runs. A user authenticates by opening a Pi session and using `/login`. Managed remote sessions therefore keep their own remote Pi credentials; Flint neither reads nor uploads the local `auth.json`.

## History Discovery

Pi sessions are JSONL files below:

```text
~/.pi/agent/sessions/--<encoded-project-path>--/*.jsonl
```

For each open project root, `PiHistoryProvider` derives Pi's encoded session directory using the target host's path style. It examines at most the 200 most recent `.jsonl` files in that directory. The bound applies independently per project root.

Every candidate must contain a valid session header with:

- `type: "session"`
- a non-empty string `id`
- an RFC 3339 `timestamp`
- a string `cwd`

The header `cwd` must equal an open project root according to the host's path style. Directory-name matching alone is insufficient because Pi's path encoding can collide.

The parser extracts:

- Session ID from the header.
- Project root from the verified header `cwd`.
- Title from the last valid `session_info.name`, when it is non-empty.
- Otherwise, title from the first textual user message.
- Otherwise, the fallback `Pi session`.
- Last activity from the greatest valid entry timestamp, falling back to the header timestamp.

Titles are normalized and truncated to 60 characters consistently with the existing providers. For user messages with content blocks, only text blocks contribute to the title.

Blank lines, malformed JSON lines, and unknown entry types are ignored. A missing or invalid header rejects only that file. A valid header with later malformed records still produces a thread using the valid metadata collected before and after those records.

The existing history parse cache and filesystem watcher infrastructure is reused. Pi history is merged with live metadata through the existing deduplication logic.

## Managed Standalone Releases

Pin Pi v0.81.1 initially. A later version update follows the existing explicit pinned-agent update process.

Map upstream standalone assets to supported remote platforms:

- `pi-darwin-arm64.tar.gz`
- `pi-darwin-x64.tar.gz`
- `pi-linux-arm64.tar.gz`
- `pi-linux-x64.tar.gz`
- `pi-windows-arm64.zip`
- `pi-windows-x64.zip`

Only Linux libc targets verified to run the published standalone binaries are registered. Unsupported targets fail before download with the existing `no pinned Pi release supports this remote target` error.

Each release entry pins:

- The official GitHub release URL.
- The archive SHA-256 from the upstream `SHA256SUMS` file.
- The SHA-256 of the extracted executable bytes.
- The executable's archive path and installed name.
- The exact accepted `pi --version` output for v0.81.1.

Add a ZIP artifact variant to the existing artifact cache. ZIP extraction must select one exact configured entry, reject absolute paths and parent traversal, enforce the existing artifact size limit, and write only to the unique partial destination. TAR extraction retains its current behavior. Both formats verify the archive before extraction and the executable after extraction.

Flint-managed Pi commands set:

```text
PI_SKIP_VERSION_CHECK=1
PI_TELEMETRY=0
```

Flint controls the managed version and does not need Pi's startup update check or install telemetry. `PI_OFFLINE` is not set because Pi still needs its normal model-catalog and provider behavior.

## Tunneled Network Policy

Pi's tunneled egress policy is a static allowlist for the pinned release. It contains:

- API and OAuth hosts used by the built-in providers shipped with Pi v0.81.1.
- `pi.dev` hosts needed by Pi's built-in metadata services.
- Any built-in model-catalog hosts contacted during normal interactive startup.

The implementation derives and documents the concrete host set from Pi v0.81.1's built-in provider definitions. Tests pin the resulting set so additions are reviewed deliberately. Redirect targets remain subject to the same policy.

Project `models.json`, custom-provider extensions, installed packages, and other extensions do not widen the allowlist dynamically. If they request another host, the tunneled proxy returns its existing blocked-destination error. Users who need arbitrary destinations configure that SSH connection with the direct agent route, where the remote machine's normal network policy applies.

## Error Handling

- Unsupported platforms fail with a user-visible error before provisioning begins.
- Download, redirect-policy, checksum, extraction, upload, permission, and remote version failures remain in the managed-agent progress flow and prevent execution.
- Corrupt cached sources or executables are removed and reacquired through the existing cache behavior.
- History errors are isolated per file; a bad session never suppresses valid sibling sessions.
- Provider authentication errors remain visible in Pi's terminal, where `/login` can resolve them.
- Blocked custom-provider destinations report the tunneled proxy error in Pi's terminal and direct the user toward a direct route.

Async provisioning and scanning errors continue propagating to the existing UI error surfaces. No fallible operation is silently discarded.

## Test Strategy

Implementation proceeds in test-first slices.

### Registry and settings

- Pi is registered after Codex and Claude with the expected capabilities.
- Codex and Claude retain their existing credential and usage policies.
- Pi's default and customized command settings resolve correctly.
- Missing Pi settings remain backward compatible.

### Launch and panel behavior

- New Pi threads include a generated `--session-id` and track that ID.
- Resume commands use `--session`, the session ID, and the project working directory.
- The panel orders Pi third and respects `hidden`.
- Pi has no usage query, credential entries, or launch-option menu.
- Live and historical Pi threads deduplicate correctly.
- Pi sessions participate in restart restoration.

### History

- Parse valid v1, v2, and v3-compatible headers and known entries.
- Prefer the latest session name over the first user-message title.
- Parse string and text-block user content.
- Select the greatest valid timestamp.
- Match POSIX and Windows project paths correctly.
- Reject colliding encoded directories whose header `cwd` does not match.
- Skip malformed records and invalid files without losing valid siblings.
- Enforce the per-project scan bound.

### Artifacts and remote execution

- Match every supported Pi release to the correct remote platform.
- Accept the pinned `pi --version` format and reject other versions.
- Extract the configured executable from TAR and ZIP fixtures.
- Reject ZIP traversal, wrong entries, oversize content, and checksum mismatches.
- Reuse verified cached executables.
- Apply Pi's managed environment without changing configured direct launches.
- Pin the complete built-in-provider egress host set.
- Surface unsupported targets and provisioning failures.

## Verification

Before pushing Rust changes:

1. Run focused `agent_threads` tests during each slice.
2. Run the affected settings and UI tests.
3. Run `cargo fmt --all -- --check` and fix any drift.
4. Run `./script/clippy` where practical for the completed change.
5. Build the local app bundle when UI behavior is ready for manual verification, accounting for the documented debug-bundle copy fallback.

## Acceptance Criteria

- Pi is visible by default beside Codex and Claude.
- A user can start, close, find, resume, and restore Pi threads for local projects.
- The same thread behavior works for direct remote projects using the configured remote `pi`.
- A supported tunneled remote project can acquire a verified managed Pi binary and launch it without a preinstalled runtime.
- Pi can reach every built-in provider covered by the pinned allowlist through the tunnel.
- Pi authentication and provider selection remain entirely inside Pi.
- Pi produces no Flint quota requests or provider-specific credential controls.
- Existing Codex and Claude behavior and existing user settings remain unchanged.
