# Managed Agent Resume Routing Design

**Date:** 2026-07-19
**Status:** Approved in discussion; pending written-spec review

## Problem

Flint can install a pinned official Codex CLI on an SSH remote and launch it by
absolute path. Historical resume currently discards that executable choice and
rebuilds the command from the global Agent Threads setting. On an offline host
without ambient `codex` on `PATH`, a session originally launched with
Flint-managed Codex therefore fails to resume with:

```text
env: codex: No such file or directory
```

The network route is a separate concern. A remote project opened with
**Through Flint** must launch official Codex with Flint-provided egress even if
the remote host has direct internet. Flint must never probe connectivity,
silently choose direct networking, or fall back to direct networking after a
tunnel failure. A project opened with **Direct** must launch without
a Flint egress lease or Flint proxy environment.

Selecting **New — Flint-managed Codex** again can also look like a second
download or upload while Flint hashes and validates an existing 299 MB remote
installation. Reuse must be distinguishable from provisioning in the UI.

## Scope

This change covers manual historical resume and automatic session restoration
for official Codex on SSH remotes. It also clarifies managed-install reuse and
its progress states.

It does not:

- add OS-enforced network isolation, a remote firewall, a container, or a
  network namespace;
- change new-thread choices other than reusing the existing managed
  provisioning service;
- infer a route from remote connectivity;
- copy a managed executable per thread;
- suppress the remote shell's locale or environment-module startup warnings;
- automatically restore a tunnel after an SSH disconnect; or
- reroute an already running process.

## Product Invariants

### Route is authoritative at launch

Every manual resume and automatic restoration reads the current route for the
active SSH connection identity.

- **Through Flint** requires Flint-managed Codex and a live Flint egress lease.
  Flint launches nothing until both are available. Provisioning failure,
  tunnel failure, route change, or proxy setup failure produces an error and no
  process. There is no ambient-executable or direct-network fallback.
- **Direct** uses the configured or ambient Codex command. Flint does
  not resolve or provision a managed executable, acquire an egress lease, or
  inject Flint proxy variables.

The route used when the historical session was created is irrelevant. The
current host route controls the next process.

The Through-Flint guarantee is a Flint launch guarantee for the supported
official Codex CLI. Flint supplies the required proxy configuration and does
not implement a fallback. It is not a security boundary against a modified or
malicious executable deliberately opening direct sockets through a remote host
that has its own internet access.

### Managed installation is shared

Flint installs one pinned executable per remote identity, agent, version, and
platform. All matching threads launch separate processes from the same verified
path, for example:

```text
~/.local/share/flint/agents/codex/0.144.6/linux-x86_64-glibc/codex
```

Threads do not receive private executable copies. Each thread retains its own
terminal lifecycle, session ID, and `AgentEgressLease`. Leases may share the
connection-scoped proxy and SSH reverse forward already managed by
`AgentEgressManager`. The remote user's Codex credential store is also shared
normally between those processes.

New pinned releases install side by side. Already running processes are not
replaced. A later resume resolves the release pinned by the running Flint
version.

## Resume Policy

Resume command construction is split into two responsibilities:

1. Resolve the executable source required by the current route.
2. Launch that executable under the same route, after verifying the route has
   not changed during asynchronous preparation.

For **Direct**, the history provider continues to build its resume
arguments from the configured `AgentLaunchCommand`. The resulting command is
launched without egress.

For **Through Flint**, Flint resolves the pinned release for the detected remote
platform, ensures the shared managed installation, replaces the command with
the verified absolute path, and then appends the history provider's resume
arguments and the agent's self-update policy. It acquires egress, applies the
proxy environment, verifies that the route is still **Through Flint**, and only
then creates the terminal process.

Codex history does not record executable provenance and Codex cannot currently
be assigned a session ID by Flint at fresh launch. The design therefore does
not attempt unreliable per-session provenance inference. The current route is
the explicit and sufficient resume policy.

## Managed Resolution and Progress

Manual resume and startup restoration use the same managed-resolution
operation.

1. Resolve the pinned release and expected remote path.
2. Show **Checking installed Codex** while validating the receipt, executable
   digest, and reported version.
3. If the remote installation is valid, show **Reusing installed Codex** and
   return its absolute path without downloading, uploading, or reinstalling.
4. If the installation is missing or invalid, validate the local cache.
5. If the artifact is not cached, prompt **Download the official Codex CLI
   v…?**. This prompt is shown for both manual resume and automatic startup
   restoration.
6. On confirmation, download and verify the official pinned artifact locally,
   upload it once, verify it remotely, and install it transactionally through
   the existing receipt and rollback protocol.
7. Return the verified managed absolute path.

Cancellation leaves the historical session available for another resume.
Refusing the startup prompt skips that restoration attempt and reports it as a
non-restored session; it does not switch routes or commands.

Progress states must distinguish:

- checking the remote installation;
- reusing the remote installation;
- checking or verifying the local cache;
- awaiting download confirmation;
- downloading;
- uploading;
- verifying the uploaded executable;
- installing; and
- resuming the session.

The notification remains non-suppressible while work is active. A completed
reuse state may be brief, but must be observable and must never use
**Downloading** or **Uploading** wording.

## Concurrency

Provisioning remains single-flight for one remote identity, agent, pinned
version, and platform. Repeated manual clicks re-show the active notification
and do not create another provisioning operation or duplicate resumed process.

Automatic restoration processes matching sessions sequentially. The first
session may prompt and provision. Later sessions validate and reuse the same
receipt-backed installation. This avoids multiple startup prompts and makes one
installation result authoritative for the restoration batch.

Local artifact acquisition retains its digest-keyed lock, so another workspace
or remote cannot download the same pinned artifact concurrently.

## Route Races and Lifecycle

Managed validation and provisioning are asynchronous. Before process creation,
Flint compares the current connection route with the route that selected the
managed resume path.

If the route changed, Flint drops any acquired egress lease, creates no
terminal, and reports that the route changed while the session was being
prepared. The user can resume again under the new route. Flint does not carry a
managed command prepared for **Through Flint** into a **Direct**
launch, or vice versa.

The `AgentEgressLease` is stored in the live Agent Thread entry and remains
alive until the terminal closes. If SSH or its forwarding process disconnects,
the affected thread must be restarted. Automatic tunnel restoration remains
outside this change.

Changing the route for a host continues to close affected live Agent Threads.
Focusing an already live historical row only focuses its terminal; it does not
launch, provision, or reroute anything.

## Error Handling

Errors are reported at the stage that failed and never converted into a route
or executable fallback.

- Unsupported remote platform: no process; report that no pinned release is
  available.
- Download declined or cancelled: no process; retain the historical row.
- Local verification failure: no upload or process.
- Remote receipt, digest, version, upload, or transactional-install failure: no
  process; retain the last valid installation through existing rollback rules.
- Egress or reverse-forward failure: no process; do not attempt direct network.
- Route changed during preparation: no process; request a new resume action.
- Configured or ambient command missing under **Direct**: preserve
  current command failure behavior; do not switch to managed Codex.

Locale and environment-module diagnostics emitted by the remote login shell are
independent noise and do not determine managed-install validity. Cleaning those
messages up requires a separate shell-environment design.

## Testing

Regression tests must cover the real resume and restoration seams.

### Policy and launch tests

- Manual **Through Flint** resume uses the pinned managed absolute path, keeps
  the provider's session ID and resume options, applies self-update policy,
  acquires egress, injects the Flint proxy environment, and retains the lease.
- Automatic **Through Flint** restoration uses the same managed and routed
  launch path.
- **Direct** resume uses the configured or ambient command and has
  no managed resolution, egress lease, or Flint proxy variables.
- A route change during managed preparation creates no terminal.
- Managed-resolution or egress failure creates no terminal and does not retry
  through another route or executable.

### Reuse and concurrency tests

- A matching receipt, digest, and version returns the existing absolute path
  with no download, upload, staging directory, or commit.
- Status transitions distinguish checking and reuse from download and upload.
- An uncached artifact prompts during manual resume and startup restoration.
- Declining the prompt leaves the session unrestored and retryable.
- Multiple automatic restores cause at most one download and installation,
  then reuse the shared executable for every process.
- Repeated manual clicks during provisioning show the active operation without
  starting a second provision or resumed terminal.

### Regression suites

Run the Agent Threads and remote-server library tests, formatting, Flint's
clippy wrapper for affected crates, and the Linux musl remote-server build used
by the debug application bundle.

## Live Validation

Build and install a fresh `/tmp/Flint-Local.app`, preserve the prior bundle, and
open the existing offline SSH project.

1. Set the host to **Through Flint**.
2. Resume a known Codex historical session.
3. Confirm the status says the installed Codex is being checked and reused,
   with no local cache or remote executable timestamp change.
4. Confirm the remote process uses the managed absolute path and the expected
   resume arguments.
5. Confirm the process receives Flint's proxy environment and is owned by a
   live egress lease.
6. Confirm an egress-start failure creates no Codex process.
7. Set the host to **Direct** and confirm resume uses the configured
   command without Flint proxy variables or a reverse-forward lease.

## Alternatives Rejected

### Persist original per-session executable provenance

This cannot reliably cover existing Codex history or fresh managed Codex
sessions because Flint does not know the CLI-generated session ID at process
creation. Correlating later history by time or project would be ambiguous.

### Add a separate managed resume action

A **Resume — Flint-managed Codex** action leaves ordinary resume able to launch
bare `codex` under **Through Flint**, contradicting the approved invariant.

### Prefer managed only when ambient command lookup fails

This makes behavior depend on remote availability and allows a host's ambient
installation to replace the pinned executable under **Through Flint**. It also
requires a speculative probe before every resume. The route is the explicit
policy and must determine the source without fallback.

### Enforce networking with a firewall or namespace

OS-enforced isolation would require host-specific privileges or facilities and
is outside the accepted Flint launch guarantee. Flint configures the supported
official CLI to use its required proxy and fails closed within the launch
workflow.
