# Collaboration and User Management Cleanup Design

## Status

Approved on 2026-07-15.

## Summary

Remove the remaining collaboration, Flint/Zed account, user-management, and
telemetry-upload code from Flint while preserving local editing, SSH remote
editing, updates, extensions, and Agent Threads plan usage.

The cleanup will be implemented as independently buildable slices. Surviving
features will depend directly on HTTP, local telemetry-free reliability
services, or SSH's `RemoteClient`/`ProtoClient` path instead of the inherited
cloud `Client` and `UserStore`.

Flint has no collaboration backend, account service, analytics backend, or
crash-reporting endpoint. The final application must not initialize or retain
code that assumes those services exist.

## Context

Flint already removed the collaboration, call, channel, and collaboration UI
crates, followed by the visible account sign-in feature. Significant inherited
infrastructure remains in the active build graph:

- `client::Client` owns an HTTP client, telemetry, Flint/Zed credentials, a
  cloud WebSocket connection, RPC handlers, and reconnection state.
- `client::UserStore` owns users, contacts, organizations, plans, participant
  indices, account state, and cloud notification integration.
- `AppState`, `Project`, `Workspace`, title bar, onboarding, previews, and tests
  construct or carry `Client` and `UserStore` even for local and SSH projects.
- `Project` retains shared-project join/share logic and collaborator state.
- `Workspace` retains collaborator presence and following behavior.
- settings, actions, UI components, protocol messages, and documentation still
  describe removed collaboration and account features.
- telemetry records usage events for an inherited `zed.dev` upload path, while
  reliability code can upload crash dumps to an inherited endpoint.

SSH remote editing does not require the collaboration WebSocket client. Its
active path is:

```text
SSH transport -> RemoteClient -> rpc::ProtoClient -> remote project stores
```

Agent Threads plan usage also does not require Flint user management. It reads
credentials created by the local Codex and Claude CLIs and queries their
provider usage endpoints.

## Goals

- Remove all Flint/Zed account, contact, organization, plan, billing, and cloud
  notification behavior.
- Remove shared-project collaboration, collaborator presence, and remote-user
  following behavior.
- Remove the cloud WebSocket client and its sign-in, credential, connection,
  reconnection, and RPC lifecycle.
- Remove product-analytics and diagnostic-upload paths.
- Keep crash and hang information available locally for self-diagnosis.
- Preserve SSH remote editing, including password, key, agent, and askpass
  authentication.
- Preserve application updates and remote-server downloads from Flint's GitHub
  releases.
- Preserve extension discovery, installation, updates, and extension-host
  operation against the intentional upstream Zed extension service.
- Preserve Agent Threads Codex and Claude plan-usage display.
- Remove stale actions, settings, UI, crates, dependencies, protocol types, and
  documentation after their consumers are gone.
- Keep every implementation slice buildable and testable.

## Non-goals

- Do not remove SSH remote editing or the generic RPC infrastructure it uses.
- Do not remove intentional upstream extension service URLs or rename the
  external Zed extension API and WIT namespaces.
- Do not remove Agent Threads or make Flint manage Codex or Claude accounts.
- Do not add a Flint account, collaboration, analytics, or crash-reporting
  replacement service.
- Do not add placeholder abstractions for hypothetical future collaboration.
- Do not redesign unrelated local project, editor, language server, terminal,
  debugger, or extension behavior.
- Do not automatically mutate or delete obsolete credentials already stored in
  a user's operating-system keychain.

## Architectural Decision

Use staged functional removal. Each stage removes one coherent responsibility
and rewires surviving callers to the narrow dependency they use. This reaches
the complete cleanup while keeping regressions attributable to a small change.

A surface-only cleanup was rejected because retaining skeletal `Client` and
`UserStore` types would preserve the misleading architecture. A one-shot
removal was rejected because it would combine application networking, project
construction, SSH RPC, workspace behavior, protocol pruning, and documentation
in a single difficult-to-review change.

## Target Architecture

### Application networking

Startup creates the proxy-aware, Flint-user-agent HTTP client and registers it
with GPUI. Consumers accept the narrow HTTP type they require:

- auto-update accepts `Arc<dyn HttpClient>` and uses Flint's GitHub releases;
- extension hosting derives its upstream `HttpClientWithUrl` from the generic
  HTTP client and `UPSTREAM_ZED_EXTENSION_SERVER_URL`;
- node runtime and other download consumers accept the generic HTTP client;
- no application-global cloud session object owns HTTP.

Move the shared proxy setting out of `client` into the settings boundary used
by both the desktop application and remote server. Preserve explicit proxy
configuration, environment proxy fallback, whitespace handling, and invalid
URL logging.

The legacy `server_url` setting is not needed for updates or extensions.
Extensions already have an explicit upstream service constant, and updates use
GitHub. Remove `server_url`, `credentials_url`, and their settings UI after all
cloud-client consumers are deleted.

Release-note links must no longer rely on the cloud client's base URL. They
resolve to the corresponding Flint GitHub release or tag.

### SSH remote editing

SSH remains isolated behind its existing remote boundary:

```text
SSH connection
  -> SSH transport and askpass
  -> RemoteClient
  -> rpc::ProtoClient
  -> WorktreeStore, BufferStore, LspStore, GitStore, TaskStore, and settings
```

Local and SSH `Project` constructors will not accept `Arc<Client>` or
`Entity<UserStore>`. They will receive only their existing filesystem,
language, node runtime, HTTP, and remote-protocol dependencies.

The `rpc` and `proto` crates remain. Protocol messages used by the remote server
remain even if their names originated in collaboration code. Only messages
with no remaining local or SSH consumer may be removed.

### Agent Threads plan usage

Keep the existing provider-owned credential flow:

```text
Agent Threads
  -> local Codex or Claude CLI credential file
  -> provider usage endpoint
  -> plan-usage display
```

Flint does not create, refresh, revoke, or otherwise manage these accounts.
This path must not depend on the removed application `Client`, user store, or
telemetry code.

### Reliability and self-diagnosis

Keep detection and local artifacts, with no network upload:

```text
Crash -> compressed <session>.dmp + <session>.json in Flint's logs directory
Hang  -> Flint.log entries + hang-*.miniprof.json in the hang-traces directory
```

Remove:

- product-analytics event collection and upload;
- the `/telemetry/events` request path and checksum machinery;
- authenticated metrics ID and staff identity;
- minidump upload and inherited endpoint configuration;
- Sentry authentication and debug-symbol upload workflow steps that exist only
  to support a remote crash service;
- collection of SSH remote-server crash files for upload;
- telemetry log UI and settings that imply reports can be transmitted;
- stale documentation describing server-side telemetry.

Delete the `telemetry` and `telemetry_events` crates after their analytics call
sites are removed. Reliability keeps its local report types within the crash
and hang modules rather than depending on analytics event models.

Hang detection continues to log slow tasks and actions locally. It retains the
existing bounded hang-trace policy. Crash handling continues to write the dump
and JSON metadata locally. Crash metadata retains build version, release
channel, commit, panic details, and relevant system/GPU details, but not account
or telemetry identity.

No crash, hang, diagnostic, or analytics data is sent to `zed.dev` or any other
endpoint.

## Component Changes

### `client`

Remove `UserStore`, user/contact/organization/plan types and behavior, cloud
credentials, browser authentication, sign-out, cloud connection state,
reconnection actions, WebSocket establishment, collaboration message handlers,
collaboration deep-link parsing, and account-oriented tests. Command palette,
editor navigation, startup argument parsing, and the open listener will stop
treating `flint://channel` links as supported internal navigation.

First move surviving dependencies out of the concrete `Client`:

- callers receive HTTP directly;
- reliability receives only local crash/hang facilities;
- project and workspace SSH code receive `ProtoClient` from `RemoteClient`;
- test helpers construct the same narrow dependencies as production code.

Delete the `client` crate after its surviving HTTP inputs have moved to their
callers. A small source file may remain temporarily during an implementation
stage, but the completed change must not retain it as a generic replacement
service object.

### Application startup and `AppState`

Remove `client` and `user_store` from `AppState`. Do not construct `UserStore`,
install client connection actions, subscribe telemetry to user changes, or
observe cloud connection status at startup.

Store or register only the dependencies required by active application
features, including filesystem, languages, node runtime, session, workspace
store, and HTTP.

Remove startup work that updates a cloud base URL or reconnects a cloud client
when settings change.

### Auto-update and extensions

Auto-update will own or receive an HTTP client rather than `Arc<Client>`. Its
release discovery and binary downloads remain GitHub-backed. Tests will assert
the repository, release channel, asset name, and download behavior through a
fake HTTP client.

Extension initialization will receive generic HTTP and no telemetry object.
The extension store continues to create an upstream-specific client using
`UPSTREAM_ZED_EXTENSION_SERVER_URL`. Extension API models that remain necessary
for registry responses stay in `cloud_api_types`.

### User management and cloud notifications

Delete:

- `UserStore` and its event stream;
- contacts and contact requests;
- authenticated current-user state;
- organizations, configurations, plans, trials, billing state, and related
  feature-flag updates;
- participant indices and cached user lookup used only by collaboration;
- the cloud notification store and notification RPC handlers;
- title-bar account, organization, plan, contact, host-user, and user-menu UI;
- onboarding and component-preview parameters that only forward `UserStore`;
- account-specific test fixtures and dependencies.

Generic local toasts and application notifications remain. Keep the
`notifications` crate for `status_toast`, but delete its cloud-backed
`notification_store` module, account/contact notification storage, and
collaboration RPC handling. Remove the crate's `client` and `rpc` dependencies
once that module is gone.

### Project collaboration

Remove the project behaviors that require the cloud collaboration client:

- join shared project and construct project from join response;
- share and unshare project;
- collaborator maps and host lookup;
- participant/user lookup;
- leave-project messages and collaboration subscriptions;
- collaboration-specific branches in project stores and disabled debugger
  paths when they have no SSH equivalent.

Local project stores remain local. SSH project stores use the protocol client
provided by `RemoteClient`. Fields named for the old collaboration client will
be removed rather than renamed when their only purpose was collaboration.

### Workspace collaboration

Remove remote-user presence and following:

- follow-next-collaborator and stop-following actions;
- peer-based follower state, leader updates, participant colors, and borders;
- active-call and in-room integration;
- collaborator panes and join-project navigation;
- cloud connection/current-user render observers;
- workspace-store handlers that forward collaborator updates.

Remove the agent-specific branch of the follower framework with the remote-user
branches. It has no caller outside that framework, and Agent Threads
terminal/session management does not depend on collaborator following.

### Shared types and protocols

Prune `cloud_api_types` by reachability after account consumers are removed.
Keep extension registry request and response models. Remove organization, plan,
billing, and authenticated-user models with no remaining consumer.

Prune protobuf messages only after local and SSH consumers compile without
them. Message names containing `collab`, `user`, `peer`, or `remote` are not
sufficient evidence for deletion because SSH reuses inherited protocol
infrastructure.

External interfaces listed in the repository rules remain unchanged, including
`zed_extension_api`, `zed:api-version`, `zed:extension/*`, upstream extension
service URLs, `zed-industries` dependencies, and load-bearing `ZED_*`
environment variables unrelated to the removed cloud account and telemetry
paths.

### UI, settings, actions, and documentation

After code consumers are gone, remove:

- collaboration actions and default keybindings;
- `collaboration_panel`, `server_url`, `credentials_url`, telemetry upload, and
  account settings;
- collaboration-only avatar, facepile, and notification components when no
  other active feature consumes them;
- collaboration and account claims, navigation entries, redirects, and
  cross-links;
- migration and feature documentation that advertises removed collaboration,
  Flint accounts, hosted billing, Copilot/MCP/ChatGPT in-app authentication, or
  telemetry services;
- the orphaned OAuth callback crate, which is outside the active build graph
  and has no consumer.

Documentation must continue to describe intentional third-party authentication
such as SSH and provider-owned CLI login state where those features exist.
Existing documentation files must not be deleted. Pages whose feature has been
removed will become concise compatibility pages that state the limitation and
point to supported alternatives where useful; they will be removed from the
normal mdBook navigation.

Update self-debugging documentation to cover:

- `flint::OpenLog` and `flint::RevealLogInFileManager`;
- the correct Flint log locations on macOS, Linux, and Windows;
- the hang-trace directory and `hang-*.miniprof.json` files;
- local minidump and adjacent JSON metadata files;
- minidump decompression and local inspection requirements;
- the need for matching symbols for fully symbolized native backtraces;
- SSH remote-server log and crash-artifact locations;
- the fact that Flint does not upload these files.

## Error Handling

- Update and extension HTTP failures continue to propagate through their
  existing user-visible status or prompt surfaces.
- SSH authentication, transport, remote-server download, and RPC failures
  continue to propagate to the remote-connection UI.
- Local crash or hang artifact write failures are logged with their context.
- Retired collaboration links are rejected with a clear unsupported message
  rather than attempting a cloud connection or silently doing nothing.
- Removed cloud operations are deleted instead of returning success from
  placeholder no-op implementations.
- Existing fallible operations continue to propagate errors or log intentional
  failures according to repository error-handling rules.

## Implementation Sequence

### 1. Lock down preserved behavior

Add or strengthen regression tests for:

- GitHub-backed update discovery and downloads;
- upstream extension registry URL construction and extension installation;
- local and SSH project construction;
- representative SSH remote-editing requests;
- Agent Threads Codex and Claude plan usage;
- local crash metadata and hang-trace creation.

Record the targeted test commands and confirm the baseline passes before
production changes.

### 2. Decouple application networking and remove uploads

Pass HTTP directly to auto-update, extensions, node runtime, project consumers,
and other surviving callers. Replace release-note base-URL construction with a
Flint GitHub release URL.

Remove usage analytics, telemetry upload, account telemetry identity, minidump
upload, remote crash collection, and associated endpoint configuration. Keep
local crash and hang output and update its tests.

### 3. Remove user management

Remove `UserStore`, account/organization/contact/plan behavior, cloud
notifications, title-bar account UI, and forwarding parameters in startup,
workspace, onboarding, previews, examples, benchmarks, and tests.

Compile and test local and SSH project creation before proceeding.

### 4. Remove project collaboration

Remove shared-project entry points, collaborator state, cloud entity
subscriptions, and collaboration-only store branches. Make local and SSH
constructors depend only on their actual services.

Run project, remote-server, editor, and workspace tests after this slice.

### 5. Remove workspace collaboration

Remove presence, remote-user following, active-call integration, participant
rendering, collaborator actions, and workspace message forwarding. Retain only
independently active agent behavior.

Run workspace and UI tests after this slice.

### 6. Prune the graph and documentation

Use compiler errors, reverse-dependency inspection, and repository searches to
remove unused crates, dependencies, settings, actions, UI components,
`cloud_api_types`, and protocol definitions.

Update the user documentation and mdBook navigation to match the final product.
Replace obsolete feature descriptions with concise compatibility pages rather
than deleting existing documentation files or preserving instructions Flint
cannot perform.

## Verification

Each slice must run the narrowest relevant tests before moving to the next
slice. Final verification includes:

- update tests using a fake HTTP client;
- extension registry, install, and update tests;
- local project tests;
- SSH connection and remote-editing integration tests;
- remote-server tests;
- Agent Threads plan-usage tests;
- crash metadata and hang-trace tests;
- documentation preprocessing, link/action validation, and Prettier checks;
- `cargo fmt --all -- --check`;
- `./script/clippy`;
- the relevant workspace test suite;
- `./script/bundle-tmp-app`, including manual copying of the fresh debug bundle
  if the known final signing/gzip step fails.

Static verification must also confirm:

- no `UserStore` construction or forwarding remains;
- no account, contact, organization, plan, or cloud notification code remains;
- no shared-project join/share or remote-user following entry point remains;
- no analytics or diagnostic upload endpoint remains;
- no `ZED_CLIENT_CHECKSUM_SEED` or `ZED_MINIDUMP_ENDPOINT` dependency remains;
- no release workflow requires Sentry credentials or uploads symbols to a
  remote crash service;
- update and extension HTTP paths remain present;
- SSH `RemoteClient`/`ProtoClient` paths remain present;
- Agent Threads provider-usage paths remain present.

## Acceptance Criteria

- Flint starts without constructing a cloud client, user store, cloud
  notification store, or telemetry uploader.
- Local projects open and operate normally.
- SSH projects connect, authenticate, download the matching remote server, and
  perform representative editing operations.
- Updates are discovered and downloaded from Flint's GitHub releases.
- Extensions can be discovered, installed, updated, loaded, and removed using
  the intentional upstream extension service.
- Agent Threads continues to display Codex and Claude plan usage from existing
  CLI credentials.
- Crashes create local dump and metadata files; hangs create local log entries
  and traces.
- Flint sends no analytics, crash, hang, or diagnostic data to any endpoint.
- No Flint/Zed account, contact, organization, billing, collaboration, or cloud
  notification surface remains.
- Documentation describes the preserved features and local debugging workflow
  without advertising removed services.
- Formatting, clippy, targeted tests, documentation checks, and the local app
  bundle verification pass.

## Risks and Mitigations

### Shared RPC ancestry

SSH reuses types and messages inherited from collaboration. Deleting by name
could break remote editing. Prune only after checking live Rust call sites and
running SSH integration tests.

### Broad constructor coupling

`Client` and `UserStore` are forwarded through many constructors and test
helpers. Remove one dependency boundary at a time, compile immediately, and do
not replace them with a broad application-services container.

### Hidden update and extension coupling

Both features currently obtain HTTP through `Client`. Add direct HTTP
regression tests before removing the client and keep their endpoints explicit.

### Local diagnostic discoverability

Removing uploads can leave useful artifacts undiscoverable. Preserve log
actions, log exact artifact paths, and document the platform-specific
locations and analysis steps.

### Documentation breadth

Collaboration, accounts, hosted services, and telemetry are referenced across
feature, migration, business, AI, and development pages. Use link and action
validation after removal so no dead navigation or misleading cross-link
remains.
