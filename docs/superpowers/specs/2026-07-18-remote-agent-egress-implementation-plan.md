# Remote Agent Egress and Provisioning Implementation Plan

## Summary

Implement the approved
[Remote Agent Egress and Provisioning Design](./2026-07-18-remote-agent-egress-design.md)
without replacing Flint's terminal-first Agent Threads model. Supported Codex
and Claude Code processes continue to run on the SSH host in the real remote
project. Flint may provision a pinned official executable through a verified
local download and SSH upload, and the user may independently choose whether
that agent's provider traffic goes `Through Flint` or `Not through Flint`.

The work is dependency-ordered and test-first. Each stage must leave ordinary
remote editing and the default `Not through Flint` route usable. Do not start a
later stage while a focused test from an earlier stage is failing.

## Non-Negotiable Boundaries

- A new SSH connection identity has no stored route and therefore opens as
  `Not through Flint` without a prompt. Connectivity probes and request errors
  never select or change the route.
- Provisioning and routing are independent. An ambient or explicitly
  configured agent can use either route, and a Flint-managed agent can use
  either route.
- Opening a project never downloads or installs an agent. Provisioning is lazy
  on an agent/login action or starts from an explicit management action.
- Flint-managed artifacts are exact official releases pinned by the current
  Flint release. Runtime settings cannot replace their source URL, digest,
  signature metadata, target, or version matcher.
- Download, manifest/signature verification, archive extraction when needed,
  and source-artifact digest verification happen locally. The remote host runs
  no vendor installer, package manager, download tool, or archive extractor.
- The normalized executable uploaded to the host has its own pinned digest.
  Flint verifies that digest locally and again through `remote_server` before
  committing the installation.
- Managed installations are per-user, require no `sudo`, do not modify a shell
  profile or general `PATH`, and are launched by absolute path.
- `Not through Flint` preserves today's ambient launch behavior. It creates no
  CONNECT proxy, reverse forward, or egress lease and injects no proxy
  environment. Only a managed executable receives self-update suppression on
  this route.
- `Through Flint` injects a process-scoped restricted proxy and self-update
  suppression into both ambient and managed agents. It is not represented as
  a host firewall.
- The OAuth browser-callback `-L` forward is independent of the egress session
  and is available under either route.
- The remote host owns the provider credential. Flint never reads or copies a
  credential store, terminates provider TLS, injects provider authorization,
  or persists a proxy capability.
- Keep raw SSH mechanics and generic remote operations in `remote`; keep agent
  catalogue, provisioning, endpoint, credential, and launch policy in
  `agent_threads`.
- Do not extend this change to ACP, a local agent for remote workspaces, an MCP
  file bridge, mirrored checkouts, or general remote internet access.

## Codebase Findings That Affect the Plan

The approved ownership split remains valid, but implementation support must
touch more than two crates:

- `crates/proto` and `crates/remote_server` carry the generic remote file and
  loopback-exchange RPCs used by `remote`.
- `crates/settings_content` and `crates/recent_projects` persist and render the
  route selected in the Remote Projects opener.
- `crates/paths` provides the local content-addressed artifact cache root.
- Policy and orchestration still live only in `agent_threads`; these support
  crates must not learn Codex or Claude endpoint policy.

There are two concrete gaps that must be resolved before provisioning:

1. `RemotePlatform` currently contains only `RemoteOs` and `RemoteArch`.
   `parse_platform` and every transport probe use `uname -sm`; there is no
   existing libc result to reuse. Extend this one shared target value rather
   than adding an Agent Threads-only probe. Failure to identify Linux libc must
   leave remote editing usable but make managed provisioning report an
   unsupported target.
2. `SshRemoteConnection::upload_file` exists only as a private helper for the
   remote-server binary. Promote a generic single-file upload contract instead
   of copying SFTP/SCP logic into Agent Threads.

Official distributions may be a raw executable or an archive. The catalogue
must therefore pin both the official source artifact and the normalized
installed executable. Extraction is local, path-traversal-safe, and accepts
only the one catalogue-named executable.

## 1. Define the Shared Remote Target and Pinned Release Contract

Files:

- Create `crates/agent_threads/src/agent_release.rs`
- Modify `crates/agent_threads/src/agent_threads.rs`
- Modify `crates/agent_threads/Cargo.toml`
- Modify `crates/remote/src/remote_client.rs`
- Modify `crates/remote/src/transport.rs`
- Modify `crates/remote/src/transport/ssh.rs`
- Modify `crates/remote/src/transport/wsl.rs`
- Modify `crates/remote/src/transport/docker.rs`
- Modify `crates/remote/src/transport/mock.rs`
- Modify the small `RemotePlatform` construction sites in
  `crates/remote_connection` and `crates/project_benchmarks` if the compiler
  identifies any after the target type changes

Write tests first:

- Extend the `transport.rs` parser tests with tagged fixtures for macOS,
  Windows, glibc Linux, musl Linux, shell-startup noise, an unknown Linux libc,
  x86_64, and aarch64.
- Assert that unknown libc does not make the base OS/architecture target
  unusable for remote-server selection.
- Add catalogue validation tests for duplicate `(agent, version, target)`
  entries, missing or malformed SHA-256 values, non-HTTPS sources, source URLs
  outside the agent's compiled official-source rules, unsupported archive
  paths, and an empty self-update suppression environment.
- Add fixture-based tests for every tolerant `--version` matcher. Accept known
  harmless formatting variation, but reject another version and another agent.
- Assert that a supported target resolves to exactly one release and an
  unknown OS, architecture, or Linux libc resolves to none.
- Assert that the catalogue distinguishes the source-artifact digest from the
  normalized executable digest when the source is an archive.

Run the focused tests and confirm they fail because libc and the catalogue do
not exist:

```sh
cargo test -p remote parse_platform
cargo test -p agent_threads agent_release
```

Implement:

- Derive `PartialEq`, `Eq`, and `Hash` for `RemotePlatform` and add an explicit
  Linux libc value such as `Glibc`, `Musl`, or `Unknown`. Non-Linux targets do
  not invent a libc.
- Replace the separate `uname -sm` parsing assumptions with one tagged POSIX
  target-probe format shared by SSH, WSL, and Docker. Keep OS/architecture
  parsing independent from libc parsing so unknown libc does not break remote
  editing.
- Add `RemoteConnection::platform()` and `RemoteClient::platform()` so
  Agent Threads consumes the target already owned by the connected transport.
  Update the mock to make its target deterministic.
- Keep remote-server asset naming based on OS and architecture. Adding libc to
  `RemotePlatform` must not change the existing Flint remote-server download
  URL or bundle name.
- Define immutable `AgentRelease`, `AgentArtifactFormat`, source-verification,
  target, version-matcher, executable-name, and self-update-environment data in
  `agent_release.rs`.
- Add methods on the existing `AgentKindDefinition` to resolve its release and
  version/environment policy. Do not create a second provider registry keyed
  separately from `agent_kind_registry()`.
- Populate no placeholder or unverified production entry. Before an entry is
  committed, record the exact official source, source digest, installed-byte
  digest, signature/manifest verification method when published, target, and
  version output fixture used to validate it.

Validation:

```sh
cargo test -p remote
cargo test -p agent_threads agent_release
cargo check -p remote_connection -p project_benchmarks
```

Expected result: Flint has one reusable remote target including libc, and the
compiled catalogue can reject an unsupported or unpinned release without doing
I/O.

## 2. Add Generic Remote File and Loopback Operations

Files:

- Create `crates/proto/proto/remote_management.proto`
- Modify `crates/proto/proto/flint.proto`
- Modify `crates/proto/src/proto.rs`
- Modify `crates/remote/src/remote_client.rs`
- Modify `crates/remote/src/transport/ssh.rs`
- Modify `crates/remote/src/transport/mock.rs`
- Modify `crates/remote/src/transport/wsl.rs`
- Modify `crates/remote/src/transport/docker.rs`
- Modify `crates/remote_server/src/headless_project.rs`
- Modify `crates/remote_server/src/remote_editing_tests.rs`
- Modify `crates/remote_server/Cargo.toml`

Write tests first:

- Add protocol request/response coverage for:
  - retrieving Flint's remote per-user application-data directory;
  - computing a file's SHA-256 digest;
  - creating a private directory tree;
  - setting a file executable for its user where the platform supports it;
  - atomically renaming a staged path without silently overwriting a target;
  - removing a file or directory with explicit recursive and
    ignore-if-missing flags; and
  - performing one bounded TCP exchange with a remote loopback port.
- Add real-temporary-directory unit tests around the handler helpers. Prove
  digest correctness, private POSIX permissions, rename collision behavior,
  recursive cleanup, and preservation of an existing destination on failure.
- Add a loopback TCP fixture that verifies request and response byte limits,
  rejects non-loopback targets, times out a stalled peer, and never waits on a
  remote shell utility.
- Extend `MockRemoteConnection` with recording hooks for single-file upload and
  make a test prove that the uploaded source and destination are exact.
- Add SSH SFTP/SCP command tests for paths containing spaces and shell
  metacharacters. The promoted file uploader must retain the current SFTP then
  SCP fallback and actionable stderr.

Run and confirm failure before adding messages and public operations:

```sh
cargo test -p proto
cargo test -p remote upload_file
cargo test -p remote_server remote_management
```

Implement:

- Give the new messages the next free envelope IDs, update the current-max
  marker, register them in `proto.rs`, and add request handlers next to
  `HeadlessProject::handle_read_remote_file`.
- Keep the RPC vocabulary generic. No message or handler may contain an agent
  kind, provider hostname, credential, release URL, or proxy capability.
- Restrict the TCP request to `127.0.0.1`/`::1` by accepting a port rather than
  an arbitrary hostname. Enforce compile-time maximum request/response sizes
  and a bounded timeout on the server even when the caller supplies smaller
  values.
- Use `remote_server` process APIs for digest, permissions, directory, rename,
  and removal. Do not invoke `sha256sum`, `chmod`, `mv`, PowerShell web cmdlets,
  or other remote shell tools for these operations.
- Promote `SshRemoteConnection::upload_file` through
  `RemoteConnection::upload_file` and `RemoteClient::upload_file`. Return a
  clear unsupported-operation error for transports not enabled for managed
  provisioning; do not pretend a no-op upload succeeded.
- Add narrow `RemoteClient` methods around the new RPCs so Agent Threads does
  not construct protocol envelopes or duplicate timeout limits.
- Propagate every transfer and handler error. Cleanup that cannot run because
  the connection is gone must be logged with the path but without secrets.

Validation:

```sh
cargo test -p proto
cargo test -p remote
cargo test -p remote_server remote_management
cargo test -p remote_server remote_editing_tests
```

Expected result: the local client can upload one verified file and ask the
already-running remote server to inspect and commit it without agent-specific
transport code or remote network tools.

## 3. Build the Verified Local Artifact Cache

Files:

- Modify `crates/paths/src/paths.rs`
- Modify `crates/agent_threads/src/agent_release.rs`
- Modify `crates/agent_threads/Cargo.toml`
- Modify supporting archive code in `crates/util` or `crates/http_client` only
  if the existing safe extractors cannot consume an already-verified local
  file

Write tests first:

- Use `FakeHttpClient` and a temporary cache root to prove that a cache miss
  downloads once, checks response status, bounds the download size, hashes
  while writing a unique partial file, and renames into the content-addressed
  path only after verification.
- Corrupt the response, source digest, manifest signature, extracted path,
  installed-byte digest, and expected executable name independently. Each case
  must fail before `RemoteClient::upload_file` can be called and remove its
  partial data.
- Redirect an official source to an unapproved host and exceed the redirect
  bound. Both must fail; an explicitly catalogued official CDN redirect may
  succeed only when every hop satisfies that agent's compiled source rule.
- Cover raw executable, tar/gzip, and zip normalization only when a pinned
  production source requires them. Reject absolute paths, `..`, links, extra
  executable candidates, and catalogue paths not present in the archive.
- Reuse a valid cache entry without another HTTP request. Re-hash an existing
  entry before trusting it; delete and reacquire one that drifted.
- Start two concurrent acquisitions for the same source digest and prove they
  share one task and produce one final cache entry.
- Assert that cache paths and logs contain digests and public artifact names,
  never provider credentials or proxy capabilities.

Run and confirm the cache tests fail:

```sh
cargo test -p agent_threads artifact_cache
```

Implement:

- Add a dedicated cache root below `paths::data_dir()` and keep artifacts
  addressed by the pinned source digest, not by an untrusted URL filename.
- Split acquisition behind a narrow interface used later by
  `ManagedAgentProvisioner`; production uses Flint's `HttpClient`, while tests
  use an in-memory fake.
- Validate the compiled official-source rule before issuing the request.
- Disable uninspected follow-all redirects. Follow only a small bounded chain
  whose every `Location` is parsed and accepted by the compiled official-source
  rule before the next request is sent.
- Verify a provider signature or signed manifest when the selected release
  declares one, and always verify the pinned source SHA-256.
- Normalize the one catalogued executable into another content-addressed path
  and verify its installed-byte digest. Never run the normalized local binary;
  its target can differ from the local machine.
- Use unique partial paths, flush/sync before rename where supported, and leave
  an older valid cache entry untouched on a failed acquisition.
- Integrate with Flint's bounded cache cleanup policy. Cleanup failures are
  warnings; verification and acquisition failures remain launch errors.

Validation:

```sh
cargo test -p agent_threads artifact_cache
cargo check -p agent_threads
```

Expected result: a caller receives only a locally normalized executable whose
source provenance and installed bytes match the signed Flint catalogue.

## 4. Implement Transactional Managed Provisioning

Files:

- Create `crates/agent_threads/src/managed_agent.rs`
- Modify `crates/agent_threads/src/agent_threads.rs`
- Modify `crates/agent_threads/src/store.rs`
- Modify `crates/agent_threads/Cargo.toml`

Write tests first:

- Define fake `AgentArtifactSource` and `RemoteAgentHost` boundaries, then test
  the provisioner without SSH or GPUI terminals.
- Return an existing managed installation only when receipt kind, version,
  target, installed digest, absolute path, remote digest, and tolerant
  `--version` output all match.
- Treat a missing receipt, malformed receipt, symlink/path escape, digest
  drift, missing executable, wrong target, and wrong version output as invalid.
- Prove local verification completes before upload and remote digest
  verification completes before executable permissions or commit.
- Inject failure at directory creation, upload, remote digest, permission,
  receipt write, version check, staging rename, rollback rename, and cleanup.
  The prior verified version must remain selectable in every failure case.
- Share one in-flight installation task for the same connection, agent,
  version, and target. Different connections or targets must not share a remote
  installation task.
- Install a newer pinned version beside the old version and keep a live-use
  guard on the old path. New launches switch only after full verification.
- Test explicit removal: prevent new managed selections first, require live
  users to close, delete only Flint-managed paths and receipts, and preserve
  credential/history directories and ambient executables.
- Test POSIX and Windows path construction using the remote application-data
  directory returned by the server. No expected path may use a locally derived
  home directory.

Run and confirm failure before writing the provisioner:

```sh
cargo test -p agent_threads managed_agent
```

Implement:

- Add one `ManagedAgentProvisioner` with injected artifact and remote-host
  interfaces. Keep download/cache concerns out of its transaction logic.
- Use `agents/<agent>/<version>/<target>/` below Flint's remote
  application-data directory (the platform-specific equivalent of
  `flint/agents/...`) and a unique sibling staging directory for each attempt.
- Upload the normalized executable and a non-secret receipt into staging.
  Verify the remote digest, set user-only execution permission on POSIX, and
  run the staged absolute path with `--version` before it becomes active.
- Commit by atomic directory rename. If an invalid same-version directory
  exists, move it to a unique rollback name first; restore it if the staged
  commit fails, and recover deterministic leftover staging/rollback states on
  the next attempt.
- Never put the managed directory on general `PATH`. Return an
  `ManagedAgentInstallation` containing the verified absolute executable path
  and a live-use guard.
- Persist only the receipt on the remote and a non-secret preference telling
  Agent Threads to use a managed installation when the user explicitly chose
  it. A custom configured command remains higher precedence than that
  preference.
- Store that preference in a dedicated `db::kvp` namespace keyed by a stable
  serialization of normalized SSH identity plus agent kind. Nickname,
  password, SSH arguments, and the selected traffic route must not fork the
  key. Removal clears the preference before deleting managed paths.
- Add explicit update and remove operations. Do not let an agent self-update
  mutate a managed version in place.

Validation:

```sh
cargo test -p agent_threads managed_agent
cargo test -p remote_server remote_management
```

Expected result: an offline SSH host can receive a verified executable through
Flint, and every failed transaction preserves the prior working state.

## 5. Persist and Render the Two Remote Agent Routes

Files:

- Modify `crates/settings_content/src/settings_content.rs`
- Modify `crates/recent_projects/src/remote_connections.rs`
- Modify `crates/recent_projects/src/remote_servers.rs`
- Modify `crates/recent_projects/src/remote_servers/filter.rs` only if route
  state becomes part of filtered row data
- Modify `crates/recent_projects/Cargo.toml`
- Modify `assets/settings/default.json`
- Modify `crates/agent_threads/src/agent_threads.rs`

Write tests first:

- Add serde/schema tests for exactly `not_through_flint` and
  `through_flint`. A missing value must deserialize to no stored override and
  resolve to `Not through Flint`.
- Add `RemoteSettings` tests that match the existing normalized SSH identity
  fields `(host, username, port)`, ignore nickname/runtime SSH fields, and do
  not apply an SSH route to WSL or Docker.
- Add Remote Projects model/render tests proving every SSH server/project row
  exposes one route control with exactly two values and visibly selects the
  effective default.
- Open a new identity without interacting with the route control and prove no
  prompt or settings write occurs.
- Open projects under both stored routes without using Agent Threads and prove
  the opener starts no download, installation, proxy, or SSH forwarding task.
- Change the route and prove only that identity's optional setting changes.
  Reopen it and prove the value remains selected.
- Add a regression test that a remote provider/network error does not write a
  different route or invoke a route-change callback.

Run and confirm failure:

```sh
cargo test -p settings_content remote_agent_route
cargo test -p recent_projects remote_agent_route
```

Implement:

- Add a shared serializable `RemoteAgentRoute` enum and an optional
  `agent_route` field to `settings::SshConnection`. Keep `None` distinct from an
  explicit `not_through_flint` so an untouched identity causes no settings
  write.
- Add a small identity-matching helper based on the same host/username/port
  semantics as `RemoteConnectionIdentity`. Do not include the route in SSH
  connection pooling or workspace identity.
- Render the route control in the Remote Projects opening surface before the
  project is opened. It is an ordinary visible selector, not a mandatory modal
  or a connectivity claim.
- For an SSH-config-only host, create a normal saved SSH entry only when the
  user changes the route or otherwise saves the host.
- Expose the effective route to Agent Threads by looking it up from the
  project's SSH connection identity. Local, WSL, Docker, and mock projects keep
  their existing launch behavior and do not acquire egress.
- Document the optional setting in the SSH connection example without changing
  the default settings value to `Through Flint`.

Validation:

```sh
cargo test -p settings_content remote_agent_route
cargo test -p recent_projects remote_agent_route
cargo test -p recent_projects test_open_remote_project_with_mock_connection
```

Expected result: the opener makes the choice visible, an untouched user gets
today's direct behavior, and route state is independent of observed network
connectivity.

## 6. Add Owned SSH Reverse and OAuth Callback Forwards

Files:

- Create `crates/remote/src/port_forward.rs`
- Modify `crates/remote/src/remote.rs`
- Modify `crates/remote/src/remote_client.rs`
- Modify `crates/remote/src/transport/ssh.rs`
- Modify `crates/remote/src/transport/mock.rs`
- Modify `crates/remote/src/transport/wsl.rs`
- Modify `crates/remote/src/transport/docker.rs`

Write tests first:

- Build a reverse-forward command and assert `ssh -N -R` binds the remote side
  to `127.0.0.1`, targets the local loopback listener, uses
  `ExitOnForwardFailure=yes`, preserves configured SSH/jump/identity/port and
  askpass inputs, and keeps stderr captured.
- On non-Windows clients, assert every owned forward contains
  `ControlMaster=no` and `ControlPath=none`, even when the normal connection
  uses Flint's or an external ControlMaster.
- Build a callback command and assert `ssh -N -L` binds only the requested local
  loopback port and targets only the remote loopback callback port. It must not
  reference an egress lease or CONNECT proxy.
- Test IPv4/IPv6 formatting, occupied ports, remote-forward collisions,
  forwarding-policy denial, unexpected child exit, cancellation before
  readiness, explicit close, and drop.
- Use a fake child-process runner to prove close terminates and awaits the
  process, monitor errors retain redacted stderr, and a dropped handle cannot
  leave its monitor task silently cancelled.
- Extend `MockRemoteConnection` so tests can control readiness, collision,
  disconnect, and cleanup results for each forward direction.

Run and confirm failure before adding the owned handle contract:

```sh
cargo test -p remote port_forward
cargo test -p remote transport::ssh::tests
```

Implement:

- Add transport-only `ReversePortForwardSpec`, `LocalPortForwardSpec`, an owned
  handle, readiness/failure state, explicit async close, and drop cancellation.
  Keep all names free of agents, OAuth providers, and destination policy.
- Start a dedicated SSH process for every owned forward. A connection has at
  most one long-lived reverse process for its egress session; each callback
  attempt owns its short-lived local-forward process.
- Use `ConnectionSharing::Dedicated` and explicitly disable ControlMaster and
  ControlPath on non-Windows. Do not add or cancel a forward on a master
  connection Flint may not own.
- Treat `ExitOnForwardFailure` startup errors separately from authentication
  and remote-server disconnect errors. Preserve bounded stderr for the UI but
  redact command environment and any future proxy URL.
- Make explicit close return cleanup errors. Drop sends cancellation and leaves
  the already-detached monitor responsible for awaiting/logging process exit.
- Return unsupported-operation errors on non-SSH transports. Ordinary existing
  `build_forward_ports_command` consumers remain unchanged.

Validation:

```sh
cargo test -p remote port_forward
cargo test -p remote transport::ssh::tests
cargo check -p repl -p project
```

Expected result: both forwarding directions have a process-backed ownership
boundary that cannot mutate an external ControlMaster.

## 7. Implement the Restricted CONNECT Proxy and Agent Policies

Files:

- Create `crates/agent_threads/src/connect_proxy.rs`
- Modify `crates/agent_threads/src/agent_threads.rs`
- Modify `crates/agent_threads/src/agent_release.rs`
- Modify `crates/agent_threads/Cargo.toml`
- Modify `crates/settings_content/src/settings_content.rs`
- Modify `assets/settings/default.json`

Write tests first:

- Accept an authenticated CONNECT to one exact allowed DNS hostname and port,
  return `200`, and relay bytes to a fake upstream.
- Reject missing, malformed, wrong, expired, and released capabilities without
  echoing the authorization value.
- Reject plain HTTP, other methods, host suffix confusion, case tricks,
  trailing-dot ambiguity, wildcard abuse, userinfo, missing ports, unexpected
  ports, IP literals, non-ASCII authorities, and disallowed hosts.
- Enforce fixed header byte/count and handshake-time limits. The timer must use
  the GPUI/background executor in GPUI tests.
- Keep an accepted stream alive past the handshake timeout while idle, then
  resume traffic.
- Half-close each direction independently and prove the other direction can
  finish. Cancel an established relay when its lease or session is revoked.
- Bind only a local loopback listener and resolve an approved hostname locally
  only after authorization and policy checks succeed.
- Capture logs with success, denial category, agent kind, destination, and
  duration. Assert no log or error contains a capability, authorization header,
  proxy URL, request body, or response body.
- Validate versioned Codex and Claude policies: required model/authentication,
  optional telemetry disabled by default, and update/installer/artifact hosts
  always unsupported.
- Add settings tests for connection-scoped, per-agent exact-host overrides.
  Reject IP literals, top-level wildcards, malformed authorities, and an
  override for an unknown agent kind.

Run and confirm failure:

```sh
cargo test -p agent_threads connect_proxy
cargo test -p agent_threads destination_policy
cargo test -p settings_content remote_agent_egress
```

Implement:

- Add a loopback-only async CONNECT listener with a small explicit parser; do
  not turn Flint's general HTTP client into a server or add plain HTTP proxying.
- Represent capabilities with a type whose `Debug` and `Display` output is
  always redacted. Generate cryptographically random per-lease values and use
  standard proxy authorization supported by the pinned agent version.
- Bind each active capability to one agent kind and check that kind's policy on
  every request. Never authorize against the union of active policies.
- Put endpoint categories and the readiness destination on methods/data of the
  existing `AgentKindDefinition`. Validate exact host overrides from settings;
  reject wildcard top-level domains and IP literals.
- Add an optional per-agent exact-host override to `settings::SshConnection`.
  Resolve it by the same SSH identity as the route, expose it in the generated
  settings schema/example, and keep it empty by default. It changes no opener
  route value and grants no update, installer, artifact, or plain-HTTP access.
- Relay opaque TCP bytes after CONNECT acceptance. Provider TLS remains end to
  end, and relay code implements half-close rather than cancelling both sides
  when the first copy future ends.
- Apply limits only to the handshake. Established streams have no fixed idle
  timeout but remain cancellable by lease/session shutdown.

Validation:

```sh
cargo test -p agent_threads connect_proxy
cargo test -p agent_threads destination_policy
cargo test -p settings_content remote_agent_egress
```

Expected result: possession of a live per-agent capability permits only the
compiled model/auth destinations and cannot become general remote egress.

## 8. Add the Connection-Scoped Egress Session and Leases

Files:

- Create `crates/agent_threads/src/egress.rs`
- Modify `crates/agent_threads/src/agent_threads.rs`
- Modify `crates/agent_threads/src/store.rs`
- Modify `crates/remote/src/remote_client.rs`

Write tests first:

- Use the mock forward, proxy, and remote loopback exchange to prove the first
  lease creates one local listener and one reverse forward, while later leases
  reuse both and receive different capabilities.
- Bind two concurrent kinds and prove each capability sees only its own policy.
- Release one lease and prove only that capability and its active relays stop.
  Release the last lease and prove the forward, listener, monitor, and all
  capability state stop.
- Fail local bind, reverse-forward startup, each collision retry, CONNECT
  readiness, and cancellation. No case may return a lease or leak a task.
- Make randomized remote-port selection deterministic in tests. Assert only a
  bounded high-port range is used, collisions choose another port, and the
  selected port remains stable for the `RemoteClient` lifecycle.
- Ask for a lease while the client is reconnecting/disconnected and assert an
  actionable state error.
- Prove a direct-route launch path cannot reach `AgentEgressManager::acquire`.

Run and confirm failure:

```sh
cargo test -p agent_threads egress
```

Implement:

- Add one global `AgentEgressManager` keyed by the live `RemoteClient` entity
  identity, not only by host text. It creates at most one
  `AgentEgressSession` per client.
- Start the CONNECT listener before the reverse forward. Retry a bounded number
  of randomized remote ports only for a classified collision.
- Register a capability, then use `RemoteClient`'s bounded loopback TCP RPC to
  send an authenticated CONNECT readiness request through the remote endpoint
  to the agent kind's required readiness destination. Remove the capability
  and all newly created state if the probe fails.
- Return an `AgentEgressLease` that owns its capability registration. Explicit
  release and drop are idempotent and never discard cleanup errors silently.
- Keep the remote port and local listener stable until the session ends. A new
  `RemoteClient` entity creates a new session and port.
- Store a lease in the runtime resources attached to a terminal only after the
  terminal is successfully created; retain a temporary guard so failures before
  registration still release it.

Validation:

```sh
cargo test -p agent_threads egress
cargo test -p remote remote_loopback_tcp_exchange
```

Expected result: concurrent through-Flint threads share one tunnel but not one
capability, and the final release removes the egress path.

## 9. Integrate Provisioning and Route Preparation into Agent Launch

Files:

- Create `crates/agent_threads/src/launch.rs`
- Modify `crates/agent_threads/src/agent_threads.rs`
- Modify `crates/agent_threads/src/store.rs`
- Modify `crates/agent_threads/src/history.rs`
- Modify `crates/agent_threads/src/panel.rs`
- Modify `crates/agent_threads/Cargo.toml`

Write tests first:

- Characterize the current local, configured remote, and ambient default
  command, arguments, environment, working directory, fresh session ID, resume,
  and restore behavior before changing `spawn_thread_task`.
- Resolve an explicit configured command ahead of every managed preference. Do
  not force a custom wrapper through the pinned version matcher.
- For the default ambient command, test platform-appropriate executable lookup
  using the same resolved project directory environment used by the terminal.
  If it exists, preserve its command string; if not, offer managed
  provisioning.
- Test the full route/environment matrix:

  | Executable | Route             | Proxy env | Self-update suppression |
  | ---------- | ----------------- | --------- | ----------------------- |
  | Ambient    | Not through Flint | No        | No                      |
  | Managed    | Not through Flint | No        | Yes                     |
  | Ambient    | Through Flint     | Yes       | Yes                     |
  | Managed    | Through Flint     | Yes       | Yes                     |

- Under `Through Flint`, assert supported proxy variables plus controlled
  `NO_PROXY` and `no_proxy` exactly equal
  `localhost,127.0.0.1,::1`. Do not merge an ambient bypass list.
- Assert `HTTP_PROXY`/`http_proxy` are omitted unless the pinned adapter proves
  that all requests it sends through those variables use supported CONNECT
  semantics. `HTTPS_PROXY` and its supported lowercase form remain scoped to
  the agent process.
- Run an agent-shaped loopback client and prove it bypasses the proxy.
- Decline a managed-install offer and assert the route and command preference
  are unchanged and no terminal or lease is created.
- Fail command resolution, provisioning, lease acquisition, readiness, and
  terminal creation. Each error must reach the Agent Threads UI and release all
  temporary managed-use/egress resources.
- Launch successfully and prove the terminal keeps the real remote project
  directory and `ThreadEntry` owns the lease and managed-version guard until
  terminal closure.
- Repeat the preparation assertions for resume and session restore.

Run the current and new focused tests before implementation:

```sh
cargo test -p agent_threads launching_a_new_thread
cargo test -p agent_threads launch
```

Implement:

- Add one `AgentLaunchPreparer` that returns a `PreparedAgentLaunch` containing
  the resolved command, environment, optional managed-use guard, optional
  egress lease, and effective route. Keep `SpawnInTerminal` construction in
  `store.rs`.
- Leave local projects and non-SSH remote transports on the current path.
- Preserve whether `launch_command_from_content` received an explicit user
  command or supplied the agent kind's default. The resolver needs this origin
  bit; comparing strings cannot distinguish an explicit `"codex"` wrapper
  choice from the ambient default.
- Preserve explicit configured commands. Probe only the known ambient default
  command with platform-specific lookup and the resolved project environment;
  a probe must not execute arbitrary user arguments.
- If the ambient default is absent, reuse a valid managed receipt or prompt to
  provision. Persist an explicit managed selection per connection identity and
  agent kind in non-secret state; clearing/removing it restores ambient lookup.
- Acquire egress only for `Through Flint`, wait for readiness, and inject the
  proxy URL only into the selected agent process. The proxy URL must never be
  added to `ProjectEnvironment`, project settings, command labels, or logs.
- Apply catalogue self-update suppression to every through-Flint process and
  every managed process. Do not modify an ambient installation's persistent
  configuration.
- Make launch/resume APIs return or detach a fallible task through an existing
  user-visible error helper. Do not reduce failures to log-only output.
- Extend `ThreadEntry` with runtime resources and release them on terminal
  release, terminal exit, explicit close, route change, and managed removal.

Validation:

```sh
cargo test -p agent_threads launch
cargo test -p agent_threads panel
cargo test -p agent_threads history
```

Expected result: the current terminal path remains the execution mechanism,
with one asynchronous preparation step selecting the executable and optional
egress resources.

## 10. Add Agent Management UI and Active Route Changes

Files:

- Modify `crates/agent_threads/src/panel.rs`
- Modify `crates/agent_threads/src/store.rs`
- Modify `crates/agent_threads/src/managed_agent.rs`
- Modify `crates/recent_projects/src/remote_servers.rs`
- Modify `crates/recent_projects/Cargo.toml`

Write tests first:

- Show the effective route read-only in each remote Agent Threads section.
  Local sections must not claim either remote route.
- When no ambient command or managed receipt is usable, show an install action
  rather than silently opening a command-not-found terminal.
- Cover install progress, retry, update available, update success, update
  rollback, invalid managed installation, reinstall, remove confirmation, and
  removal failure states for Codex and Claude.
- Prove an explicit custom command is not removed or replaced by managed-agent
  actions.
- Change the route for an identity with no active agent terminals and assert it
  applies to the next launch without reconnecting ordinary remote editing.
- Change it with live agent/credential terminals, require confirmation, close
  only terminals for that connection identity, await their resource cleanup,
  then persist the new route. Cancelling must change nothing.
- Once a route change is confirmed, reject new agent/credential launches for
  that identity until terminal cleanup and the settings write complete.
- Moving away from `Through Flint` must close the reverse forward after the
  final affected lease; moving to it must remain lazy until the next agent or
  credential action.
- Removing a managed version closes only terminals using that managed version
  after confirmation, not ambient terminals or unrelated connections.

Run and confirm failure:

```sh
cargo test -p agent_threads agent_management
cargo test -p recent_projects active_agent_route_change
```

Implement:

- Add install/use managed, update, reinstall, and remove actions to the existing
  per-kind panel controls. Reuse panel state rather than opening a separate
  agent-management application.
- Expose narrow `AgentThreadStore` queries/actions for live runtime resources
  by normalized connection identity. `recent_projects` may call this UI-facing
  boundary after confirmation; it must not inspect capabilities or provisioner
  internals.
- Track each thread's window and terminal item sufficiently to close it through
  the normal pane lifecycle. Killing a process without releasing/removing its
  terminal resource is not sufficient.
- Persist a route only after all confirmed close operations complete. If
  cleanup fails, keep the old route selected and show the error.
- Distinguish catalogue, download, verification, upload, installation, route,
  proxy, SSH forward, readiness, and authentication stages in displayed errors.

Validation:

```sh
cargo test -p agent_threads
cargo test -p recent_projects active_agent_route_change
```

Expected result: provisioning and route changes are understandable, explicit,
and transactional from the user's existing Remote Projects and Agent Threads
surfaces.

## 11. Add Credential Status, Login, Logout, and Revocation Guidance

Files:

- Create `crates/agent_threads/src/credentials.rs`
- Create non-secret status/output fixtures below
  `crates/agent_threads/test_data/credentials/`
- Modify `crates/agent_threads/src/agent_threads.rs`
- Modify `crates/agent_threads/src/panel.rs`
- Modify `crates/agent_threads/src/store.rs`
- Modify `crates/agent_threads/src/launch.rs`

Write tests first:

- Parse authenticated, unauthenticated, expired/revoked where distinguishable,
  and changed/unrecognized output for each pinned Codex and Claude CLI. Unknown
  output becomes `Unknown`, not a launch or logout blocker.
- Assert fixtures contain no live token, key, cookie, authorization header, or
  home-directory credential file.
- Under each route, build a credential-management terminal with the same
  executable resolution as a normal thread. `Through Flint` gets its own
  egress lease; `Not through Flint` gets none.
- For a pinned login with a fixed or safely discoverable callback port, start
  the independent temporary local forward under both routes. Assert it carries
  no CONNECT capability and survives release of an unrelated egress lease.
- Close the callback handle on success, cancellation, timeout, disconnect,
  terminal release, and handle drop.
- Fail callback-port discovery, local bind, or SSH local forwarding and prove
  the UI offers the pinned device/code-copy/headless fallback without changing
  the route.
- Logout must close affected terminals and callback forwards, release their
  leases, run the supported remote logout command, and verify status. A command
  or verification failure must not claim the host is disconnected.
- While **Disconnect this host** is running, reject new agent and credential
  actions for that connection identity and agent kind; clear that guard on
  failure so the user can retry.
- Assert the provider-revocation action opens only the compiled provider
  management URL and explains that remote logout removes only the local copy.

Run and confirm failure:

```sh
cargo test -p agent_threads credentials
```

Implement:

- Put versioned status/login/logout command definitions and parsers on the
  existing agent-kind policy. Do not infer commands from arbitrary CLI output.
- Run status and logout as captured remote commands whose stdout/stderr is
  bounded and redacted. Login remains an interactive terminal so the native
  CLI UX is preserved.
- Attach a credential terminal's egress lease and callback-forward handle to
  the same terminal-lifecycle resource mechanism used by agent threads.
- Start `RemoteClient::start_local_port_forward` only when the pinned login
  contract supplies a safe port. Use a fixed timeout and close partial state
  before presenting fallback instructions.
- Implement separate **Disconnect this host** and **Revoke at provider**
  actions. Never automate provider-page scraping or claim provider-side
  invalidation after remote logout.
- Represent an in-progress disconnect in the Agent Thread store before closing
  terminals so no new launch can race between containment and logout.
- Persist only the route and managed selection. Credential status is queried on
  demand and secrets are never copied into Flint state.

Validation:

```sh
cargo test -p agent_threads credentials
cargo test -p remote port_forward
```

Expected result: the user can authenticate natively on the remote host, remove
the remote credential copy, and reach the provider's authoritative revocation
surface without Flint receiving the credential.

## 12. Implement Disconnect, Reconnect, and Runtime Failure Handling

Files:

- Modify `crates/remote/src/remote_client.rs`
- Modify `crates/agent_threads/src/egress.rs`
- Modify `crates/agent_threads/src/store.rs`
- Modify `crates/agent_threads/src/panel.rs`
- Modify `crates/project/src/project.rs` only to handle an expanded
  `RemoteClientEvent` exhaustively without changing existing disconnect
  behavior

Write tests first:

- Emit observable connection-state transitions for connected, heartbeat
  missed, reconnecting, reconnected, and permanently disconnected states while
  preserving the existing `Disconnected { server_not_running }` behavior.
- On connection loss, reject new leases, mark existing through-Flint threads
  unavailable, and keep their stable remote port/capabilities in memory without
  claiming the forward is alive.
- On reconnect, start a new dedicated reverse-forward process on the same
  remote port, run the bounded authenticated readiness exchange, and restore
  availability.
- Fail same-port restoration and assert the egress session requires affected
  threads to restart; do not give live processes a new proxy URL they cannot
  receive.
- Make the forward or local proxy exit while connected. Mark only affected
  threads offline, invalidate capabilities, and present the classified runtime
  error.
- If the current remote terminal died as part of the existing reconnect
  lifecycle, prove normal history/resume behavior applies and egress does not
  add process persistence.
- Disconnect during install, update, remove, login, and logout. Assert staged
  local/remote cleanup is attempted, errors are logged without secrets, and no
  operation reports success prematurely.

Run and confirm failure:

```sh
cargo test -p remote remote_client_event
cargo test -p agent_threads reconnect
```

Implement:

- Expand `RemoteClientEvent` with connection-state transitions useful to
  lifecycle owners. Keep Project's existing permanent-disconnect notifications
  intact.
- Subscribe each egress session to its exact client entity. During reconnect,
  keep the local proxy listener and stable remote port but reject acquisition.
- Recreate only the owned forward after the new SSH connection becomes usable,
  then perform readiness before marking leases available.
- Treat a permanent client disconnect, forward process exit, local proxy exit,
  and stable-port restore failure as distinct states and errors.
- Ensure every monitor task is owned, detached deliberately, or stored. Never
  discard a failed async cleanup with `let _ =`.

Validation:

```sh
cargo test -p remote
cargo test -p agent_threads reconnect
cargo test -p project remote_client
```

Expected result: the egress path follows the SSH lifecycle without inventing a
second remote-process lifecycle or silently changing the route.

## 13. Complete Windows Support and Run Real SSH Integration

Files:

- Modify `crates/remote/src/transport/ssh.rs`
- Modify `crates/remote_server/src/headless_project.rs`
- Modify `crates/agent_threads/src/managed_agent.rs`
- Add a Linux SSH integration fixture and runner under the existing test/script
  conventions, for example `script/test-remote-agent-egress`
- Add focused integration tests in the owning crates rather than exposing
  production internals solely for the runner
- Modify `.github/workflows/run_tests.yml` to run the isolated Linux SSH suite
  when its OpenSSH/container prerequisites are available
- Add a short release-maintainer document for updating pinned agent releases
  only if no existing release checklist is an appropriate home

Write tests first:

- Extend `build_command_windows` tests so the base64-encoded PowerShell command
  safely sets the small, controlled agent environment. Validate environment
  variable names, quote adversarial values, preserve working directory and
  arguments, and stay below the documented command-line bound.
- Run remote file transaction tests with Windows paths and semantics. Executable
  permissions are a POSIX operation; Windows still verifies digest, version,
  and atomic staging/rollback behavior.
- Run reverse- and local-forward command/process tests on a Windows OpenSSH
  client. Assert no unsupported ControlMaster option is emitted and configured
  jump/identity/port/askpass behavior remains intact.
- In an isolated Linux SSH fixture, cover:
  - direct ambient launch with no proxy environment or egress lease;
  - direct provider failure with no route change;
  - callback `-L` under direct routing without a reverse forward;
  - through-Flint ambient launch without reinstall;
  - an SSH host with no outbound network and no agent executable;
  - locally verified signed test artifact upload, no-`sudo` install, absolute
    managed launch, and streaming authenticated CONNECT;
  - exact destination denial and expected optional-category denial;
  - two capabilities sharing one reverse tunnel;
  - connectivity removal after the final lease; and
  - a second SSH server configuration that denies reverse forwarding.
- Keep all network/provider endpoints fake in automated tests and all
  credentials synthetic.

Run the Windows unit tests and the new Linux integration runner before marking
the feature complete:

```sh
cargo test -p remote transport::ssh::tests
cargo test -p agent_threads
script/test-remote-agent-egress
```

Implement:

- Replace the current ignored `_input_env` in `build_command_windows` with
  validated PowerShell environment assignments inside the already encoded
  command. Pass only the prepared agent environment, not the full ambient map.
- Finish Windows staging, rollback, receipt, path, and version-check handling
  without invoking a remote download or vendor installer.
- Make the SSH integration runner create explicit temporary keys, host keys,
  users/directories, allow/deny forwarding configurations, fake provider, and
  internal network. It must clean up only its validated temporary/container
  targets and print actionable prerequisite skips locally.
- Add the integration job to CI without embedding provider credentials. Keep
  command-construction unit tests on macOS and Windows runners even if the full
  isolated SSH topology runs only on Linux.
- Manually validate every production catalogue entry on its supported target
  from an arbitrary user-owned directory with self-update suppressed. Exercise
  login, history, resume, update denial, required destinations, callback or
  headless fallback, logout, and provider revocation with dedicated test
  credentials.
- Do not merge a catalogue update when the official source, digest/signature,
  installed-byte digest, version matcher, self-update suppression, or endpoint
  fixture is unverified.

Validation:

```sh
cargo fmt --all -- --check
./script/clippy
cargo test -p proto
cargo test -p remote
cargo test -p remote_server
cargo test -p agent_threads
cargo test -p recent_projects
cargo check -p flint
script/test-remote-agent-egress
```

Also run the repository's normal Windows test job or its equivalent before
release.

Expected result: the feature works with supported local SSH clients and POSIX
or Windows SSH hosts, while the automated suite proves the offline managed
install and restricted egress path without live services.

## Acceptance Checklist

- [ ] An untouched SSH identity opens without a question and uses
      `Not through Flint`.
- [ ] The opener contains exactly the two route values and persists only an
      explicit change.
- [ ] A connectivity probe or provider failure never changes the stored route.
- [ ] Direct ambient launch is byte-for-byte equivalent at the command/env/cwd
      boundary to today's behavior.
- [ ] Through-Flint ambient launch does not force provisioning and suppresses
      self-update only for the launched process.
- [ ] Managed provisioning downloads only an exact official pin locally,
      verifies source and installed bytes, uploads through Flint, and requires
      no remote internet, package manager, or `sudo`.
- [ ] Managed launch uses an absolute versioned path and explicit updates retain
      the prior version until no live thread uses it.
- [ ] Proxy and self-update environment follow the four-case launch matrix, and
      controlled loopback `NO_PROXY` works.
- [ ] One live SSH client owns at most one reverse egress forward; leases have
      distinct, per-kind, revocable capabilities.
- [ ] The CONNECT proxy accepts only authenticated, exact policy destinations,
      preserves TLS opacity/half-close, and never logs secrets.
- [ ] Closing the final lease removes the reverse path; reconnect restores the
      same remote port or requires thread restart.
- [ ] OAuth callback forwarding works under either route and has an independent
      lifecycle.
- [ ] Logout and provider revocation are distinct actions, and Flint never
      reads the remote credential.
- [ ] POSIX and Windows remote launch paths preserve the prepared environment.
- [ ] Ordinary terminals, remote editing, history, resume, and non-SSH remote
      transports retain their existing behavior.

## Review Checkpoints

Claude should review before implementation begins, with particular attention
to:

1. the shared libc-target extension and its compatibility with existing
   remote-server asset selection;
2. the decision to verify official packages and normalize archives locally,
   pinning both source and installed-byte digests;
3. the generic `remote_management.proto` surface and loopback-only TCP RPC;
4. optional route persistence on `settings::SshConnection` without changing
   SSH/workspace identity;
5. the explicit configured-command versus managed-selection precedence;
6. the owned forward handle's async cleanup model; and
7. the split between unit/mocked lifecycle tests and the isolated real-SSH CI
   fixture.

After stages 1-4, pause for a security review of provenance and remote
transactions. After stages 6-8, pause for a security review of forwarding,
capabilities, parser limits, logging, and teardown. No production catalogue
entry should ship before the final official-release validation in stage 13.
