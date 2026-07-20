# Managed Agent Resume Routing Implementation Plan

**Date:** 2026-07-19
**Design:** `docs/superpowers/specs/2026-07-19-managed-agent-resume-routing-design.md`

## Goal

Resume historical and automatically restored Codex sessions according to the
active SSH host route:

- **Through Flint** resolves the shared pinned Flint-managed Codex, requires a
  Flint egress lease, and never falls back to ambient Codex or direct routing.
- **Not through Flint** uses the configured or ambient Codex command without
  managed resolution, Flint egress, or Flint proxy variables.

Existing valid remote installations must be visibly reused without a local
download prompt, upload, or reinstall.

## Constraints

- Keep executable selection and egress enforcement explicit even though the
  approved Through-Flint resume policy requires both managed Codex and egress.
- Preserve the existing new-thread choices and managed-new action.
- Keep one managed executable per remote identity, agent, pinned version, and
  platform; do not create per-thread copies.
- Keep download confirmation for uncached artifacts, including startup
  restoration.
- Do not add direct-network fallback, connectivity probing, OS firewalling, or
  automatic SSH tunnel restoration.
- Use red-green-refactor for every behavior change.

## 1. Expose Remote Installation Reuse Before Artifact Acquisition

**Files:**

- Modify `crates/agent_threads/src/managed_agent.rs`
- Modify `crates/agent_threads/src/managed_agent_progress.rs`

### Red

Add focused fake-host tests proving that:

1. a matching receipt, executable digest, and version returns the existing
   `ManagedAgentInstallation` before `AgentArtifactSource::acquire` is called;
2. reuse reports checking and reuse phases, with no upload, staging, chmod,
   receipt write, or commit event;
3. a missing, malformed, or mismatched receipt reports checking and proceeds to
   artifact acquisition and transactional installation; and
4. the progress notification has distinct user-visible states for checking an
   installed agent, reusing it, verifying an uploaded agent, and resuming.

Run and observe the expected failures:

```sh
cargo test -p agent_threads managed_agent::tests::reuses_valid_remote_installation_before_acquiring_artifact
cargo test -p agent_threads managed_agent_progress::tests
```

### Green

Split managed provisioning into a reusable remote-validation boundary and the
existing transactional installation boundary. The validation method receives
the agent ID and pinned release, computes the expected receipt and path, and
returns `Option<ManagedAgentInstallation>` without touching the artifact
source.

Make the existing install entry point validate the remote installation first,
then acquire and upload only when validation returns `None`. Add progress
phases for checking and reuse and map them to unambiguous notification states.
Do not log receipt contents, proxy capabilities, or credentials.

### Refactor

Keep receipt construction and expected-path construction in one implementation
so validation and installation cannot disagree. Retain the existing rollback
and cleanup behavior unchanged.

## 2. Extract a Shared Managed-Resolution Workflow

**Files:**

- Modify `crates/agent_threads/src/store.rs`
- Modify `crates/agent_threads/src/managed_agent_progress.rs`

### Red

Add tests around the resolution coordinator proving:

1. a valid remote installation reaches **Reusing installed Codex** without
   checking the local cache or prompting;
2. an invalid or absent remote installation checks the cache and prompts only
   when the pinned artifact is absent;
3. declining the prompt returns a cancellation outcome and starts no upload or
   launch;
4. repeated callers for the same provisioning key re-show one active
   notification and do not create a second owner; and
5. a completed reuse or installation returns the same shared absolute path to
   the launch caller.

Run and observe the expected failures:

```sh
cargo test -p agent_threads managed_resolution
```

### Green

Extract the download-confirmation, progress-channel, cache, remote-host, and
provisioner orchestration currently embedded in `launch_managed_thread` into a
single helper that resolves `Option<ManagedAgentInstallation>` for a workspace,
kind, and remote client.

The helper must:

- reserve the existing single-flight provisioning key;
- show or re-show the persistent notification;
- validate the remote installation before consulting the local cache;
- prompt if and only if both remote reuse and local cache reuse are unavailable;
- return cancellation separately from failure;
- always release the provisioning reservation; and
- leave process creation to its caller.

Update `launch_managed_thread` to consume this helper and preserve its current
new-thread behavior.

### Refactor

Use a small result enum for `Ready(ManagedAgentInstallation)`, `Cancelled`, and
`AlreadyInProgress` rather than overloading errors or nested options. Keep UI
ownership in `store.rs` and byte/receipt operations in `managed_agent.rs`.

## 3. Make Resume Policy Route-Driven

**Files:**

- Modify `crates/agent_threads/src/store.rs`
- Modify tests in `crates/agent_threads/src/panel.rs` where the terminal-spawn
  seam is already exercised

### Red

Add tests proving:

1. **Not through Flint** resume retains the configured command, resume session
   ID, resume options, and environment, and requests no managed resolution;
2. **Through Flint** resume replaces the command with the managed absolute
   path, retains the resume session ID and options, applies self-update policy,
   and reaches the route-aware launcher with a required Through-Flint route;
3. managed resolution cancellation or failure creates no terminal and does not
   call the configured command;
4. egress acquisition failure creates no terminal and does not retry without
   proxy variables; and
5. the managed installation object is shared while each spawned thread has its
   own terminal and egress lease.

Use the narrowest injectable seam needed to provide a fake managed-resolution
result and fake egress outcome. Do not write a unit test that only checks an
enum without exercising resume command construction and process gating.

Run and observe the expected failures:

```sh
cargo test -p agent_threads resume
```

### Green

Give route-sensitive resume an explicit required-route value. Manual resume
reads the current SSH route:

- For **Not through Flint**, build the history provider's configured/ambient
  resume command and launch with required route `NotThroughFlint`.
- For **Through Flint**, resolve the shared managed installation, build the
  provider's resume command with its command replaced by the verified absolute
  path, and launch with required route `ThroughFlint`.

Extend the route-aware launcher so a required route is checked at preparation
and immediately before terminal creation. Through-Flint launch must acquire
egress before process creation, apply proxy variables, and retain the lease in
the live thread entry. A mismatch or lease failure returns an error without a
terminal or alternate launch.

Keep ordinary new-thread launch behavior unchanged by using no required route
for that path. It still reads and applies the current route as it does today.

### Refactor

Name the domain values after policy, such as `RequiredAgentRoute`, instead of
passing an ambiguous boolean. Keep command construction, managed resolution,
and terminal creation as separate functions.

## 4. Restore Sessions Sequentially Through the Same Resume Path

**Files:**

- Modify `crates/agent_threads/src/store.rs`
- Modify restoration tests in `crates/agent_threads/src/panel.rs` or
  `crates/agent_threads/src/store.rs`

### Red

Add tests proving that:

1. automatic restoration invokes the same route-driven resume operation as a
   manual resume;
2. two Through-Flint Codex records finish managed resolution sequentially, so
   the second reuses the first installation;
3. an uncached first restoration displays one download prompt;
4. declining that prompt counts the first record as not restored without
   changing the route, and later records remain deterministic; and
5. restoration reports the correct failure count while preserving successful
   sessions.

Run and observe the expected failures:

```sh
cargo test -p agent_threads restore
```

### Green

Stop constructing every resume task before the restoration loop. Carry the
prepared restoration records into one foreground async task, and for each
record:

1. update the workspace to create its route-driven resume task;
2. await that task to completion; and
3. then proceed to the next record.

Continue skipping unknown or hidden agent kinds and preserve the existing
first-attempt guard and failure logging.

### Refactor

Keep restoration-record parsing and filtering synchronous. Only process
preparation and completion are sequential; history scanning and unrelated
workspace behavior remain unchanged.

## 5. Verify Progress Semantics and No-Fallback Behavior

**Files:**

- Modify `crates/agent_threads/src/managed_agent_progress.rs`
- Modify `crates/agent_threads/src/store.rs`

Add or update tests for the exact menu/notification labels:

- **Checking installed Codex**
- **Reusing installed Codex**
- **Downloading Codex CLI** with byte progress when applicable
- **Uploading Codex CLI to remote**
- **Verifying uploaded Codex CLI**
- **Installing Codex CLI on remote**
- **Resuming Codex session**

Assert that reuse never enters downloading or uploading states and that every
failure path leaves the prepared terminal count unchanged.

Run:

```sh
cargo test -p agent_threads managed_agent_progress
cargo test -p agent_threads resume
```

## 6. Full Validation

Run focused and regression validation:

```sh
cargo fmt --all -- --check
cargo test -p agent_threads --lib
cargo test -p remote_server --lib
./script/clippy -p agent_threads
./script/clippy -p remote_server
RUSTFLAGS='-C target-feature=+crt-static' cargo zigbuild --package remote_server --features debug-embed --target-dir target/remote_server --target x86_64-unknown-linux-musl
```

Confirm there is no debug instrumentation and inspect the final diff for
unrelated settings, route, shell, or provisioning changes.

## 7. Package and Live-Validate

Build with:

```sh
./script/bundle-tmp-app
```

If the documented debug-tail bug prevents the final copy, verify and stage the
fresh signed bundle from
`target/aarch64-apple-darwin/debug/bundle/osx/Flint.app`. Do not overwrite a
running app bundle. Preserve the previous `/tmp/Flint-Local.app` under a unique
backup path before installing the new bundle.

On the existing offline SSH project:

1. Select **Through Flint** and resume a known Codex session.
2. Verify the UI says the existing managed installation is checked and reused.
3. Verify local cache and remote installed-file timestamps do not change.
4. Verify the remote process command is the shared managed absolute path with
   the expected `resume <session-id>` arguments.
5. Verify the process receives the Through-Flint proxy environment and the SSH
   reverse forward remains present for its terminal lifetime.
6. Verify a failed egress setup creates no Codex process and no direct retry.
7. Select **Not through Flint**, resume, and verify the configured/ambient
   command is used without a Flint proxy or reverse-forward lease.

Record any live-only limitation separately. Do not claim OS-enforced network
isolation or automatic tunnel restoration.
