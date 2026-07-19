# Shared SSH Reverse Forward Design

**Date:** 2026-07-19  
**Status:** Implemented and verified

## Problem

Through-Flint agent egress currently creates its SSH reverse forward with a
dedicated SSH connection. When the configured SSH destination is a
load-balanced cluster alias, that connection can terminate on a different
login node from Flint's project connection. The remote Codex process then
receives a loopback proxy URL for a listener that does not exist on its node and
fails with `Connection refused`.

The live reproduction had these properties:

- the Flint project connection and Codex process were on `login04`;
- Flint's local CONNECT proxy was listening successfully;
- the dedicated reverse-forward process was still alive; and
- the advertised remote proxy port had no listener on `login04`.

A temporary reverse forward installed through Flint's existing OpenSSH
ControlMaster appeared on `login04` and accepted explicit cancellation. This
confirms that the failure is connection placement, not provider connectivity,
proxy policy, or proxy authentication.

## Goals

- Install POSIX-client reverse forwards on the exact SSH connection used by the
  remote project.
- Preserve dynamic remote-port allocation and loopback-only binding.
- Keep reverse-forward lifetime owned by `RemotePortForward`.
- Remove the forward when its handle is explicitly closed or dropped.
- Return actionable setup and explicit-close errors without exposing command
  environments or proxy capabilities.
- Leave local OAuth callback forwards and non-SSH transports unchanged.

## Non-goals

- Adding shared-connection forwarding to Windows OpenSSH in this change.
- Adding a new RPC byte-forwarding protocol.
- Discovering or persisting a cluster's concrete backend hostname.
- Restoring agent egress automatically after an SSH disconnect.

Windows retains the current dedicated reverse-forward implementation until a
separate cross-platform transport design is reviewed.

## Considered Approaches

### Add the forward to Flint's existing OpenSSH ControlMaster

This is the selected approach for macOS and Linux. It guarantees that the
loopback listener is created in the same remote network namespace as the
project and agent processes. OpenSSH supplies explicit `forward` and `cancel`
control operations, so Flint does not need to infer the backend hostname.

### Resolve and reconnect to the selected backend hostname

This is rejected. Backend discovery is cluster-specific, concrete login-node
names may not be directly reachable, and authentication or jump-host policy
can differ from the load-balanced alias.

### Carry proxy bytes through Flint's remote-server RPC

This would be transport-independent and could cover Windows, but it adds a new
stream-multiplexing protocol and more failure and flow-control behavior. It is
unnecessary for the approved macOS/Linux-first scope.

## POSIX Forward Lifecycle

On macOS and Linux, reverse-forward creation runs a one-shot OpenSSH control
operation against Flint's existing socket:

```text
ssh <configured options> \
  -o ControlMaster=no \
  -o ControlPath=<Flint socket> \
  -O forward \
  -o ExitOnForwardFailure=yes \
  -R 127.0.0.1:0:127.0.0.1:<local proxy port> \
  <destination>
```

OpenSSH prints the dynamically allocated remote port and exits, while the
forward remains owned by the existing master connection. Flint parses and
validates that port before returning a ready handle.

The handle retains a cancellation specification containing the SSH socket,
configured SSH arguments, destination, and the original reverse-forward
request. Cancellation uses the original dynamic request rather than replacing
port zero with the allocated port:

```text
ssh <configured options> \
  -o ControlMaster=no \
  -o ControlPath=<Flint socket> \
  -O cancel \
  -R 127.0.0.1:0:127.0.0.1:<local proxy port> \
  <destination>
```

The original request form is required by OpenSSH's multiplex-forward registry;
live validation showed that cancelling with the allocated port does not match
the registered forward.

`RemotePortForward::close` awaits the cancellation command and returns a
failure if OpenSSH rejects it. Dropping an unclosed handle starts the same
cancellation operation on the background executor and logs any failure. The
cancellation state is taken exactly once so explicit close and drop cannot
issue duplicate requests.

Killing a normal multiplexed `ssh -N -R` child is not used as cleanup. Live
validation showed that the forward remains registered in the ControlMaster
after that child exits.

## Windows Behavior

The existing dedicated `ssh -N -R` process remains in use when Flint runs on
Windows. Windows OpenSSH does not provide the ControlMaster socket used by this
design. This preserves existing behavior but does not guarantee same-node
placement for load-balanced SSH aliases; that limitation must remain explicit
until a Windows transport design is implemented.

## Error Handling

Forward creation fails before launching an agent if:

- the shared ControlMaster is unavailable;
- OpenSSH rejects reverse forwarding;
- the operation times out or exits before returning a valid port; or
- the allocated port output is malformed.

Explicit cancellation propagates OpenSSH failures. Drop-time cancellation
cannot report to a caller, so it logs a redacted error after observing the
child's exit. Commands and errors must not print process environments or proxy
capability URLs.

If the project SSH master disconnects, its forwards disappear with the
connection. Automatic forward recreation remains outside this change.

## Testing

Tests follow red-green-refactor and cover the production command-building seam:

- POSIX reverse-forward creation uses Flint's shared `ControlPath` and never
  `ControlPath=none`;
- creation uses `-O forward`, a loopback-only dynamic `-R` request, and
  `ExitOnForwardFailure=yes`;
- cancellation uses `-O cancel` and the original port-zero request;
- dynamic allocated-port output is accepted and malformed output is rejected;
- explicit close runs cancellation once and propagates failure;
- dropping a handle schedules cancellation once;
- the Windows dedicated command remains unchanged under Windows compilation;
  and
- local callback-forward behavior remains dedicated and unchanged.

The focused `remote` transport tests run first, followed by the full `remote`
and `agent_threads` suites, formatting, and Flint clippy checks.

## Acceptance Criteria

- A Through-Flint Codex process on a load-balanced SSH destination can reach its
  Flint proxy without `Connection refused` while its project connection is
  healthy.
- The remote proxy listener is visible on the same host as the Flint project
  connection.
- Closing the last egress lease removes the reverse listener from the shared
  SSH master.
- Starting another egress session after cleanup allocates one new listener and
  does not reuse stale cancellation state.
- Not-through-Flint launches remain unchanged and receive no proxy variables.
