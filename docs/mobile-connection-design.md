# Flint Mobile Connection Layer

## Context

The goal is to "vibe code" from an iPhone: reach into a coding-agent terminal
session running on a Flint desktop, from anywhere, without manually
configuring port forwarding, dynamic DNS, or a VPN. The phone should be able
to pair with more than one desktop independently.

Two things about Flint's existing shape constrain this design:

- `agent_threads` already runs Codex/Claude as terminal subprocesses of the
  GUI workspace (`SpawnInTerminal`/`TerminalView`), not as a process Flint
  understands the protocol of.
- `docs/terminal-first-fork.md` states the fork's product philosophy
  explicitly: Flint should not become "another native AI client." Agent
  authentication, permissions, tools, and model selection belong to the CLI
  or TUI running inside the terminal, not to Flint.

This document covers only the connection layer between the phone and a
paired desktop: transport, pairing/trust, and payload shape. The running
Flint GUI process is the **connection host** — it hosts the Iroh endpoint
and the PTYs being streamed. This is the intended MVP, not a stopgap:
remote access works while Flint is running. Extracting the host role into a
standalone headless daemon is explicitly deferred to a later phase (see
Rollout phases) and is *not* a prerequisite — it's pursued only if users
need sessions to survive Flint quitting or restarting, and it's a separate
redesign of `agent_threads`' process model that earns its own spec.

## Goals

- A paired phone can reach a specific Flint desktop's terminal sessions from
  any network (cellular, a different building, etc.), not just the same LAN.
  Production-grade reachability when no direct path exists depends on relay
  choice; see Decisions (Relay hosting) for the explicit caveat on this.
- One phone can be independently paired with multiple desktops.
- No Flint account or third-party account is required of the end user to
  pair or connect.
- Whatever relays traffic (including infrastructure Flint itself operates)
  cannot read session contents in transit.
- The connection carries terminal I/O — grid/byte output down, keystrokes
  and resize events up — plus a minimal terminal-multiplexer-level control
  protocol (list sessions, attach/detach, deliver an initial snapshot,
  report exit). It has no protocol-level concept of "agent," "message," or
  "permission prompt." Session management is terminal-level, not agent-level
  (tmux has the same and understands nothing about agents), so this keeps the
  connection layer, and Flint, agnostic to whatever CLI is running,
  consistent with the terminal-first philosophy above.

## Non-Goals

- Decoupling PTY/endpoint ownership from the GUI into a standalone headless
  daemon. This is a deferred later phase (see Rollout phases), pursued only
  if sessions must survive Flint quitting / restarting / updating — not a
  limitation this design works around. For the MVP, the Flint GUI *is* the
  connection host.
- The iPhone app's UI/UX, including how it renders or interacts with
  terminal output.
- Any parsing or understanding of agent-specific semantics (permission
  prompts, structured messages, diffs) at the protocol level.
- Account-based pairing or cross-device discovery. Considered and rejected
  in favor of per-machine QR pairing (see Decisions).

## Decisions

### Transport: Iroh, not Tailscale

Reaching a desktop from an arbitrary network means both endpoints are
typically behind NAT, so some internet-reachable component for connection
setup is unavoidable. [Iroh](https://www.iroh.computer) (a Rust QUIC-based
P2P library) was chosen over Tailscale for three reasons specific to this
codebase:

- It's Rust-native and embeds directly via the `iroh` crate, driven through
  the existing `gpui_tokio` crate. Tailscale's `tsnet` is Go, requiring a
  foreign-runtime embed on both desktop and iOS.
- Each device gets a stable identity (an Ed25519 keypair, its Endpoint ID)
  with no control-plane account. Tailscale requires either depending on a
  third-party company's API/ToS to provision tailnets and ACL policy per
  pairing programmatically, or self-hosting `headscale` — both heavier than
  this problem needs.
- Iroh's pairing primitive (an `EndpointTicket`: endpoint ID + address
  hints, meant to be shared out-of-band) maps directly onto the per-machine
  QR pairing model below. There's no shared network to scope with ACLs, so
  there's no "ACL policy bug = cross-tenant leak" failure mode to design
  around.

Pin to **iroh 1.0** and the `iroh-tickets` crate for `EndpointTicket`. The
API renamed `NodeId`/`NodeAddr`/`NodeTicket` to
`EndpointId`/`EndpointAddr`/`EndpointTicket` in 0.94 and moved generic
tickets out of `iroh-base` into `iroh-tickets`; this document uses the
current (1.0) names throughout. The exact QR payload is defined under
Pairing below rather than relying on an unspecified "ticket."

The trade-off accepted: Iroh and its operator (n0) are younger and smaller
than Tailscale, with a less battle-tested NAT-traversal track record.

### Connectivity: P2P first, relay as fallback

Two precedents exist for this exact problem (mobile companion apps for
Claude Code): Happy and Paseo always relay through a server, even when a
direct path exists; LitterKitty's P2P piece (`alleycat`, also Iroh-based)
attempts a direct connection first and only relays when NAT traversal fails.
We follow the latter: lower latency and lower relay bandwidth cost in the
common case, at the cost of two connectivity scenarios to test (direct
succeeds vs. falls back) instead of one. Either path is end-to-end encrypted
between the two devices' Endpoint IDs; a relay (when used) only ever forwards
undecryptable QUIC packets.

**The relay plays two roles, and "fallback" only describes the second.**
Iroh doesn't choose direct-or-relay up front — it always starts on the relay
and upgrades opportunistically:

1. **Coordination (always):** the relay is how the two devices first find
   each other and begin NAT hole-punching, and traffic flows through it from
   the first byte, so the connection works immediately.
2. **Fallback data path (only if hole-punching fails):** Iroh then tries to
   upgrade to a direct device-to-device link. If it **succeeds**, the relay
   steps aside and bytes flow P2P. If it **fails** (symmetric NAT, locked-down
   corporate/hotel networks), the connection simply stays on the relay as its
   ongoing data path.

This is why the relay is in the critical path even for connections that end
up direct, and why its availability matters (see Relay hosting). Iroh's
`Endpoint` handles this upgrade transparently; we don't implement two
separate transports.

### Relay hosting: n0's public relays for development; dedicated/self-hosted required before production

Iroh's default relay set (the `n0` preset) requires no registration or API
key and is free to use, which makes it the right starting point for early
development. But n0's own documentation is explicit that the public relays
"carry no uptime or performance guarantees," are shared across all iroh
developers worldwide, and are "suitable for development and testing" — for
production they recommend dedicated relays (managed Iroh Services or
self-hosted).

This matters because the relay path is the *only* fallback when a direct P2P
connection can't be established (hard/symmetric NAT, restrictive corporate
or hotel networks). If we shipped a product depending on public relays, the
primary "from anywhere" promise could degrade or stop working with no
recourse. So this is a hard gate, not a "revisit if it becomes a problem":

- **Development / early stages:** public `n0` relays.
- **Before production:** switch to self-hosted `iroh-relay` (a single
  stateless binary with no control-plane state) or managed dedicated relays.

Switching is a config change (relay URLs), not a redesign, so deferring the
*work* is fine — but the production reliability goal is explicitly
conditional on completing that switch.

### Pairing: per-machine QR code, not account-based discovery

Considered binding pairing to an existing Flint account (sign into the same
account on phone and desktop, machines auto-listed). Rejected: it would
make this feature depend on Flint's account system supporting multi-device
session listing, which doesn't exist today, and it's a heavier mental model
than "scan this code" for what is fundamentally a one-time, per-machine
trust decision — closer to pairing a smart-home device than signing into a
synced account.

Each desktop generates a QR code on demand. The QR payload is:

- The desktop's `EndpointTicket` (its Endpoint ID plus current address
  hints / relay URL), serialized via `iroh-tickets`.
- A short-lived (~2 minute), single-use pairing token (high-entropy random
  bytes generated per QR display).

The phone parses the ticket, dials the desktop's Endpoint ID directly over
Iroh, and sends the pairing token over the now-encrypted stream.

### Trust: explicit allow-list, not "ticket equals access"

A bare `EndpointTicket` (Endpoint ID + address hints) is not a secret —
anyone who captures a copy of the QR code (e.g. a photo) within its validity
window could otherwise dial the same Endpoint ID. To close that gap:

1. The desktop checks the pairing token sent over the stream against the
   one it generated, and invalidates it immediately on first use (whether
   or not the connection is ultimately approved).
2. The desktop still requires one explicit "Allow this device?" confirmation
   from the user before trusting the connecting phone, even on a token
   match.
3. Only after that confirmation does the phone's own Endpoint ID get written
   to a persistent local allow-list on the desktop. Reconnects after that
   check the allow-list directly; no re-scan needed.

**Authorization is enforced per stream, not just at connection time.** Every
incoming stream open re-checks the connecting Endpoint ID against the
allow-list, so authorization can't be granted once at connect and then
relied on indefinitely.

**Revocation terminates existing access immediately.** Settings expose the
allow-list so a pairing can be revoked at any time. Revoking an Endpoint ID
both removes it from the allow-list *and* force-closes any currently open
connections and streams from that identity — otherwise revocation would only
affect future connections and an already-attached phone would keep its
session. Closing the Iroh connection on revoke, plus the per-stream check
above, closes that race.

Each pairing is fully independent — pairing with N desktops is N allow-list
entries dialed from a single Iroh `Endpoint` on the phone, not a shared
network requiring access-control policy to get right.

### Payload: terminal I/O over a minimal terminal-multiplexer control protocol

The connection's job is "attach to a remote terminal session," the same as
tmux/screen — and like tmux, that needs a small control protocol *around*
the raw I/O, even though the I/O itself is agent-agnostic. Raw PTY bytes
alone cannot list which sessions exist, attach to a specific one, tell a
just-connected client what's already on screen, or report that a session
exited. So the protocol defines, at minimum:

- **List / attach / detach:** sessions are addressed by an opaque terminal
  ID; the client lists available sessions and attaches to one (or more).
- **Initial snapshot on attach:** a just-attached client is brought up to the
  current screen state, then receives incremental updates. The snapshot is
  not raw bytes (Flint retains no replayable byte stream) and not plain text;
  see "Snapshot and incremental updates" below for what it actually contains.
- **Framed output / input:** incremental display updates (grid deltas)
  downstream; keystrokes upstream.
- **Exit status:** the session reports when its process exits, so clients
  don't hang on a dead terminal.

Crucially, this control protocol is *terminal-level, not agent-level*. It
has no concept of "agent thread," "message," or "permission prompt" — those
remain just terminal text that the running CLI renders, exactly as on the
desktop. This is the explicit alternative to building a richer protocol that
parses agent semantics, which would functionally make Flint a native AI
client and contradict the philosophy in `docs/terminal-first-fork.md`.

### Snapshot and incremental updates: the desktop is the authoritative emulator

A grid of text alone is not enough state to reconstruct a terminal on a
freshly attached client. The emulator also carries terminal modes,
alternate-screen state (anything full-screen like `vim`/`less` lives on the
alt screen), saved cursor, tab stops, and per-cell styling/attributes. The
`last_non_empty_lines` helper returns plain `String`s and drops all of that,
so it is *not* a viable snapshot foundation — it was only ever a convenience
for reading visible text.

Two structurally different ways to keep a client in sync were considered:

- **(A) Authoritative desktop emulator + structured grid sync (chosen).** The
  desktop's `alacritty_terminal::Term` (`AlacrittyTerm = Term<FlintListener>`,
  `crates/terminal/src/alacritty.rs`) remains the single source of truth. On
  attach it serializes a *structured* snapshot — the visible cell grid with
  styling, cursor, active screen (primary/alt), relevant modes, and
  dimensions — and thereafter sends structured deltas (changed cells plus any
  changed non-cell state; see requirements below). The phone is a thin
  renderer of cells; it runs no emulator and never parses raw PTY bytes.
- **(B) Emulator checkpoint + ordered raw PTY bytes.** Send a full
  emulator-state reconstruction, then forward subsequent raw PTY bytes for a
  phone-side emulator (e.g. SwiftTerm) to parse.

(A) is chosen because (B) requires the phone's emulator to reproduce
alacritty's parsing behavior exactly or the two displays silently diverge —
a correctness risk this codebase's "correctness over efficiency" guideline
weighs against. (A) costs more bandwidth on high-throughput output, which is
the accepted trade-off. The cross-emulator approach stays documented as a
fallback if grid-sync bandwidth proves untenable.

This model has several requirements that aren't free, called out because the
obvious implementation gets each wrong:

**A single versioned producer that owns damage, driven by terminal events
(not GUI frames).** Flint does not currently use alacritty's damage API (no
`Term::damage`/`reset_damage` in `crates/terminal`); the GUI reads grid
content afresh each render. The remote fan-out producer *introduces* damage
tracking and is its **sole** consumer — alacritty's damage is global
accumulated state with no version and no per-subscriber cursor, so it
tolerates exactly one reader that reads-and-resets; a second consumer calling
`reset_damage` would clear the first's changes and silently drop updates.
Two consequences:

- The desktop GUI does **not** become a subscriber to this stream. Resetting
  damage doesn't remove grid content, so the GUI keeps reading the
  authoritative `Term` directly, exactly as it does today. Only damage
  *tracking* is exclusively owned by the producer; the network sync stays out
  of the desktop render path entirely. (Reworking desktop rendering into the
  sync model would expand scope without improving correctness.)
- The producer is driven by terminal output / wakeup events with its own
  batching and coalescing — **not** by the GUI's render cadence. GUI frames
  stop or throttle precisely when all windows are closed, minimized, or
  occluded, which is exactly when a remote phone may be the only viewer; if
  updates were generated "once per frame" the phone would freeze in that
  case. Each batch is stamped with a monotonically increasing version the
  producer maintains itself and fanned out to every remote subscriber; new
  attaches register with it.

**Every update carries changed non-cell state, not just cells.** Cursor
movement, mode changes, alt-screen transitions, and resize can happen
without cell damage, and alacritty explicitly excludes selection and the vi
cursor from damage tracking. So each fanned-out update includes a small
metadata header (current cursor, modes, active screen, dimensions) alongside
any changed cells — a client must never reconstruct state from damaged cells
alone.

**Bounded per-client queues with resync on overflow.** A slow or
unreachable phone must never block PTY processing or grow an unbounded delta
backlog. Each subscriber has a bounded queue; the producer never waits on a
subscriber's acknowledgement. On overflow, that subscriber's queue is
dropped and it is flagged to receive a fresh snapshot at the current version
(a resync), or disconnected if it repeatedly can't keep up. The
PTY→emulator path is never gated on any client.

**Bounded initial scrollback, paged older history.** Terminal history is
configurable up to `MAX_SCROLL_HISTORY_LINES` (100,000; default 10,000 —
`crates/terminal/src/terminal.rs`), far too large to ship on every attach.
The initial snapshot includes the visible screen plus a bounded recent
window of scrollback; older history is fetched on demand (paged) when the
user scrolls up, not sent eagerly.

**Atomic attach boundary.** Because attach is handled inside the single
producer, the snapshot is emitted tagged with the current version V and the
client then receives exactly the updates with version > V — no update
produced during attachment is lost or double-applied, even when multiple
clients attach concurrently.

### Input and resize ownership: a single active-controller lease

A PTY has exactly one row/column size and one input stream, but the desktop
window and one or more phones can be attached at once. Without an owner,
viewers' dimensions would fight (constant reflow and `SIGWINCH` churn) and
interleaved keystrokes would corrupt input. So exactly one attached viewer
holds an **active-controller lease** at a time. The lease governs *both*
write-side concerns together — keystroke input *and* the dimensions that
drive the PTY size. Output (the grid sync above) always goes to every
attached viewer regardless of who holds the lease; non-holders are read-only
and scale the grid to their own display as a rendering concern, not a PTY
resize.

Lease transfer:

- **Explicit handoff:** a viewer requests the lease; the current holder
  yields.
- **Automatic acquisition when the current holder is inactive.** The common
  case is an unattended desktop: if the desktop window is unfocused or idle,
  it does not hold the lease hostage — a requesting phone acquires it without
  needing someone present at the desktop to yield. Otherwise a phone could
  never become interactive while away from the machine. A holder that becomes
  unreachable also forfeits the lease on timeout.

The desktop GUI is the default holder for sessions it spawned *while it is
the active/focused viewer*; it relinquishes per the rules above.

### Where the endpoint lives

The Iroh endpoint and the PTY it streams must live in the same process,
since attaching a remote viewer to a PTY only works where the PTY actually
is. Today, agent terminal sessions are subprocesses of the GUI
(`SpawnInTerminal`), so the endpoint is embedded in the GUI process. It must
be listening in two situations, not just one:

- **While a pairing QR/token is active.** First-time pairing requires the
  phone to dial the desktop, so the endpoint has to be up *before* any device
  is paired — it starts when the user opens the pairing screen and a QR is
  displayed, and can stop when that pairing window closes if nothing else is
  paired. (Without this, pairing is a chicken-and-egg deadlock: the phone
  can't dial an endpoint that only activates after pairing.)
- **Whenever at least one device is already paired**, so reconnects work.

A settings toggle can disable it entirely. This is the MVP model — the
running Flint GUI *is* the connection host, by design, not a stopgap:
closing windows is fine, remote access works while Flint runs, and quitting
goes offline (see lifecycle below). A future headless daemon that owns PTYs
independently of the GUI would let the endpoint move there instead (Rollout
phases, phase 3) — this design doesn't need to change for that to happen,
only its embedding location does.

### Process lifecycle and reachability states

Because reachability is tied to the host process being alive, the lifecycle
behavior must be specified, not incidental:

- **Closing the last window does not quit Flint.** The process keeps running
  in the background and paired devices stay reachable. This is the default on
  macOS; on Windows/Linux it requires deliberately keeping a background/tray
  presence instead of exiting on last-window-close.
- **Quitting Flint takes the machine offline** for remote access — the
  endpoint and sessions stop. This is the explicit, user-initiated "go
  offline" action, not a bug.
- **Sleep and network changes trigger reconnection.** On wake or network
  change the endpoint re-establishes connectivity (Iroh re-discovers
  paths/relays) and the phone re-dials, rather than treating a dropped
  connection as terminal failure.
- **Optional "Launch Flint at login"** setting, so a rebooted machine becomes
  reachable again without someone manually reopening the app.

The phone should present distinct states rather than collapsing everything
into a generic "can't connect," because each implies a different user action:

| State | Meaning | What the user does |
|---|---|---|
| Connecting | Dial / path establishment in progress | Wait |
| Connected | Reachable, attached | — |
| Disabled | Remote access toggled off on that desktop | Re-enable in desktop settings |
| Offline | Flint quit / not launched | Relaunch the app (or rely on launch-at-login) |
| Sleeping | Machine asleep, expected to return | Wait / wake the machine |

Honest caveat: from the phone's vantage point only *Connecting*, *Connected*,
and a generic *unreachable* are directly observable. Splitting "unreachable"
into Disabled / Offline / Sleeping needs an extra signal — e.g. last-known
status cached from the previous session, or relay-side presence (whether the
desktop's endpoint is currently connected to the relay). The exact mechanism
is an open question (see below) and must not weaken the dumb-relay / E2E
properties.

### Identity key storage and reset

Pairing trust depends on each device's Endpoint ID being stable, which means
its Ed25519 private key must persist and be protected:

- **Storage:** the private key lives in OS-protected storage — the macOS
  Keychain on desktop, the iOS Keychain on the phone — not a plaintext file.
  This mirrors how `credentials_provider` already stores secrets in this
  codebase. Note the key cannot be *Secure Enclave-backed*: the Secure
  Enclave only supports P-256 signing keys, while Iroh identities are
  Ed25519. The Ed25519 key is therefore stored as a standard (exportable)
  Keychain item guarded by an appropriate access-control / data-protection
  class (e.g. accessible only after first unlock, this-device-only), rather
  than as a non-exportable Secure Enclave key.
- **Key loss = new identity = re-pairing.** Reinstalling the app, wiping the
  keychain entry, or restoring to a device where the key didn't transfer
  produces a new Endpoint ID. Because the old ID is what's on each desktop's
  allow-list, the user must re-scan to re-pair. This is acceptable (it's a
  rare event and fail-closed) but must be surfaced clearly rather than
  failing silently.
- **Explicit reset:** the user can deliberately reset a device's identity
  (generate a fresh keypair), which invalidates all of that device's
  existing pairings — useful if a key is suspected compromised.

## Rollout phases

The connection host does not need to be a separate daemon to ship. The
intended progression:

1. **MVP — Flint is the connection host.** The running Flint GUI process
   hosts the Iroh endpoint and the PTYs, exactly as designed above. Closing
   all windows does not quit it; remote access works whenever Flint is
   running. No new long-running service to build, deploy, or supervise.
2. **Convenience.** Optional "Launch Flint at login" plus a menu-bar / status
   indicator showing whether remote access is active and how many devices are
   connected — so "is my machine reachable?" is answerable at a glance and a
   rebooted machine comes back without a manual relaunch.
3. **Later — extract a daemon, only if needed.** Decouple PTY/endpoint
   ownership into a background daemon *only* if users actually need sessions
   to survive Flint quitting, restarting, or updating. This is a real
   redesign of `agent_threads`' process model and earns its own spec; it is
   deliberately not a prerequisite for phases 1–2.

Framing note: describe Flint as the **connection host**, not a "service" or
"daemon." That sets the correct expectation — remote access works while
Flint is running — and avoids implying an always-on background service that
phases 1–2 intentionally don't provide.

## Open Questions / Future Work

- A headless daemon decoupling PTY/endpoint ownership from the GUI (Rollout
  phases, phase 3), so sessions survive Flint quitting / restarting /
  updating. Pursued only if users need it; a separate redesign of
  `agent_threads`' process model that earns its own spec. This connection
  layer is agnostic to where the host role lands.
- The iPhone app's UX: how multiple paired desktops and their sessions are
  presented, how scanning/pairing is initiated, notification behavior.
- The exact byte-level wire format for the control protocol (frame headers,
  snapshot encoding, control-lease handoff messages). The protocol's
  *shape* is decided above; its concrete serialization is an
  implementation detail to settle when building it.
- Operationalizing the production relay switch (managed Iroh Services vs.
  self-hosted `iroh-relay`, where it deploys, monitoring) — required before
  production per Decisions, but the specific hosting choice is open.
- The presence/status signal that lets the phone distinguish Disabled vs.
  Offline vs. Sleeping (see Process lifecycle) — cached last-known state,
  relay-side presence, or both — chosen without weakening the dumb-relay /
  E2E properties.
