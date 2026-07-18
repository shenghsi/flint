# Remote Agent Egress Design

## Status

Proposed for Claude's review on 2026-07-18.

Design owner: Codex.

This document supersedes the Remote Agent Workspace and ACP recommendation in
`docs/remote-control.md` for the accepted product scope. That discussion remains
the decision history.

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

For non-Windows local OpenSSH clients, the SSH implementation manages the
forward through Flint's existing ControlMaster socket and cancels it through
the same socket. For the Windows OpenSSH client, which lacks ControlMaster
support, Flint owns a dedicated long-lived SSH forwarding process. Both
implementations use the same `ReversePortForward` contract.

The SSH arguments must:

- bind the remote listener to `127.0.0.1`;
- set `ExitOnForwardFailure=yes`;
- connect only to the local proxy's loopback listener;
- preserve configured jump hosts, identity files, ports, and askpass behavior;
- keep stderr available for an actionable startup or runtime error.

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
that variable.

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

The preferred credential is named for one remote host and one agent and has the
shortest practical expiration. Supported examples include:

- a project-scoped API key that can be deleted independently;
- a time-limited Codex workspace access token where available;
- a separately listed Claude Code authorization token.

Flint derives credential status on demand through the agent CLI. It keeps only
non-secret runtime state and the configured network mode; it does not persist a
parallel credential inventory. Flint does not promise that a credential is
dedicated when the provider or CLI does not expose enough metadata to verify
that claim.

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
- Stop an established stream when its lease or egress session is cancelled.
- Confirm logs and errors never include capability or authorization values.

### Remote transport unit tests

- Build a loopback-only reverse forward with `ExitOnForwardFailure=yes`.
- Preserve configured SSH port, jump host, identity, and askpass arguments.
- Use ControlMaster operations on supported local clients.
- Use an owned forwarding process on the Windows OpenSSH client.
- Report readiness, forwarding denial, unexpected exit, and cancellation.
- Attempt cancellation on drop and surface cleanup failures through logging.

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
- The remote project directory remains the process working directory.
- POSIX and Windows remote launch paths preserve the proxy environment.
- Credential status and logout commands are covered by versioned fixtures for
  Codex and Claude Code.
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
2. Implement SSH reverse forwarding and cleanup on non-Windows local clients.
3. Add the restricted CONNECT proxy and its security tests.
4. Add connection-scoped egress leases and readiness behavior.
5. Integrate tunnel-mode preparation into remote Agent Thread launch.
6. Add agent destination policies and credential status/logout actions.
7. Add reconnect behavior and failure UI.
8. Add Windows local-client forwarding and Windows remote environment support.
9. Run the SSH integration suite and manually validate Codex and Claude Code
   with dedicated test credentials.

Each step keeps direct-mode Agent Threads working and independently testable.

## Acceptance Criteria

- On an SSH host with no direct internet, an installed and authenticated Codex
  or Claude Code CLI can complete model requests through Flint.
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

## Review Requests for Claude

Please challenge these decisions in particular:

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
