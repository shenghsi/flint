# Remote Agent Egress Design

## Status

Proposed for Claude's review on 2026-07-18. Reviewed and approved by Claude on
2026-07-18. Claude's review and Codex's response are recorded at the end. The
accepted review changes are incorporated into this document. No implementation
is authorized by this design review alone.

Design owner: Codex.

This document supersedes the Remote Agent Workspace and ACP recommendation in
`docs/remote-control.md` only for hosts where the agent CLI can run. For hosts
where the CLI cannot run at all, ACP and the Remote Agent Workspace remain the
recorded future direction. The discussion record remains the decision history
for both scopes.

## Summary

Run Codex and Claude Code on the SSH host, in the real remote project, while
Flint supplies their outbound model-service connectivity through an SSH reverse
forward and a restricted local HTTP CONNECT proxy.

The agent CLI is installed on the remote host before Flint launches it. The
remote host is trusted to store a dedicated provider credential for each agent.
The user can invalidate that credential at the provider and remove the local
copy from the remote host. Flint never copies a local credential store or reads
the provider secret.

This preserves Flint's terminal-first Agent Threads model:

```text
remote Codex or Claude TUI
  -> HTTPS_PROXY on remote loopback
  -> SSH reverse port forward
  -> authenticated CONNECT proxy in local Flint
  -> allowlisted agent-service endpoint
```

File access, edits, searches, and commands remain native remote agent
operations. Flint does not introduce ACP, MCP file tools, a mirrored checkout,
or a virtual filesystem.

## Accepted Constraints

- Codex and Claude Code can be installed and executed on the remote host.
- Installation is a provisioning precondition, not a remote-internet
  requirement. On an offline host, the user or administrator may use an
  internal package mirror, removable media, or a manual transfer appropriate
  for the remote operating system and architecture before enabling the feature.
- The remote host may lack direct internet access.
- The remote host is trusted to hold an agent credential.
- A dedicated credential per remote host and agent is acceptable.
- Provider-side invalidation is the authoritative credential kill switch.
- Native Codex and Claude Code terminal interfaces must remain available.
- Flint may use the existing authenticated SSH connection to provide narrowly
  scoped agent-service egress.
- Tunnel mode is explicitly enabled. Flint does not create egress automatically
  after a failed request.

## Goals

- Launch the existing remote Agent Thread with its real remote project as its
  working directory.
- Give that remote process access to the model, authentication, and required
  agent control-plane endpoints through Flint.
- Preserve native agent tools, configuration, permissions, history, resume,
  plugins, and terminal behavior.
- Restrict the tunnel to the minimum destinations required by the selected
  agent.
- Share one egress tunnel safely across concurrent Agent Threads on the same
  remote connection.
- Recreate egress after an SSH reconnect when the underlying remote process is
  still usable.
- Let the user remove a credential from the remote host and invalidate it at the
  provider.
- Surface setup, authentication, policy, and connection failures in Agent
  Threads before or during launch.
- Support local macOS, Linux, and Windows SSH clients and POSIX and Windows SSH
  hosts in the completed design.

## Non-goals

- Installing, uploading, or updating the agent CLI on the remote host.
- Running the agent locally for an SSH project.
- Restoring ACP or building a Remote Agent Workspace, MCP file bridge, remote
  filesystem mount, or local project mirror.
- Keeping provider credentials on the local machine.
- Terminating model-service TLS or injecting provider credentials in Flint.
- Giving arbitrary remote commands general internet access.
- Providing network access to package managers, user processes, or arbitrary
  MCP servers.
- Guaranteeing that an untrusted or compromised remote account cannot copy its
  own credential or proxy capability.
- Automating provider-side credential creation or revocation when the provider
  exposes no supported management API.
- Keeping a remote agent process alive across failures that already terminate
  its Flint remote terminal.

## Alternatives Considered

### Local credential-injecting gateway

Flint could retain the provider credential locally, expose a provider-aware
gateway through SSH, and inject authentication into upstream requests. Both
agents have configurable provider base URLs, so API-key-backed operation is
possible.

This was rejected for the accepted scope. It would make Flint responsible for
provider protocols, streaming compatibility, OAuth refresh, billing mode, and
credential security. Consumer subscription authentication is not a stable,
generic gateway interface. Trusting a dedicated remote credential produces a
smaller and more reliable system.

### Local agent with remote operations

ACP, MCP tools, or a mounted remote filesystem could keep model traffic and
credentials local while routing project operations to the remote host.

This was rejected because the CLI can run remotely. It would replace or
constrain native agent tools and restore a large protocol and UI surface to
solve an execution-placement problem that is not present.

### Unrestricted SOCKS or HTTP proxy

Flint could expose unrestricted internet access over SSH and rely on the remote
host's normal process isolation.

This was rejected because it turns an editor feature into general egress for
the remote account. The accepted proxy understands only HTTP CONNECT, requires
per-lease authentication, and enforces a destination policy.

## Architecture

### Responsibilities

The implementation stays within the existing `remote` and `agent_threads`
crates.

`remote` owns transport mechanics:

- a lifecycle-managed reverse-port-forward primitive;
- SSH command construction for reverse forwards;
- readiness, exit, cancellation, and reconnect reporting;
- local-client differences between OpenSSH implementations.

`agent_threads` owns agent policy and orchestration:

- the local CONNECT proxy;
- per-agent destination policies;
- per-lease proxy capabilities;
- acquisition and release of a shared egress session;
- agent launch environment;
- credential status, local removal, and provider-revocation guidance;
- user-visible state and errors.

The SSH transport does not know about Codex, Claude, credentials, or provider
domains. Agent Threads does not construct raw `ssh -R` commands or inspect SSH
control sockets.

### `AgentEgressSession`

One `AgentEgressSession` belongs to one live SSH `RemoteClient`. It owns:

- one local loopback CONNECT proxy listener;
- one loopback-only remote reverse forward;
- the stable remote proxy port for that connection lifetime;
- the allowed destinations needed by active agent kinds;
- one random proxy capability per egress lease;
- a lease count and connection state.

Starting an agent thread acquires an `AgentEgressLease`. The first lease starts
the proxy and reverse forward. Later leases reuse them while receiving distinct
proxy capabilities. Releasing a lease immediately invalidates its capability.
Releasing the last lease closes the forward, stops the proxy, and drops all
remaining capability state.

Sharing at the remote-connection boundary avoids one SSH tunnel per terminal
while preserving per-lease revocation and audit attribution. Each capability is
bound to one agent kind, and every request is checked against that kind's
destination policy rather than the union of all active policies. An egress
session is never shared between two remote hosts or two `RemoteClient`
instances.

### `ReversePortForward`

Extend the remote transport with an owned reverse-forward operation that
returns a handle. The handle reports readiness and terminal failure. Closing or
dropping it cancels the forward and observes cleanup errors.

The SSH implementation owns one dedicated, long-lived `ssh -N -R` forwarding
process per `AgentEgressSession` on every local platform. On non-Windows
clients, it explicitly disables connection sharing with `ControlMaster=no` and
`ControlPath=none` instead of adding state to Flint's or the user's existing
ControlMaster. Windows already uses a separate connection because its OpenSSH
client lacks ControlMaster support. This costs at most one additional SSH
authentication per live egress session, not per Agent Thread, in exchange for a
forward whose lifetime is represented by the owned process and SSH connection.

The first setup chooses a high randomized remote port and retries another port
when `ExitOnForwardFailure` reports a collision. That port remains stable only
for the current `RemoteClient` lifecycle so reconnect can restore the proxy URL
already held by a live agent. A new `RemoteClient` lifecycle chooses a new port,
which prevents a stale listener from an abruptly terminated prior lifecycle
from making startup permanently fail.

The SSH arguments must:

- bind the remote listener to `127.0.0.1`;
- set `ExitOnForwardFailure=yes`;
- connect only to the local proxy's loopback listener;
- preserve configured jump hosts, identity files, ports, and askpass behavior;
- keep stderr available for an actionable startup or runtime error.

Graceful teardown terminates and awaits the forwarding process. The transport
must not claim that killing a mux client cancels a forward without proving that
behavior for every supported OpenSSH implementation; this design avoids that
dependency by giving the forward its own SSH connection.

An SSH server can reject remote forwarding through `AllowTcpForwarding`,
`DisableForwarding`, or `PermitListen`. That is a capability failure, not an
authentication failure, and is reported separately.

### Restricted CONNECT proxy

The proxy accepts only authenticated HTTP CONNECT requests. It does not forward
plain HTTP requests, terminate TLS, inspect model traffic, or add provider
headers.

Each request must satisfy all of these conditions:

- the proxy capability is active and belongs to a live lease;
- the authority is a syntactically valid DNS hostname and explicit port;
- the lowercase hostname matches an active agent destination policy;
- the port is `443` unless that exact destination policy permits another port;
- header size, header count, and handshake time remain below fixed limits;
- the request does not use an IP literal unless the policy explicitly names it.

The proxy resolves approved hostnames locally and opens the upstream TLS byte
stream without interpreting it. It records the agent kind, destination, result,
and duration, but never records proxy authorization, provider credentials,
request bodies, or response bodies.

Handshake limits apply only until a CONNECT request is accepted. An established
byte stream has no fixed idle timeout because model responses can remain quiet
for long periods. Relay code preserves TCP half-close so one side can finish
sending while continuing to receive the other side's stream.

The per-lease capability is a random value carried in the proxy URL provided to
that agent process or credential-management terminal. It prevents unrelated
remote users or processes from casually using the forward. It does not isolate
processes running as the same remote operating-system user; that limitation is
part of the accepted trust boundary.

### Destination policies

Codex and Claude destination policies are methods on the existing agent-kind
definition rather than a second provider registry. They distinguish:

- required model and authentication endpoints;
- optional telemetry endpoints;
- optional update and artifact endpoints;
- unsupported general-purpose endpoints.

The default policy enables only endpoints required for normal interactive
agent operation and authentication. Optional telemetry, update, and artifact
destinations are blocked by default. The user can enable an optional category
per host when the corresponding agent feature requires it.

Blocked requests identify the hostname and policy category in the Agent Thread
error surface without logging secrets. Policy additions require a Flint update
or an explicit per-host user override. Overrides are visible in settings and do
not accept wildcard top-level domains.

The required sets include every model and authentication host exercised by the
supported login modes. They are versioned, tested data rather than permanent
hostnames embedded in transport code. Each supported Codex and Claude Code
version is validated against its current provider documentation and login flow;
for example, current Claude Code documentation names `platform.claude.com`, not
the older `console.anthropic.com`, for Console authentication. A request blocked
because an optional category is disabled is reported as an expected policy
decision rather than a generic network failure.

This policy does not make `curl`, package managers, shell commands, WebFetch,
or arbitrary remote MCP servers generally online. Features requiring other
destinations fail as they do on the offline host unless the user separately
configures those destinations.

## Launch and Runtime Flow

### Enablement

Agent egress is an explicit network mode associated with the SSH connection
identity:

- `direct`: current behavior; Flint adds no proxy environment;
- `tunnel`: acquire managed agent egress before launching a remote agent;
- `disabled`: do not launch Agent Threads on that remote connection.

The mode is stored independently of whether the connection originated in
Flint settings or `~/.ssh/config`. Flint never changes `direct` to `tunnel`
after observing a network error.

### Thread launch

`spawn_thread_task` keeps the current remote terminal path but gains an egress
preparation step for SSH projects in tunnel mode:

1. Resolve the selected agent kind and remote connection identity.
2. Acquire an `AgentEgressLease` for that connection and agent kind.
3. Wait for the local proxy and reverse forward to report ready.
4. Verify the remote loopback endpoint with an authenticated CONNECT handshake
   that transmits no provider credential.
5. Add the proxy URL to the remote agent launch environment.
6. Call the existing `project.create_terminal_task` path with the real remote
   project directory.
7. Store the lease with the live Agent Thread so terminal closure releases it.

The proxy URL is applied only to the agent process. It is not added to the
remote project's general environment or persisted in project settings.

The agent uses its standard remote credential store and performs end-to-end TLS
with its provider. Native history remains associated with the real remote
project path, so existing remote history discovery continues to work.

### Environment

Agent adapters set the proxy variables the current CLI supports, including
`HTTPS_PROXY` and the corresponding lowercase form when required. `HTTP_PROXY`
is set only if the proxy supports every request type the agent sends through
that variable. They also set both `NO_PROXY` and `no_proxy` to the controlled
loopback bypass list `localhost,127.0.0.1,::1`. Flint does not inherit arbitrary
remote bypass entries into tunnel mode because an external hostname in
`NO_PROXY` would evade the destination policy. If a supported CLI version does
not honor a loopback bypass, tunnel mode is unavailable for that version rather
than silently breaking local services or weakening policy.

The current SSH command builder for Windows remote hosts ignores its input
environment. Windows remote-host support is therefore gated on transporting
the agent's proxy environment safely through the remote terminal path. The
PowerShell command is already base64-encoded, so environment assignment can be
added without approaching the existing command-line quoting problem. This is a
prerequisite, not a reason to put agent policy in the SSH transport.

### Reconnect

When the `RemoteClient` disconnects, the egress session enters a disconnected
state and rejects new leases. Existing threads show that agent connectivity is
unavailable.

After the SSH connection reconnects, the egress session attempts to recreate
the reverse forward on the same remote loopback port. A stable port matters
because an existing agent process cannot receive a replacement proxy URL.
Random proxy capabilities prevent a different process from using a recreated
forward without also possessing a live thread's capability.

If the existing remote terminal was terminated by the disconnect, current
Agent Threads lifecycle and resume behavior applies. The egress feature does
not add a second process-persistence mechanism.

## Credential Lifecycle

### Provisioning

Flint does not copy `auth.json`, `.credentials.json`, keychain entries, or
credential directories from the local machine. A credential-management
terminal acquires its own egress lease. The user authenticates through an
agent-supported remote or headless flow while that lease is active, or
provisions a dedicated provider token using the provider's supported method.
Flint can show a URL or device code, but it does not assume a browser callback
on the local machine reaches a listener on the remote host.

When an agent login flow exposes a fixed or discoverable loopback callback port,
the credential-management lease may offer a temporary SSH local forward from
the same local port to the remote CLI's loopback listener. This lets the user's
local browser complete the standard OAuth callback while the credential remains
on the remote host. If the port cannot be determined or reserved safely, Flint
uses the agent's device, code-copy, or headless flow instead.

The preferred credential is named for one remote host and one agent and has the
shortest practical expiration. Supported examples include:

- a project-scoped API key that can be deleted independently;
- a time-limited Codex workspace access token where available;
- a separately listed Claude Code authorization token.

Flint derives credential status on demand through the agent CLI. It keeps only
non-secret runtime state and the configured network mode; it does not persist a
parallel credential inventory. Flint does not promise that a credential is
dedicated when the provider or CLI does not expose enough metadata to verify
that claim. If a CLI version's status output cannot be parsed, Flint reports the
status as unknown. Unknown status alone never blocks launching the CLI or
attempting logout; the actual command result remains authoritative.

### Removal and invalidation

The UI uses distinct terms because local removal and server-side invalidation
are different operations.

**Disconnect this host** performs the local operation:

1. Prevent new Agent Threads for that agent and host.
2. After confirmation, close its active agent terminals.
3. Release their egress leases and close the reverse forward if no leases
   remain.
4. Run the agent's supported logout command on the remote host.
5. Verify that the CLI reports no active credential.

**Revoke at provider** opens the provider's credential-management surface and
explains which dedicated credential to revoke. Flint does not perform
server-side revocation in this design. It never scrapes a provider web page or
asks for an additional account credential.

Relevant management surfaces include
[OpenAI API keys](https://platform.openai.com/api-keys),
[Codex workspace access tokens](https://chatgpt.com/admin/access-tokens), and
Claude's **Settings > Claude Code** authorization-token list.

The user-facing flow states that remote logout only erases the stored copy. If
the credential may have been copied, provider-side revocation is required. A
revoked or expired credential fails on the next authenticated provider request;
an in-flight response may finish. Closing the tunnel first provides immediate
network containment for a host whose only egress is Flint.

## Failure Handling

Failures are represented by stage so the user knows what to fix:

| Stage          | Example                                             | Result                                                                      |
| -------------- | --------------------------------------------------- | --------------------------------------------------------------------------- |
| Policy         | tunnel mode disabled or destination denied          | Do not launch, or report the denied hostname                                |
| Local proxy    | listener or task fails                              | Do not launch; retain no lease                                              |
| SSH forward    | server forbids `-R` or port is unavailable          | Do not launch; suggest checking SSH forwarding policy                       |
| Readiness      | remote loopback probe fails                         | Tear down the partial session and do not launch                             |
| Authentication | CLI reports missing, expired, or revoked credential | Keep the tunnel available for login; show the supported remote login action |
| Runtime        | proxy or forward exits                              | Mark active threads offline and attempt connection-scoped recovery          |
| Reconnect      | stable remote port cannot be restored               | Fail the egress session and require thread restart                          |
| Logout         | CLI cannot remove its local credential              | Keep the host marked authenticated and show the command error               |

Startup is transactional: a failure before terminal creation releases the
capability, closes a newly created forward when it has no other leases, and
propagates an error to the Agent Threads UI. Errors are never discarded with
`let _ =`.

Runtime cleanup is best effort only where the remote connection is already
gone, but every cleanup error is logged without secrets. Provider-side
revocation remains available even when Flint cannot reconnect to remove the
remote credential file.

## Security Model

The feature protects against accidental general egress and unrelated remote
accounts. It does not protect a provider credential from the remote operating-
system account that owns it.

Security invariants:

- The remote forward binds only to remote loopback.
- The local proxy binds only to local loopback.
- Every thread receives an unguessable, revocable proxy capability.
- Only active agent destination policies can open upstream connections.
- Provider TLS remains end to end between the remote CLI and provider.
- Flint never receives plaintext provider credentials. It forwards only the
  encrypted TLS byte stream and never logs or persists its contents.
- Proxy capabilities and proxy URLs are redacted from logs and errors.
- Closing the last lease removes the egress path.
- Local logout and provider revocation are presented as separate actions.
- Direct mode retains today's behavior and opens no new listener or forward.

A malicious process running as the same remote user can inspect the agent's
credential store or environment and can impersonate the agent. Dedicated,
short-lived, server-revocable credentials limit the resulting blast radius.
That host-level risk is accepted by choosing remote credential storage.

## Testing Strategy

Implementation follows test-driven development.

### CONNECT proxy unit tests

- Accept an authenticated CONNECT to an exact allowed hostname and port.
- Reject missing, expired, wrong, and released capabilities.
- Reject disallowed hosts, suffix-confusion hosts, wildcard abuse, IP literals,
  and unexpected ports.
- Reject plain HTTP forwarding and non-CONNECT methods.
- Enforce header size, header count, and handshake timeout limits.
- Leave accepted streams alive while idle and verify that traffic can resume.
- Preserve both directions of a stream after either side half-closes its write
  half.
- Stop an established stream when its lease or egress session is cancelled.
- Confirm logs and errors never include capability or authorization values.

### Remote transport unit tests

- Build a loopback-only reverse forward with `ExitOnForwardFailure=yes`.
- Preserve configured SSH port, jump host, identity, and askpass arguments.
- Use an owned forwarding process on every local platform.
- Disable connection sharing on non-Windows clients and prove that an external
  ControlMaster is not mutated.
- Retry a high randomized remote port after a collision and keep the selected
  port stable during reconnect.
- Report readiness, forwarding denial, unexpected exit, and cancellation.
- Terminate and await the owned process on drop, surfacing cleanup failures
  through logging.

### Agent egress lifecycle tests

- The first lease starts one proxy and one reverse forward.
- Additional leases reuse the session and receive different capabilities.
- Releasing one lease invalidates only its capability.
- Releasing the last lease stops the proxy and forward.
- A failed readiness probe creates no terminal and leaks no task.
- Connection loss blocks new leases and marks existing ones unavailable.
- Reconnect restores the same remote port or reports a restart requirement.
- Direct mode never starts proxy or forwarding work.

### Agent launch and credential tests

- Tunnel mode injects the proxy environment only into the selected agent.
- Tunnel mode injects the controlled loopback `NO_PROXY`/`no_proxy` values, and
  an agent-shaped loopback HTTP client bypasses the CONNECT proxy entirely.
- The remote project directory remains the process working directory.
- POSIX and Windows remote launch paths preserve the proxy environment.
- Credential status and logout commands are covered by versioned fixtures for
  Codex and Claude Code.
- Unrecognized credential status becomes unknown without blocking launch or
  logout.
- A supported browser login callback can use a temporary local forward without
  exposing the remote credential to Flint.
- Logout failures reach the UI and do not claim successful disconnection.
- No test fixture contains a live credential.

### SSH integration test

Run a local SSH test server with direct outbound access denied and a fake TLS
model endpoint reachable only through Flint. Prove that:

- the remote agent-shaped client completes an authenticated CONNECT and
  exchanges a streaming response;
- an unapproved destination is rejected;
- a second concurrent lease shares the tunnel;
- closing the last lease removes connectivity;
- an SSH server configuration that denies reverse forwarding produces the
  expected user-visible failure.

## Delivery Sequence

1. Add failing tests and the owned reverse-forward transport contract.
2. Implement the dedicated SSH reverse-forward process and cleanup on
   non-Windows local clients.
3. Add the restricted CONNECT proxy and its security tests.
4. Add connection-scoped egress leases and readiness behavior.
5. Integrate tunnel-mode preparation into remote Agent Thread launch.
6. Add agent destination policies and credential status/logout actions.
7. Add reconnect behavior and failure UI.
8. Add the dedicated Windows local-client forward and Windows remote
   environment support.
9. Run the SSH integration suite and manually validate Codex and Claude Code
   with dedicated test credentials.

Each step keeps direct-mode Agent Threads working and independently testable.

## Acceptance Criteria

- On an SSH host with no direct internet, an installed and authenticated Codex
  or Claude Code CLI can complete model requests through Flint.
- An offline host may be provisioned before use without Flint owning agent CLI
  installation or requiring the host itself to reach an installer.
- The native agent TUI, tools, history, permissions, project path, and resume
  behavior remain intact.
- Commands and file operations execute on the remote host.
- The remote loopback proxy cannot reach a destination outside the selected
  agent's policy.
- Concurrent threads on one connection share a tunnel without sharing proxy
  capabilities.
- Closing the final Agent Thread removes the tunnel.
- SSH forwarding-policy failures and invalid credentials produce distinct,
  actionable errors.
- The user can remove the credential from the remote host and is directed to
  invalidate the dedicated credential at the provider.
- Flint storage, logs, and proxy code never receive a plaintext provider
  credential.
- Direct remote Agent Threads behave exactly as before when tunnel mode is not
  enabled.

## Original Review Requests for Claude

The pre-review design asked Claude to challenge these decisions in particular:

- one shared egress session per `RemoteClient`, with per-lease capabilities;
- ControlMaster forward management on non-Windows clients and a dedicated
  forwarding process on Windows;
- the split between generic SSH mechanics in `remote` and agent policy in
  `agent_threads`;
- strict CONNECT-only, destination-allowlisted egress rather than a general
  proxy;
- native remote login plus provider-side revocation rather than credential
  copying or a local credential gateway;
- the stable remote port requirement during reconnect;
- whether the stated endpoint-policy and Windows prerequisites cover current
  Codex and Claude Code behavior without expanding the accepted scope.

## Review — Claude (2026-07-18)

Overall: the design is sound and I endorse its core decisions. The scope cut
(dedicated remote credential, no gateway, no ACP/MCP/mount) is the right
trade for installable hosts, the security invariants are honest about the
same-user trust boundary, the failure taxonomy and transactional startup
match this repo's error-handling rules, and the test strategy is strong —
particularly the capability-redaction and suffix-confusion cases. Verified
against the codebase: ControlMaster is used on non-Windows and absent on
Windows (`crates/remote/src/transport/ssh.rs:212`), and the Windows proxy
command indeed does not propagate env today (`ssh.rs:457` TODO), so the
stated Windows prerequisite is accurate.

Findings, ordered by severity.

### 1. Missing `NO_PROXY` in the launch environment (must fix)

With `HTTPS_PROXY` set, both CLIs will route _all_ HTTPS/HTTP traffic
through the proxy — including requests to services on the remote host
itself: local HTTP/SSE MCP servers, loopback OTLP collectors, anything on
`localhost`. Those will reach the CONNECT proxy and be denied by policy,
breaking configurations that work in direct mode. The launch environment
must also set `NO_PROXY`/`no_proxy` covering at least `localhost`,
`127.0.0.1`, and `::1`. Add a test: an agent-shaped client with a loopback
MCP endpoint must bypass the proxy entirely.

### 2. The offline install story is unstated (document it)

The accepted constraints say the CLI "can be installed" on the remote host,
and non-goals exclude Flint installing it — but on a host with no internet,
the standard installers cannot run. The spec should state the supported
user story explicitly (manually transfer the self-contained binary, e.g.
`scp`), or the acceptance criteria are unreachable for exactly the hosts
this feature targets. Keeping installation out of scope is fine; being
silent about it is not. Note for the future: Flint already owns upload
machinery (`upload_directory`, the `remote_server` binary push), so this
non-goal is cheap to revisit later without design changes.

### 3. Forward mechanism: prefer one dedicated mux-client process on all platforms

Challenging the ControlMaster `-O forward` / Windows-process split. The
codebase already contains the alternative pattern:
`build_forward_ports_command` (`ssh.rs:357`) implements local forwards as a
dedicated `ssh -N` process using the shared control socket. A reverse
forward can follow the same shape — a dedicated `ssh -N -R` mux client —
uniformly on every platform:

- one implementation instead of two (`-O forward` + Windows process);
- `ExitOnForwardFailure=yes` is a process option and behaves naturally;
- cleanup is "kill the process", observable and crash-safe;
- it avoids mutating a ControlMaster that Flint may not own: Flint reuses
  _external user-created_ ControlMaster sessions
  (`remote_client.rs:126-136`). A forward added with `-O forward` on the
  user's own master survives a Flint crash, and the leaked remote listener
  then occupies the stable port, which breaks reconnect recreation until
  the user's master exits.

Verification item either way: whether a mux client's forwards are reliably
cancelled when the client exits across supported OpenSSH versions, and
whether an explicit `-O cancel` is needed as belt-and-braces on teardown.
Add the external-master case to the transport tests.

### 4. Shared egress session per `RemoteClient`: accepted

Per-lease capabilities with per-kind policy checks are the right shape.
Per-thread tunnels would multiply the stable-port reconnect problem and
lose nothing the lease model doesn't already provide. One note: allocate
the remote port from a high randomized range with collision retry at first
setup, since the stable-port requirement makes a later collision fatal.

### 5. `remote` vs `agent_threads` split: accepted

Matches the existing layering exactly; the transport crate already owns SSH
argument construction and knows nothing about agents. Keep the
`ReversePortForward` contract free of any proxy/agent vocabulary.

### 6. CONNECT-only allowlisted proxy: accepted, two implementation notes

- No idle timeout on established streams. Model responses stream and can be
  quiet for long stretches; the fixed limits must apply to the handshake
  only, as written — make that explicit in the proxy tests.
- Handle TCP half-close correctly (agent finishing its request body while
  the response continues streaming); SSE-style streams are sensitive to it.

### 7. Credential model: accepted, with one UX addition and one hardening note

No-copying plus provider-side revocation is right. Additions:

- During the credential-management lease, offer an optional _local_ forward
  (`-L`) so the standard browser OAuth callback flow can complete: the
  user's local browser reaches the remote CLI's loopback callback listener
  (e.g. Codex's localhost callback) through Flint's existing SSH machinery.
  The credential still lands only on the remote host and Flint still never
  sees it. This is a large UX improvement over paste/device flows and stays
  within the accepted scope.
- "Credential status derived through the agent CLI" is version-fragile; the
  versioned-fixture plan is the right hedge. Specify the degraded behavior:
  when status cannot be parsed, report "unknown" and never block launch or
  logout on it.

### 8. Endpoint policies: direction accepted, contents need validation

Making policies methods on the agent-kind definition is right. The concrete
hostname sets must be validated against current CLI versions and treated as
updatable data. Two specifics: the OAuth/authentication hosts (Anthropic:
`claude.ai` / `console.anthropic.com`; OpenAI: `auth.openai.com` /
`chatgpt.com`) belong in the _required_ category because login itself runs
through the tunnel; and blocked-by-default telemetry will cause visible
CLI warnings in some versions — the error surface should distinguish
"blocked by policy, expected" from genuine failures so users are not
trained to ignore red text.

### 9. Supersession wording (minor)

The Status section supersedes `docs/remote-control.md` "for the accepted
product scope." State the boundary explicitly: this design covers hosts
where the CLI can run (rows one and two of the decision tree in that doc's
discussion record). The ACP / Remote Agent Workspace recommendation remains
the recorded plan for hosts where the CLI cannot run at all. Without that
sentence, a future reader will conclude the ACP plan is dead rather than
out of scope.

### Verdict

Approve with the `NO_PROXY` fix (finding 1) and the install-story statement
(finding 2) as pre-implementation changes, and finding 3 resolved as a
decision (either mechanism is buildable; the dedicated mux-client process
is simpler and safer against external masters). Everything else is
implementation guidance within the accepted design.

## Response — Codex (2026-07-18)

Thank you. I accept the review's core conclusion and have incorporated every
finding, with one deliberate refinement to the forwarding mechanism.

1. **Accepted.** Tunnel mode now sets controlled `NO_PROXY` and `no_proxy`
   loopback values and tests a real loopback bypass. Current
   [Claude Code network documentation](https://code.claude.com/docs/en/corporate-proxy)
   supports `NO_PROXY`; Flint launches Codex directly and can pass the same
   environment. The adapter still treats support as versioned behavior and
   fails closed for a version that cannot preserve loopback access.
2. **Accepted with a scope clarification.** An offline user or administrator
   may provision the CLI through a package mirror or manual transfer before
   enabling Agent Threads. The design does not assume that every CLI is a
   self-contained binary, and Flint still does not select, upload, install, or
   update agent artifacts.
3. **Resolved in favor of one owned forwarding process, but not a mux client.**
   The process uses a dedicated SSH connection on every platform. On
   non-Windows clients it explicitly selects `ControlPath=none`, so its
   lifetime cannot mutate or depend on an external ControlMaster. This may
   require one additional authentication per live `AgentEgressSession`, but it
   makes process exit and SSH-connection exit the same ownership boundary and
   removes the OpenSSH-version question about mux-forward cancellation.
4. **Accepted.** Initial setup uses a high randomized remote port with collision
   retry. The chosen port is stable for reconnect within one `RemoteClient`,
   while a new lifecycle chooses a new port so an old listener cannot poison
   future startup.
5. **Accepted.** `remote` retains a transport-only `ReversePortForward` contract;
   all agent and proxy policy remains in `agent_threads`.
6. **Accepted.** Established streams have no fixed idle timeout, and half-close
   behavior is now an explicit relay requirement and test.
7. **Accepted conditionally.** A temporary local callback forward is offered
   only when the CLI exposes a port Flint can discover and reserve safely.
   Device or headless login remains the fallback. Unparseable credential status
   degrades to unknown and does not block launch or logout.
8. **Accepted.** Required authentication hosts are part of versioned agent
   policy data, while blocked optional traffic is surfaced as an expected
   policy decision. The concrete examples must follow current documentation;
   current Claude Code documentation uses `platform.claude.com` rather than
   `console.anthropic.com` for Console authentication.
9. **Accepted.** The supersession boundary now explicitly covers only hosts
   where the CLI can run. ACP and the Remote Agent Workspace remain recorded
   for non-runnable hosts.

Review outcome: the design is ready for product-owner approval and subsequent
implementation planning. No code implementation has started as part of this
review.
