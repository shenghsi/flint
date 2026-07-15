# Collaboration and User Management Cleanup Implementation Plan

## Summary

Remove Flint's inherited collaboration client, account and organization state,
cloud notifications, analytics, and crash uploads. Keep the code paths that are
still part of Flint: local editing, SSH remote editing, updates from Flint's
GitHub releases, the upstream Zed extension registry, provider-owned Codex and
Claude authentication used by Agent Threads, and local crash and hang
artifacts.

The work is deliberately staged. Each stage first locks down the behavior it
must preserve, then removes one dependency boundary, and ends with focused
tests before the next stage begins.

## Non-Negotiable Boundaries

- Do not rename or replace `zed_extension_api`, Zed WIT namespaces,
  `UPSTREAM_ZED_EXTENSION_SERVER_URL`, or other intentional upstream extension
  interfaces.
- Do not remove `remote::RemoteClient`, `rpc`, `proto`, SSH transport,
  askpass, remote-server download, or the store protocols used by SSH remote
  editing.
- Do not make Flint own Codex or Claude credentials. Agent Threads continues to
  read provider CLI state and query provider usage endpoints.
- Do not upload analytics, crash dumps, hang reports, debug symbols, or remote
  crash artifacts to `zed.dev`, Sentry, or another endpoint.
- Keep `Flint.log`, compressed local minidumps, adjacent crash metadata, the
  local input-latency report, and bounded `hang-*.miniprof.json` traces.
- Keep generic workspace notifications and `notifications::status_toast`.
  Remove only the cloud-backed notification store.
- Do not prune a protobuf message because its name sounds collaborative. Remove
  it only after a usage search and the SSH remote-editing tests prove that it
  has no remaining consumer.

## 1. Make Surviving Shared Dependencies Explicit

Files:

- Modify `crates/settings/src/settings.rs`
- Modify `crates/http_client/src/http_client.rs`
- Modify `crates/system_specs/src/system_specs.rs`
- Modify `crates/install_cli/src/register_flint_scheme.rs`
- Modify `crates/project/src/project.rs`
- Modify `crates/project/src/git_store.rs`
- Modify `crates/project/src/trusted_worktrees.rs`
- Modify `crates/remote_server/src/headless_project.rs`
- Modify `crates/remote_server/src/server.rs`
- Modify Rust files that import `proto`, `TypedEnvelope`, `ErrorCode`,
  `ErrorExt`, `SessionId`, or related RPC types through `client`
- Modify the corresponding `Cargo.toml` files

Write or move tests first:

- Move the proxy trimming test out of `client` and extend it to cover a valid
  explicit URL, blank input, invalid input, and environment fallback.
- Add a test that the explicit proxy takes precedence over the environment and
  that invalid explicit input is logged before falling back.
- Add a URL-scheme test that registers `flint` without importing a cloud
  client constant.
- Add compile-level coverage for the project-owned `ProjectId` in the remote
  git and trusted-worktree paths.

Run the new focused tests before moving implementation and confirm they fail
because the helpers or types still live in `client`.

Implement:

- Move raw `ProxySettings` registration into `settings`.
- Add one HTTP-layer proxy resolver that trims explicit input, logs invalid
  URLs, and falls back to the existing proxy environment variables. Use it in
  desktop startup and the remote server.
- Move the operating-system name and version helpers used by system
  information and feedback into `system_specs`; they are not telemetry.
- Define a Flint-owned `FLINT_URL_SCHEME` next to URL-scheme registration.
  Preserve all supported non-collaboration `flint://` routes.
- Move `ProjectId` to the `project` boundary and update git-store,
  trusted-worktree, and remote-server imports.
- Replace every use of `client` as an RPC facade with the canonical `rpc`,
  `proto`, or `util` import. Do not change the underlying protocol behavior in
  this step.
- Remove `client` dependencies from crates that used only its re-exports.

Validation:

```sh
cargo test -p http_client
cargo test -p settings
cargo check -p install_cli -p project -p remote_server
cargo metadata --no-deps --format-version 1 > /tmp/flint-metadata.json
```

Expected result: proxy behavior is unchanged, the Flint URL scheme still
registers, SSH-owned types no longer come from `client`, and the remaining
`client` dependencies represent real cloud-client consumers.

## 2. Decouple Auto-Update from the Cloud Client

Files:

- Modify `crates/auto_update/src/auto_update.rs`
- Modify `crates/auto_update/Cargo.toml`
- Modify `crates/auto_update_ui/src/auto_update_ui.rs`
- Modify `crates/auto_update_ui/Cargo.toml`
- Modify `crates/http_client/src/github.rs`
- Modify `crates/flint/src/main.rs`

Write tests first:

- Update the existing fake-HTTP auto-update test so initialization accepts
  `Arc<dyn HttpClient>` and no `Client`.
- Assert stable and preview release discovery requests
  `repos/shenghsi/flint/releases` and selects the expected prerelease flag.
- Assert nightly uses the `nightly` tag and stable version lookups use a `v`
  tag.
- Assert app and remote-server asset names remain channel, OS, and architecture
  specific.
- Add release-note URL tests for stable, preview, nightly, and dev. No expected
  URL may use the former cloud base URL or `zed-industries/flint`.
- Add a fake GitHub response test for local release-note rendering. Extend the
  GitHub release model with the fields required for the title and Markdown
  body rather than calling `/api/release_notes/v2`.

Run:

```sh
cargo test -p auto_update
cargo test -p auto_update_ui
```

Confirm the revised tests fail while `AutoUpdater` and the local release-note
view still require `client::Client`.

Implement:

- Store only the HTTP interface in `AutoUpdater`.
- Build download requests directly through `HttpClient`, preserving redirect
  handling, response status checks, progress, and user-visible errors.
- Keep release discovery and binary downloads on `shenghsi/flint` GitHub
  releases.
- Fetch local release-note Markdown from the GitHub release response. Keep the
  browser fallback for errors.
- Point browser release-note URLs at the matching Flint GitHub release, tag,
  or commit history.
- Pass GPUI's registered HTTP client into auto-update initialization.
- Remove `client` and telemetry dependencies from both update crates.

Validation:

```sh
cargo test -p auto_update
cargo test -p auto_update_ui
```

Expected result: checking, downloading, installing, remote-server downloading,
and release-note display all work with a fake generic HTTP client and no cloud
session.

## 3. Decouple Extensions While Preserving the Upstream Registry

Files:

- Modify `crates/extension_host/src/extension_host.rs`
- Modify `crates/extension_host/src/headless_host.rs`
- Modify `crates/extension_host/src/extension_store_test.rs`
- Modify `crates/extension_host/benches/extension_compilation_benchmark.rs`
- Modify `crates/extension_host/Cargo.toml`
- Modify `crates/extensions_ui/src/extensions_ui.rs`
- Modify `crates/extensions_ui/Cargo.toml`
- Modify `crates/flint/src/main.rs`

Write tests first:

- Keep and strengthen `extension_http_client_uses_upstream_zed_registry` so it
  proves a differently configured application HTTP client still produces
  `https://api.zed.dev/extensions`.
- Add fake-HTTP assertions for extension list, download, install, update, and
  remove operations where coverage is missing.
- Assert extension initialization has no telemetry parameter or event side
  effect.
- Keep the existing extension-store reload and dev-extension tests green.

Run the focused test before changing the constructor:

```sh
cargo test -p extension_host extension_http_client_uses_upstream_zed_registry
cargo test -p extension_host extension_store
```

Implement:

- Change extension initialization and `ExtensionStore::new` to accept generic
  HTTP inputs and no `Client` or `Telemetry`.
- Derive the registry-specific `HttpClientWithUrl` from the generic client and
  `UPSTREAM_ZED_EXTENSION_SERVER_URL`.
- Remove extension install/update analytics while keeping operation events
  used by the UI.
- Update headless hosts, tests, examples, and benchmarks to construct the
  narrow dependencies.
- Keep extension registry response models in `cloud_api_types`.

Validation:

```sh
cargo test -p extension_host
cargo check -p extensions_ui -p extension_cli
```

Expected result: extensions can still be discovered, installed, updated,
loaded, and removed, and the intentional upstream Zed compatibility boundary
is explicit.

## 4. Make Reliability Local-Only and Remove Analytics

Files:

- Modify `crates/crashes/src/crashes.rs`
- Modify `crates/crashes/Cargo.toml`
- Modify `crates/flint/src/reliability.rs`
- Modify `crates/flint/src/reliability/hang_detection.rs`
- Delete `crates/flint/src/reliability/hang_detection/telemetry.rs`
- Modify `crates/flint/src/reliability/hang_detection/task_traces.rs`
- Modify `crates/input_latency_ui/src/input_latency_ui.rs`
- Modify `crates/input_latency_ui/Cargo.toml`
- Delete `crates/flint/src/flint/telemetry_log.rs`
- Delete `crates/project/src/telemetry_snapshot.rs`
- Modify all Rust call sites of `telemetry::event!`
- Delete `crates/telemetry`
- Delete `crates/telemetry_events`
- Modify `Cargo.toml` and affected crate manifests
- Modify `script/bundle-linux`
- Modify `script/bundle-mac`
- Modify `script/bundle-windows.ps1`
- Modify `.github/workflows/release.yml`
- Modify `.github/workflows/release_nightly.yml`
- Modify `.github/workflows/run_bundling.yml`
- Modify `.github/workflows/nix_build.yml`
- Modify generated-workflow sources under
  `tooling/xtask/src/tasks/workflows/`

Write tests first:

- Extract a fallible crash-artifact persistence helper and test that it writes
  compressed `<session>.dmp` and adjacent `<session>.json` metadata in a
  temporary logs directory.
- Test that crash metadata contains version, binary, release channel, commit,
  panic, and system/GPU fields but no account, staff, metrics, installation, or
  upload identity.
- Add a hang-trace cleanup test proving the directory retains at most three
  newest trace files and ignores unrelated files.
- Keep input-latency formatting tests while deleting only its background
  telemetry baseline and sender.
- Add a static workflow/script check, or equivalent assertions in the final
  verification script, for forbidden minidump endpoint and Sentry symbol
  upload configuration.

Run the focused tests and confirm the new local-only crash metadata expectation
fails while `UserInfo` and upload code remain.

Implement:

- Keep crash-handler IPC, minidump generation, compression, JSON metadata, and
  local panic/GPU context.
- Remove crash `UserInfo`, authenticated telemetry identity, upload scanning,
  upload deletion, multipart Sentry requests, and remote crash-file collection.
- Handle crash artifact compression, rename, serialization, and write failures
  explicitly. Log failures with paths and context instead of using `unwrap`,
  `.ok()`, or silently discarded results in the touched persistence path.
- Keep hang logging and trace generation. Delete periodic hang analytics and
  the telemetry flush on quit.
- Keep the command that renders an input-latency histogram locally; delete its
  background event reporting.
- Delete usage-event calls and telemetry-only models throughout editor,
  project, git, settings, onboarding, extension, REPL, and workspace crates.
- Delete the telemetry log action, toolbar item, menu item, and view.
- Delete telemetry settings and VS Code telemetry import rather than leaving
  no-op controls.
- Remove `ZED_CLIENT_CHECKSUM_SEED`, `ZED_MINIDUMP_ENDPOINT`, Sentry setup, and
  Sentry debug-symbol upload steps from application bundling and release
  workflows and their generator sources.
- Leave the separate Sentry crash-reading utilities untouched in this task;
  they do not upload application data. They can be removed later only if their
  legacy investigation workflow is retired separately.

Validation:

```sh
cargo test -p crashes
cargo test -p input_latency_ui
cargo check -p flint
rg -n 'telemetry::event!|/telemetry/events|ZED_MINIDUMP_ENDPOINT|ZED_CLIENT_CHECKSUM_SEED' crates script .github tooling
rg -n 'sentry-cli debug-files upload|SENTRY_AUTH_TOKEN' script/bundle-linux script/bundle-mac script/bundle-windows.ps1 .github/workflows/release.yml .github/workflows/release_nightly.yml .github/workflows/run_bundling.yml
```

Expected result: the tests pass and both searches return no application upload
configuration. Local crash files and hang traces remain on disk across
restarts.

## 5. Remove Project Collaboration Without Touching SSH Stores

Files:

- Modify `crates/project/src/project.rs`
- Modify `crates/project/src/buffer_store.rs`
- Modify `crates/project/src/git_store.rs`
- Modify `crates/project/src/lsp_store.rs`
- Modify `crates/project/src/task_store.rs`
- Modify `crates/project/src/toolchain_store.rs`
- Modify `crates/project/src/trusted_worktrees.rs`
- Modify `crates/project/Cargo.toml`
- Modify project constructors in `crates/recent_projects`,
  `crates/settings_ui`, `crates/component_preview`, `crates/git_ui`,
  `crates/project_benchmarks`, and `crates/remote_server`

Write preservation tests first:

- Extend local project construction tests so the constructor accepts HTTP,
  node runtime, languages, filesystem, environment, and flags without a cloud
  client or user store.
- Extend remote project construction coverage so it receives an
  `Entity<RemoteClient>` and its `ProtoClient` is used by remote worktree,
  buffer, LSP, git, task, toolchain, and settings stores.
- Keep the remote git and trusted-worktree request tests using the
  project-owned `ProjectId`.
- Run the basic remote-editing test before implementation to record a green
  baseline.

```sh
cargo test -p remote_server test_basic_remote_editing
```

Implement:

- Change `Project::local` and `Project::remote` to accept only their active
  dependencies. Remove `collab_client`, `user_store`, collaborator maps,
  join-response state, and cloud subscriptions.
- Remove `Project::in_room`, join-shared-project construction, share/unshare,
  leave-project, host lookup, collaborator update handlers, participant lookup,
  and collaboration-only events.
- Collapse the top-level project client state to local versus SSH remote state
  expressed by `RemoteClient` and the stores, not a cloud collaboration enum.
- Keep the individual store upstream/downstream protocol machinery used by the
  headless SSH server. A `shared`, `remote`, or `downstream` name is not enough
  reason to remove it.
- Keep remote disconnect/reconnect handling, worktree trust, debugger HTTP,
  terminal routing, language servers, tasks, and settings synchronization.
- Remove `client` from project and project-benchmark manifests once direct
  imports and constructors no longer need it.

Validation:

```sh
cargo test -p project --features test-support --test integration
cargo test -p remote_server test_basic_remote_editing
cargo test -p remote_server test_remote_settings
cargo test -p remote_server test_remote_lsp
```

Expected result: local projects and SSH projects work, while no project API can
join, share, unshare, or enumerate cloud collaborators.

## 6. Remove Workspace Presence, Calls, and Following

Files:

- Modify `crates/workspace/src/workspace.rs`
- Modify `crates/workspace/src/multi_workspace.rs`
- Modify `crates/workspace/src/item.rs`
- Modify `crates/workspace/src/pane.rs`
- Modify `crates/workspace/src/pane_group.rs`
- Modify `crates/workspace/src/dock.rs`
- Delete `crates/workspace/src/shared_screen.rs`
- Modify `crates/workspace/src/theme_preview.rs`
- Modify `crates/workspace/Cargo.toml`
- Modify `crates/editor/src/editor.rs`
- Modify `crates/editor/src/navigation.rs`
- Modify `crates/command_palette/src/command_palette.rs`
- Modify `crates/flint/src/flint/open_listener.rs`
- Modify default and Vim keymaps under `assets/keymaps/`

Write tests first:

- Add an open-listener test showing `flint://channel/...` is rejected as an
  unsupported retired collaboration link while file, SSH, extension, agent,
  settings, schema, git clone, and git commit links still route normally.
- Keep workspace pane, navigation, serialization, and close/save tests green
  without a global active-call provider.
- Add a focused action-registry or keymap validation assertion proving removed
  follow actions are not referenced.

Run the revised tests and confirm the channel-link expectation fails while
`parse_flint_link` still recognizes it.

Implement:

- Remove `AnyActiveCall`, `GlobalAnyActiveCall`, active-call events, room and
  channel operations, shared screens, participant locations, and remote-user
  presence.
- Remove follower/leader state, `CollaboratorId`, agent following, peer colors,
  pane borders, follow-next, and stop-following behavior.
- Remove collaborator item remote IDs and workspace handlers that send follow
  or location updates through the cloud client.
- Simplify `WorkspaceStore` so it tracks workspaces without a cloud client or
  collaboration subscriptions.
- Remove `parse_flint_link` from editor navigation, the command palette,
  startup argument classification, and the open listener. Preserve every
  non-collaboration Flint URL handled directly by the open listener.
- Remove the collaboration actions and default keybindings.
- Keep local multi-workspace, SSH window restore, pane layout, navigation,
  serialization, and generic notifications.

Validation:

```sh
cargo test -p workspace
cargo test -p flint open_listener
cargo check -p editor -p command_palette
cargo test -p remote_server test_basic_remote_editing
```

Expected result: workspaces have no remote-user or call state and SSH projects
still open and navigate normally.

## 7. Remove User, Organization, Plan, and Cloud Notification Consumers

Files:

- Modify `crates/title_bar/src/title_bar.rs`
- Delete `crates/title_bar/src/plan_chip.rs`
- Modify `crates/title_bar/src/title_bar_settings.rs`
- Modify `crates/title_bar/Cargo.toml`
- Delete `crates/notifications/src/notification_store.rs`
- Modify `crates/notifications/src/notifications.rs`
- Modify `crates/notifications/Cargo.toml`
- Modify `crates/onboarding/src/onboarding.rs`
- Modify `crates/onboarding/src/basics_page.rs`
- Modify `crates/onboarding/Cargo.toml`
- Modify `crates/component_preview/src/component_preview.rs`
- Modify `crates/component_preview/examples/component_preview.rs`
- Modify `crates/component_preview/Cargo.toml`
- Modify `crates/settings_ui/src/settings_ui.rs`
- Modify `crates/settings_ui/src/page_data.rs`
- Modify `crates/settings_ui/Cargo.toml`
- Modify `crates/flint/src/main.rs`

Write tests first:

- Keep a `StatusToast` component/render test to guard the local notification
  surface before deleting the cloud store.
- Add or update title-bar tests so SSH connection status and update controls
  render without current-user, plan, organization, host-user, or cloud
  connection state.
- Update onboarding, component-preview, and settings-editor test fixtures to
  construct without `UserStore`.
- Add a settings-editor test showing values come only from the selected
  settings file and are never disabled by an organization override.

Implement:

- Delete the cloud notification global, account/contact notification entries,
  RPC handlers, connection watcher, pagination, and user prefetch.
- Keep the `notifications` crate and `status_toast` module, and remove its
  `client`, `rpc`, `sum_tree`, and account-only dependencies.
- Remove account/cloud connection rendering, collaborator project host,
  organization selection, plan chips, user avatar menu, sign-in progress, and
  related title-bar settings. Preserve the SSH connection indicator,
  worktree/project controls, menus, and update status.
- Remove `UserStore` parameters and fields that onboarding, component preview,
  visual tests, and settings UI only forward.
- Delete settings organization-override machinery. All current generated
  setting fields use `None`, so remove the field and its mechanical
  initializers rather than replacing it with another abstraction.
- Remove cloud notification initialization from normal and restored app
  startup.

Validation:

```sh
cargo test -p notifications
cargo test -p title_bar
cargo check -p onboarding -p component_preview -p settings_ui
```

Expected result: local status toasts and app prompts work, while no UI reads a
current Flint user, organization, plan, contact, or cloud notification store.

## 8. Remove the Application Client and Prune the Build Graph

Files:

- Modify `crates/flint/src/main.rs`
- Modify `crates/flint/src/flint.rs`
- Modify `crates/workspace/src/workspace.rs`
- Modify `crates/agent_threads/src/panel.rs`
- Modify `crates/feedback/src/feedback.rs`
- Modify `crates/inspector_ui/src/inspector.rs`
- Modify remaining real `Client` consumers found by `cargo metadata` and `rg`
- Delete `crates/client`
- Delete `crates/oauth_callback_server`
- Delete orphaned `crates/livekit_api`
- Delete orphaned `crates/livekit_client`
- Modify `crates/cloud_api_types/src/cloud_api_types.rs`
- Delete unused account, plan, timestamp, internal API, and WebSocket modules
  from `crates/cloud_api_types/src/`
- Modify `crates/cloud_api_types/Cargo.toml`
- Modify `Cargo.toml` and `Cargo.lock`

Write tests first:

- Update `AppState::test` and startup-oriented fixtures to build from
  filesystem, languages, node runtime, session, workspace store, and the GPUI
  HTTP global without `Client` or `UserStore`.
- Keep Agent Threads plan-usage parsing and provider selection tests. Add an
  assertion that the path uses only Codex/Claude CLI credentials and the
  supplied generic HTTP client.
- Keep feedback system-information tests or add focused tests after moving OS
  helpers to `system_specs`.
- Run `cargo metadata` and save the pre-removal reverse dependency list so all
  actual consumers are accounted for.

Implement:

- Construct one proxy-aware `ReqwestClient` with the Flint user agent at
  startup and register it through GPUI. Do not wrap it in an application cloud
  session.
- Remove cloud client construction, global registration, reconnection actions,
  connection observation, credential handling, sign-in state, system and
  installation telemetry IDs, and server URL reload behavior.
- Remove `client` and `user_store` from `AppState` and all fixtures.
- Obtain generic HTTP from GPUI at the point where Agent Threads, feedback,
  inspector UI, node runtime, updates, extensions, and project construction
  need it.
- Delete `client` after `cargo metadata` reports no reverse dependency.
- Delete the unused OAuth callback server and the already-orphaned LiveKit
  crates.
- Reduce `cloud_api_types` to extension registry manifests, metadata, and
  responses. Keep its public extension types stable for extension CLI and host
  consumers.
- Regenerate `Cargo.lock` through `cargo check`; do not edit it manually.

Validation:

```sh
cargo test -p agent_threads plan_usage
cargo check -p flint -p remote_server -p extension_host -p auto_update
cargo metadata --no-deps --format-version 1 | jq -e '[.packages[].name] | index("client") | not'
cargo metadata --no-deps --format-version 1 | jq -e '[.packages[].name] | index("telemetry") | not'
cargo metadata --no-deps --format-version 1 | jq -e '[.packages[].name] | index("telemetry_events") | not'
```

Expected result: Flint's build graph has no application cloud client,
analytics crates, user store, OAuth callback server, or LiveKit collaboration
crates. Agent plan usage still passes with provider-owned login state.

## 9. Prune Protocols, Settings, Actions, and Collaboration-Only UI

Files:

- Modify `crates/proto/proto/app.proto`
- Modify `crates/proto/proto/flint.proto`
- Modify `crates/proto/src/proto.rs`
- Modify `crates/remote_server/src/server.rs`
- Modify `crates/settings_content/src/settings_content.rs`
- Modify `crates/settings/src/vscode_import.rs`
- Modify `crates/settings_ui/src/page_data.rs`
- Modify `assets/settings/default.json`
- Modify `crates/flint_actions/src/lib.rs`
- Modify `crates/ui/src/components.rs`
- Modify `crates/ui/src/components/avatar.rs`
- Create `crates/ui/src/components/update_button.rs` by moving the existing
  non-collaboration update component
- Delete `crates/ui/src/components/collab.rs`
- Delete `crates/ui/src/components/collab/collab_notification.rs`
- Delete `crates/ui/src/components/collab/update_button.rs` after the move
- Delete `crates/ui/src/components/facepile.rs` if no non-collaboration consumer
  remains
- Modify `crates/workspace/src/theme_preview.rs`
- Modify component stories and previews that reference removed UI

Test and search before deletion:

- For every candidate protobuf message, run an exact-symbol `rg` across
  `crates`, tests, and `.proto` files. Keep messages used by `RemoteClient`,
  `AnyProtoClient`, headless stores, or SSH tests.
- Add settings deserialization coverage showing old `server_url`,
  `credentials_url`, `telemetry`, and `collaboration_panel` keys are ignored as
  unknown/legacy input according to existing settings policy, while `proxy`
  remains supported.
- Keep base `Avatar` tests because git commit UI still uses it.
- Run the remote-editing suite before and after protocol generation changes.

Implement:

- Remove `GetCrashFiles`, cloud notification, user/contact/organization,
  collaboration join/share/follow, and other protocol messages only when the
  pre-deletion search shows no surviving local or SSH consumer.
- Remove the remote `GetCrashFiles` handler; remote dumps remain on the remote
  host for local inspection there.
- Remove `server_url`, `credentials_url`, telemetry upload controls,
  collaboration panel settings, user-menu settings, and corresponding Settings
  Editor entries.
- Remove stale collaboration and telemetry actions, action metadata, menus,
  and keymap contexts.
- Keep base avatars used by git history. Remove audio status, collaborator
  availability, facepile, and collaboration notification components once
  their workspace/theme previews are gone.
- Preserve `UpdateButton`, move it out of the collaboration module, and give
  its component story a status-oriented scope. The title bar still uses it for
  auto-update progress.
- Regenerate protobuf Rust code and action metadata using the repository's
  existing scripts when required.

Validation:

```sh
cargo test -p settings
cargo test -p ui
cargo test -p remote_server test_basic_remote_editing
./script/prettier
```

Expected result: configuration and UI describe the single-user product, and
protocol pruning does not change SSH behavior.

## 10. Rewrite Documentation Around Supported Behavior

Files:

- Modify `docs/src/SUMMARY.md`
- Modify all files under `docs/src/account/`
- Modify all files under `docs/src/collaboration/`
- Modify all files under `docs/src/business/`
- Modify `docs/src/roles.md`
- Modify `docs/src/telemetry.md`
- Modify `docs/src/development/debugging-crashes.md`
- Modify `docs/src/troubleshooting.md`
- Modify `docs/src/remote-development.md`
- Modify affected files under `docs/src/ai/`
- Modify affected migration guides under `docs/src/migrate/`
- Modify `docs/src/reference/all-settings.md` through the repository's
  generated-settings process when applicable

Implement:

- Remove Collaboration, Account & Billing, Flint Business, and Telemetry from
  normal `SUMMARY.md` navigation.
- Do not delete existing documentation files. Turn removed feature pages into
  concise compatibility pages stating that Flint has no collaboration
  backend, account, organization, plan, billing, hosted model service, or
  telemetry service.
- Remove stale cross-links and redirects that advertise those features.
- Audit AI docs for nonexistent in-app Copilot, ChatGPT, MCP, or Flint account
  authentication. Preserve and clearly describe provider-owned Codex and
  Claude CLI login state used by Agent Threads, API-key providers that really
  exist, local models, and extension-based MCP support that remains functional.
- Preserve SSH authentication and remote development instructions. Correct
  stale Zed product names and repository links when the text is about Flint.
- Rewrite `telemetry.md` as a local diagnostics and privacy page: Flint does
  not send analytics, crash dumps, hang reports, or diagnostics.
- Document `flint::OpenLog` and `flint::RevealLogInFileManager` and these
  default paths:
  - macOS logs: `~/Library/Logs/Flint/`
  - Linux logs: `$XDG_DATA_HOME/flint/logs/` or
    `~/.local/share/flint/logs/`
  - Windows logs: `%LOCALAPPDATA%\\Flint\\logs\\`
  - hang traces: the `hang_traces` directory under Flint's data directory
- Document compressed `<session>.dmp`, adjacent `<session>.json`, `zstd`
  decompression, `minidump-stackwalk`, and the need for matching build symbols
  for full native symbolization.
- Document that SSH remote-server logs, minidumps, and JSON metadata remain in
  the remote host's Flint data/log directory and are never collected by the
  desktop app.

Validation:

```sh
cd docs && npx prettier --write src/
cd docs && npx prettier --check src/
mdbook build ./docs --dest-dir=../target/deploy/docs/
```

Expected result: documentation accurately distinguishes supported third-party
authentication and SSH from removed Flint accounts, and gives users enough
information to inspect local failures themselves.

## 11. Final Verification and Static Audit

Run focused preservation suites:

```sh
cargo test -p auto_update
cargo test -p extension_host
cargo test -p agent_threads plan_usage
cargo test -p crashes
cargo test -p input_latency_ui
cargo test -p project --features test-support --test integration
cargo test -p workspace
cargo test -p remote_server test_basic_remote_editing
cargo test -p remote_server test_remote_settings
cargo test -p remote_server test_remote_lsp
```

Run static audits:

```sh
rg -n 'UserStore|ParticipantIndex|AnyActiveCall|FollowNextCollaborator|StopFollowing|flint://channel' crates assets
rg -n 'telemetry::event!|/telemetry/events|ZED_MINIDUMP_ENDPOINT|ZED_CLIENT_CHECKSUM_SEED' crates script .github tooling
rg -n 'sentry-cli debug-files upload|SENTRY_AUTH_TOKEN' script/bundle-linux script/bundle-mac script/bundle-windows.ps1 .github/workflows/release.yml .github/workflows/release_nightly.yml .github/workflows/run_bundling.yml
rg -n 'client\.workspace' Cargo.toml crates --glob 'Cargo.toml'
rg -n 'name = "(client|telemetry|telemetry_events)"' Cargo.lock
rg -n 'UPSTREAM_ZED_EXTENSION_SERVER_URL|zed_extension_api|zed:api-version|zed:extension/' crates/extension_host crates/extension_api
```

The first five searches must return no active-code remnants. The last search
must still find the intentional upstream extension compatibility identifiers.

Run repository checks:

```sh
cargo fmt --all -- --check
./script/clippy
./script/prettier
git diff --check
```

Build the local application bundle:

```sh
./script/bundle-tmp-app
```

Check the exit code. If the known debug remote-server signing step fails after
creating a fresh bundle, copy it explicitly as documented in the repository
instructions:

```sh
cp -R target/<target-triple>/debug/bundle/osx/Flint.app /tmp/Flint-Local.app
```

Smoke-test `/tmp/Flint-Local.app`:

1. Open a local folder and edit, save, search, run a task, and use git status.
2. Open an SSH project, edit and save a remote file, start a remote terminal,
   and reconnect once.
3. Check for updates and open release notes.
4. Browse, install, update, load, and remove an extension.
5. Open Agent Threads plan usage with existing Codex or Claude CLI login state.
6. Run the local input-latency report action.
7. Open and reveal `Flint.log`; verify a hang trace is written by the dev hang
   action in a debug build.
8. Confirm no network request targets `/telemetry/events`, a minidump endpoint,
   or Sentry during startup, shutdown, crash-artifact discovery, or hang
   detection.

## Acceptance Criteria

- The `client`, `telemetry`, and `telemetry_events` crates are absent from the
  workspace and dependency graph.
- No active code constructs a Flint user, user store, contact, organization,
  plan, billing state, cloud notification store, collaboration room, shared
  project, follower, or shared screen.
- Local editing and SSH remote editing pass their integration tests and manual
  smoke tests.
- Auto-update and release notes use `shenghsi/flint` GitHub releases through a
  generic HTTP client.
- Extensions still use the intentional upstream Zed registry and compatibility
  identifiers through a generic HTTP client.
- Agent Threads still reads provider-owned Codex or Claude login state and
  displays plan usage without Flint account management.
- Flint writes local logs, compressed minidumps, crash JSON, input-latency
  reports, and bounded hang traces, and never uploads them.
- Application bundling and release workflows contain no minidump endpoint,
  telemetry checksum, Sentry setup, or debug-symbol upload requirement.
- Generic local status toasts remain; cloud account/contact notifications are
  gone.
- Settings, actions, keymaps, title bar, onboarding, component previews, and
  docs no longer advertise removed collaboration, account, billing, hosted
  service, or telemetry controls.
- Documentation tells users where local and SSH diagnostics live and what
  symbols/tools are required for self-debugging.
