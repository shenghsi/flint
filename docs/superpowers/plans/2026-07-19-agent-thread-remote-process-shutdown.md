# Agent Thread Remote Process Shutdown Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make closing a remote Agent Thread gracefully stop its CLI and then safely force-terminate that exact remote process group if it does not exit.

**Architecture:** A focused `remote_process` module wraps every POSIX remote Agent Thread launch with a private lifecycle record and owns the validated shared-SSH cleanup command. `AgentThreadStore` converts a live entry into a single owned shutdown operation; that operation sends `Ctrl-C`, races terminal completion against a two-second GPUI timer, force-cleans only on timeout, and retains its egress lease until the result is known.

**Tech Stack:** Rust, GPUI entities/tasks/timers, existing `remote::RemoteConnection` command construction, POSIX shell lifecycle wrapper, `/proc` validation on Linux, existing terminal PTY API.

## Global Constraints

- Apply the wrapper to remote Agent Threads under both `Through Flint` and `Not through Flint`.
- Leave ordinary terminal-tab behavior unchanged.
- Use `ConnectionSharing::Shared` for cleanup so load-balanced SSH aliases stay on the project backend.
- Never signal an unvalidated PID or use process-name-wide cleanup such as `pkill codex`.
- Send `Ctrl-C`, allow exactly two seconds for graceful completion, then allow 500 ms between `SIGTERM` and `SIGKILL`.
- Retain `AgentEgressLease` through graceful and forced cleanup; release it after success or failure.
- If the SSH host is unreachable or identity validation fails, report the error and revoke egress rather than claiming termination.
- Use GPUI executor timers in GPUI tests; do not use `smol::Timer` with `run_until_parked()`.
- Follow red-green-refactor for every behavior change.

## File Structure

- Create `crates/agent_threads/src/remote_process.rs`: lifecycle ID, launch wrapping, validated force-cleanup command construction/execution, and shutdown race policy.
- Modify `crates/agent_threads/src/agent_threads.rs`: declare the private module.
- Modify `crates/agent_threads/src/store.rs`: retain terminal/process ownership, prepare remote launches, and coordinate close sources.
- Modify `docs/superpowers/specs/2026-07-19-agent-thread-remote-process-shutdown-design.md`: mark the reviewed design implemented only after verification.
- Modify this plan file only to check completed tasks during execution.

---

### Task 1: Remote lifecycle identity and command generation

**Files:**
- Create: `crates/agent_threads/src/remote_process.rs`
- Modify: `crates/agent_threads/src/agent_threads.rs:1-14`
- Test: `crates/agent_threads/src/remote_process.rs`

**Interfaces:**
- Consumes: `AgentLaunchCommand`, `remote::RemoteConnection`, `remote::Interactive`, and `remote::ConnectionSharing`.
- Produces: `RemoteAgentProcess::prepare(command, connection, lifecycle_id) -> anyhow::Result<Self>` and `RemoteAgentProcess::force_terminate(&self) -> impl Future<Output = anyhow::Result<()>>`.

- [x] **Step 1: Write failing lifecycle-wrapper tests**

Add the module declaration and these tests before defining the production types:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use collections::HashMap;

    const LIFECYCLE_ID: &str = "7c3630a3-d084-4c8a-bb13-0adf73936942";

    #[test]
    fn remote_launch_uses_fixed_wrapper_and_preserves_agent_argv() {
        let mut command = AgentLaunchCommand {
            command: Some("/opt/codex path/codex".into()),
            args: vec!["resume".into(), "session with spaces".into()],
            env: HashMap::default(),
            ..AgentLaunchCommand::default()
        };

        wrap_remote_launch(&mut command, LIFECYCLE_ID).expect("valid command");

        assert_eq!(command.command.as_deref(), Some("/bin/sh"));
        assert_eq!(command.args[0], "-c");
        assert_eq!(command.args[2], LIFECYCLE_ID);
        assert_eq!(command.args[3], "/opt/codex path/codex");
        assert_eq!(command.args[4], "resume");
        assert_eq!(command.args[5], "session with spaces");
        assert_eq!(command.env.get("FLINT_AGENT_THREAD_ID").map(String::as_str), Some(LIFECYCLE_ID));
        assert!(!command.args[1].contains("/opt/codex path/codex"));
    }

    #[test]
    fn lifecycle_id_must_be_a_canonical_uuid() {
        let mut command = AgentLaunchCommand {
            command: Some("codex".into()),
            ..AgentLaunchCommand::default()
        };

        let error = wrap_remote_launch(&mut command, "../../other-process")
            .expect_err("unsafe lifecycle id must fail");

        assert_eq!(error.to_string(), "invalid Agent Thread lifecycle ID");
    }

    #[test]
    fn cleanup_command_is_non_interactive_and_shared() {
        let request = cleanup_request(LIFECYCLE_ID).expect("valid lifecycle id");

        assert_eq!(request.program, "/bin/sh");
        assert_eq!(request.args[0], "-c");
        assert_eq!(request.args[2], LIFECYCLE_ID);
        assert_eq!(request.interactive, remote::Interactive::No);
        assert_eq!(request.connection_sharing, remote::ConnectionSharing::Shared);
    }
}
```

- [x] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test -p agent_threads remote_process::tests --lib
```

Expected: compilation fails because `remote_process`, `wrap_remote_launch`, and `cleanup_request` do not exist.

- [x] **Step 3: Implement lifecycle IDs and fixed command descriptions**

Declare `mod remote_process;` in `agent_threads.rs`. In the new module, add these exact boundaries:

```rust
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use collections::HashMap;
use uuid::Uuid;

use crate::AgentLaunchCommand;

const LIFECYCLE_ENVIRONMENT_KEY: &str = "FLINT_AGENT_THREAD_ID";
const LAUNCH_SCRIPT: &str = r#"set -eu
lifecycle_id=$0
state_directory=$HOME/.local/state/flint/agent-threads
record=$state_directory/$lifecycle_id
temporary=$record.$$
umask 077
mkdir -p "$state_directory"
process_group=$(ps -o pgid= -p $$ | tr -d ' ')
if test -r /proc/$$/stat; then
    start_identity=linux:$(awk '{print $22}' /proc/$$/stat)
else
    start_identity=posix:$(ps -o lstart= -p $$)
fi
printf '%s\n%s\n%s\n%s\n' "$lifecycle_id" "$$" "$process_group" "$start_identity" >"$temporary"
mv -f "$temporary" "$record"
cleanup_record() {
    rm -f "$record"
}
trap cleanup_record EXIT
set +e
"$@"
exit_status=$?
set -e
exit "$exit_status"
"#;

const CLEANUP_SCRIPT: &str = r#"set -eu
lifecycle_id=$0
state_directory=$HOME/.local/state/flint/agent-threads
record=$state_directory/$lifecycle_id
test -f "$record" || exit 65
{
    IFS= read -r recorded_id
    IFS= read -r process_id
    IFS= read -r process_group
    IFS= read -r recorded_start
} <"$record"
test "$recorded_id" = "$lifecycle_id" || exit 66
case $process_id:$process_group in
    *[!0-9:]*|:*|*:) exit 66 ;;
esac
if ! kill -0 "$process_id" 2>/dev/null; then
    rm -f "$record"
    exit 0
fi
live_group=$(ps -o pgid= -p "$process_id" | tr -d ' ')
test "$live_group" = "$process_group" || exit 67
case $recorded_start in
    linux:*)
        test -r "/proc/$process_id/stat" || exit 67
        live_start=linux:$(awk '{print $22}' "/proc/$process_id/stat")
        test "$live_start" = "$recorded_start" || exit 67
        tr '\000' '\n' <"/proc/$process_id/environ" |
            grep -Fqx "FLINT_AGENT_THREAD_ID=$lifecycle_id" || exit 67
        ;;
    posix:*)
        live_start=posix:$(ps -o lstart= -p "$process_id")
        test "$live_start" = "$recorded_start" || exit 67
        ps eww -p "$process_id" -o command= |
            grep -Fq "FLINT_AGENT_THREAD_ID=$lifecycle_id" || exit 67
        ;;
    *) exit 66 ;;
esac
/bin/kill -TERM -- "-$process_group" 2>/dev/null ||
    /bin/kill -TERM "$process_id" 2>/dev/null || exit 68
attempt=0
while test "$attempt" -lt 5 && kill -0 "$process_id" 2>/dev/null; do
    sleep 0.1
    attempt=$((attempt + 1))
done
if kill -0 "$process_id" 2>/dev/null; then
    /bin/kill -KILL -- "-$process_group" 2>/dev/null ||
        /bin/kill -KILL "$process_id" 2>/dev/null || exit 68
fi
attempt=0
while test "$attempt" -lt 5 && kill -0 "$process_id" 2>/dev/null; do
    sleep 0.1
    attempt=$((attempt + 1))
done
kill -0 "$process_id" 2>/dev/null && exit 69
rm -f "$record"
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CleanupRequest {
    program: String,
    args: Vec<String>,
    interactive: remote::Interactive,
    connection_sharing: remote::ConnectionSharing,
}

fn canonical_lifecycle_id(value: &str) -> Result<String> {
    let id = Uuid::parse_str(value).map_err(|_| anyhow!("invalid Agent Thread lifecycle ID"))?;
    let canonical = id.hyphenated().to_string();
    if canonical != value {
        return Err(anyhow!("invalid Agent Thread lifecycle ID"));
    }
    Ok(canonical)
}

fn wrap_remote_launch(command: &mut AgentLaunchCommand, lifecycle_id: &str) -> Result<()> {
    let lifecycle_id = canonical_lifecycle_id(lifecycle_id)?;
    let program = command
        .command
        .take()
        .ok_or_else(|| anyhow!("remote Agent Thread command is missing"))?;
    let original_arguments = std::mem::take(&mut command.args);
    command.command = Some("/bin/sh".into());
    command.args = vec!["-c".into(), LAUNCH_SCRIPT.into(), lifecycle_id.clone(), program];
    command.args.extend(original_arguments);
    command
        .env
        .insert(LIFECYCLE_ENVIRONMENT_KEY.into(), lifecycle_id);
    Ok(())
}

fn cleanup_request(lifecycle_id: &str) -> Result<CleanupRequest> {
    let lifecycle_id = canonical_lifecycle_id(lifecycle_id)?;
    Ok(CleanupRequest {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), CLEANUP_SCRIPT.into(), lifecycle_id],
        interactive: remote::Interactive::No,
        connection_sharing: remote::ConnectionSharing::Shared,
    })
}
```

Keep both fixed scripts in `remote_process.rs`; they form one lifecycle
component and do not justify separate shell asset files.

- [x] **Step 4: Add controller execution using shared SSH**

Add:

```rust
#[derive(Clone)]
pub(crate) struct RemoteAgentProcess {
    lifecycle_id: String,
    connection: Arc<dyn remote::RemoteConnection>,
}

impl RemoteAgentProcess {
    pub(crate) fn prepare(
        command: &mut AgentLaunchCommand,
        connection: Arc<dyn remote::RemoteConnection>,
        lifecycle_id: Uuid,
    ) -> Result<Self> {
        let lifecycle_id = lifecycle_id.hyphenated().to_string();
        wrap_remote_launch(command, &lifecycle_id)?;
        Ok(Self { lifecycle_id, connection })
    }

    pub(crate) async fn force_terminate(&self) -> Result<()> {
        let request = cleanup_request(&self.lifecycle_id)?;
        let command = self.connection.build_command(
            Some(request.program),
            &request.args,
            &HashMap::default(),
            None,
            None,
            request.interactive,
            request.connection_sharing,
        )?;
        let output = util::command::new_command(&command.program)
            .args(&command.args)
            .envs(&command.env)
            .output()
            .await
            .context("failed to run remote Agent Thread cleanup")?;
        if !output.status.success() {
            let stderr_length = output.stderr.len().min(64 * 1024);
            let stderr = String::from_utf8_lossy(&output.stderr[..stderr_length]);
            return Err(anyhow!(
                "remote Agent Thread cleanup exited with {}: {}",
                output.status,
                stderr.trim()
            ));
        }
        Ok(())
    }
}
```

Never log command environments or lifecycle record contents.

- [x] **Step 5: Run tests and verify GREEN**

Run:

```bash
cargo test -p agent_threads remote_process::tests --lib
```

Expected: all lifecycle-wrapper and cleanup-request tests pass.

- [x] **Step 6: Commit Task 1**

```bash
git add crates/agent_threads/src/agent_threads.rs crates/agent_threads/src/remote_process.rs
git commit -m "agent_threads: Track remote agent processes"
```

---

### Task 2: Graceful-then-forced shutdown policy

**Files:**
- Modify: `crates/agent_threads/src/remote_process.rs`
- Test: `crates/agent_threads/src/remote_process.rs`

**Interfaces:**
- Consumes: `RemoteAgentProcess::force_terminate()` from Task 1.
- Produces: `wait_for_graceful_exit_or_force(completion, timeout, force) -> anyhow::Result<ShutdownOutcome>` and `ShutdownOutcome::{Graceful, Forced}`.

- [x] **Step 1: Write failing shutdown-race tests**

```rust
#[gpui::test]
async fn graceful_completion_skips_force_cleanup(cx: &mut gpui::TestAppContext) {
    let forced = std::rc::Rc::new(std::cell::Cell::new(false));
    let outcome = wait_for_graceful_exit_or_force(
        futures::future::ready(()),
        cx.background_executor.timer(std::time::Duration::from_secs(60)),
        {
            let forced = forced.clone();
            async move {
                forced.set(true);
                anyhow::Ok(())
            }
        },
    )
    .await
    .expect("graceful shutdown");

    assert_eq!(outcome, ShutdownOutcome::Graceful);
    assert!(!forced.get());
}

#[gpui::test]
async fn timeout_runs_force_cleanup_once(cx: &mut gpui::TestAppContext) {
    let force_count = std::rc::Rc::new(std::cell::Cell::new(0));
    let outcome = wait_for_graceful_exit_or_force(
        futures::future::pending(),
        cx.background_executor.timer(std::time::Duration::from_millis(1)),
        {
            let force_count = force_count.clone();
            async move {
                force_count.set(force_count.get() + 1);
                anyhow::Ok(())
            }
        },
    )
    .await
    .expect("forced shutdown");

    assert_eq!(outcome, ShutdownOutcome::Forced);
    assert_eq!(force_count.get(), 1);
}
```

- [x] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test -p agent_threads remote_process::tests --lib
```

Expected: compilation fails because the shutdown policy and outcome do not exist.

- [x] **Step 3: Implement the minimal race policy**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShutdownOutcome {
    Graceful,
    Forced,
}

pub(crate) async fn wait_for_graceful_exit_or_force<Completion, Timeout, Force>(
    completion: Completion,
    timeout: Timeout,
    force: Force,
) -> Result<ShutdownOutcome>
where
    Completion: std::future::Future<Output = ()>,
    Timeout: std::future::Future<Output = ()>,
    Force: std::future::Future<Output = Result<()>>,
{
    use futures::FutureExt as _;
    let completion = completion.fuse();
    let timeout = timeout.fuse();
    futures::pin_mut!(completion, timeout);
    futures::select_biased! {
        () = completion => Ok(ShutdownOutcome::Graceful),
        () = timeout => {
            force.await?;
            Ok(ShutdownOutcome::Forced)
        }
    }
}
```

- [x] **Step 4: Run tests and verify GREEN**

Run:

```bash
cargo test -p agent_threads remote_process::tests --lib
```

Expected: every `remote_process` test passes.

- [x] **Step 5: Commit Task 2**

```bash
git add crates/agent_threads/src/remote_process.rs
git commit -m "agent_threads: Define bounded agent shutdown"
```

---

### Task 3: Register process ownership with every remote Agent Thread

**Files:**
- Modify: `crates/agent_threads/src/store.rs:1144-1360`
- Test: `crates/agent_threads/src/store.rs:1556-end`

**Interfaces:**
- Consumes: `RemoteAgentProcess::prepare()` from Task 1.
- Produces: `ThreadEntry.remote_process: Option<RemoteAgentProcess>` and a wrapped `SpawnInTerminal` command for every non-Windows remote Agent Thread.

- [x] **Step 1: Write a failing launch-preparation test**

Extract a small helper whose desired API is:

```rust
fn prepare_remote_thread_process(
    command: &mut AgentLaunchCommand,
    remote_connection: Option<Arc<dyn remote::RemoteConnection>>,
    is_windows: bool,
    lifecycle_id: uuid::Uuid,
) -> Result<Option<RemoteAgentProcess>>;
```

Test the no-remote decision directly: pass `None`, assert the result is `None`,
and assert the command remains byte-for-byte equal to its clone. The Task 1
wrapper test already covers the POSIX remote transformation with deterministic
arguments.

```rust
#[test]
fn local_agent_launch_is_not_wrapped() {
    let mut command = AgentLaunchCommand {
        command: Some("codex".into()),
        args: vec!["resume".into(), "session-a".into()],
        ..AgentLaunchCommand::default()
    };
    let original = command.clone();

    let process = prepare_remote_thread_process(
        &mut command,
        None,
        false,
        uuid::Uuid::nil(),
    )
    .expect("local launch preparation");

    assert!(process.is_none());
    assert_eq!(command, original);
}
```

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p agent_threads prepare_remote_thread_process --lib
```

Expected: compilation fails because the helper and `ThreadEntry.remote_process` do not exist.

- [x] **Step 3: Prepare the process after preserving the visible command label**

In `spawn_thread_task_for_route`, retain a clone of the remote connection for process control. Pass it through both routing branches into `spawn_thread_task_inner`. In `spawn_thread_task_inner`:

```rust
let command_label = command_label(&command, &label);
let is_windows = workspace.project().read(cx).path_style(cx).is_windows();
let remote_process = prepare_remote_thread_process(
    &mut command,
    remote_connection,
    is_windows,
    uuid::Uuid::new_v4(),
)?;
```

Compute `command_label` before wrapping so the tab continues to describe Codex or Claude rather than `/bin/sh`.

Extend `ThreadEntry`, `AgentThreadStore::register`, and the `register` call with:

```rust
terminal: Entity<terminal::Terminal>,
remote_process: Option<RemoteAgentProcess>,
egress: Option<AgentEgressLease>,
```

Store the strong terminal entity intentionally: it keeps process control alive after the `TerminalView` release callback begins shutdown. Rename `_egress` to `egress`, because shutdown now explicitly consumes it.

For Windows remotes, leave the command unwrapped and return `None`; direct terminal interruption still occurs, but targeted POSIX force cleanup is unavailable.

- [x] **Step 4: Run launch and existing routing tests**

Run:

```bash
cargo test -p agent_threads store::tests --lib
```

Expected: all store tests pass, including managed resume, Through-Flint environment, and the new launch-preparation cases.

- [x] **Step 5: Commit Task 3**

```bash
git add crates/agent_threads/src/store.rs
git commit -m "agent_threads: Register remote process ownership"
```

---

### Task 4: Replace bookkeeping-only close with owned shutdown

**Files:**
- Modify: `crates/agent_threads/src/store.rs:245-285,362-435`
- Test: `crates/agent_threads/src/store.rs:1556-end`

**Interfaces:**
- Consumes: `ThreadEntry.terminal`, `ThreadEntry.remote_process`, `ThreadEntry.egress`, and `wait_for_graceful_exit_or_force()`.
- Produces: `AgentThreadStore::begin_shutdown(terminal_item_id, cx) -> Option<Task<Result<()>>>` used by both release observation and `close_threads_for_connection`.

- [x] **Step 1: Write failing single-owner and lease-order tests**

Introduce a focused shutdown resource type and test its ownership independently of workspace UI setup:

```rust
struct ThreadShutdown {
    terminal: Entity<terminal::Terminal>,
    remote_process: Option<RemoteAgentProcess>,
    egress: Option<AgentEgressLease>,
    workspace: WeakEntity<Workspace>,
}
```

Add a generic private helper used by `ThreadShutdown::run` so a drop probe can verify ordering without a test-only `AgentEgressLease` constructor:

```rust
async fn retain_resource_until_shutdown<Resource, Shutdown>(
    resource: Resource,
    shutdown: Shutdown,
) -> Result<()>
where
    Shutdown: std::future::Future<Output = Result<()>>,
{
    let result = shutdown.await;
    drop(resource);
    result
}
```

Use these tests for resource ordering and one-shot entry ownership:

```rust
struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[gpui::test]
async fn shutdown_resource_is_retained_until_cleanup_finishes(
    cx: &mut gpui::TestAppContext,
) {
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (release_tx, release_rx) = async_channel::bounded(1);
    let task = cx.background_executor.spawn(retain_resource_until_shutdown(
        DropProbe(dropped.clone()),
        async move {
            release_rx.recv().await?;
            anyhow::Ok(())
        },
    ));

    cx.run_until_parked();
    assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
    release_tx.send(()).await.expect("release shutdown");
    task.await.expect("shutdown result");
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn repeated_shutdown_cannot_take_the_same_entry() {
    let id = EntityId::from(7);
    let mut entries = HashMap::from_iter([(id, "thread")]);

    assert_eq!(take_thread_for_shutdown(&mut entries, id), Some("thread"));
    assert_eq!(take_thread_for_shutdown(&mut entries, id), None);
}
```

- [x] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p agent_threads retain_resource_until_shutdown repeated_shutdown --lib
```

Expected: compilation fails because `ThreadShutdown`, the resource helper, and `begin_shutdown` do not exist.

- [x] **Step 3: Implement `ThreadShutdown::run`**

Use the existing terminal completion and PTY APIs:

```rust
const AGENT_SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(2);

impl ThreadShutdown {
    fn run(self, cx: &mut App) -> Task<Result<()>> {
        let completion = self.terminal.update(cx, |terminal, cx| {
            let completion = terminal.wait_for_completed_task(cx);
            terminal.input(vec![0x03]);
            completion
        });
        let timeout = cx
            .background_executor()
            .timer(AGENT_SHUTDOWN_GRACE_PERIOD);
        let terminal = self.terminal;
        let remote_process = self.remote_process;
        let egress = self.egress;
        cx.spawn(async move |cx| {
            let shutdown = async move {
                let force = async move {
                    if let Some(remote_process) = remote_process {
                        remote_process.force_terminate().await
                    } else {
                        terminal.update(cx, |terminal, _| terminal.kill_active_task())?;
                        Ok(())
                    }
                };
                wait_for_graceful_exit_or_force(
                    async move { drop(completion.await) },
                    timeout,
                    force,
                )
                .await?;
                Ok(())
            };
            retain_resource_until_shutdown(egress, shutdown).await
        })
    }
}
```

Move `terminal`, `remote_process`, and `egress` into the spawned future exactly
as shown so no entity borrow crosses an `await`. Propagate both entity-update
and cleanup errors with `?`.

- [x] **Step 4: Implement one-shot store shutdown and detached error reporting**

Add the one-shot helper used by `begin_shutdown`:

```rust
fn take_thread_for_shutdown<Entry>(
    entries: &mut HashMap<EntityId, Entry>,
    terminal_item_id: EntityId,
) -> Option<Entry> {
    entries.remove(&terminal_item_id)
}
```

Replace `remove_thread` with `begin_shutdown`. It calls
`take_thread_for_shutdown`, removes the subscriptions, emits `ThreadClosed`
once, constructs `ThreadShutdown`, and returns its task.

The `TerminalView` release observer calls `begin_shutdown`, then awaits the returned task in a detached foreground task. On error, upgrade the stored workspace and call `workspace.show_error(&error, cx)`; if the workspace is gone, log the error with context.

Do not drop the returned task: GPUI cancels work when a `Task` is dropped.

- [x] **Step 5: Make route-change close await the same shutdown**

For each entry in `close_threads_for_connection`:

1. Call `begin_shutdown` and retain the returned task.
2. Close the pane item.
3. Await the shutdown task before moving to the next entry.
4. Propagate cleanup failure to the existing route-change UI.

This ordering removes the UI immediately, prevents the release observer from starting duplicate cleanup, and keeps the route-change operation pending until the old process and egress lease are settled.

- [x] **Step 6: Run focused and full Agent Threads tests**

Run:

```bash
cargo test -p agent_threads store::tests --lib
cargo test -p agent_threads --lib
```

Expected: all Agent Threads tests pass; the new tests observe one shutdown and correct resource ordering.

- [x] **Step 7: Commit Task 4**

```bash
git add crates/agent_threads/src/store.rs
git commit -m "agent_threads: Stop agents when threads close"
```

---

### Task 5: Regression verification and delivery build

**Files:**
- Modify: `docs/superpowers/specs/2026-07-19-agent-thread-remote-process-shutdown-design.md`
- Modify: `docs/superpowers/plans/2026-07-19-agent-thread-remote-process-shutdown.md`

**Interfaces:**
- Consumes: completed lifecycle, launch, and shutdown behavior from Tasks 1-4.
- Produces: verified formatting, lint, tests, app bundle, and implementation-status documentation.

- [x] **Step 1: Run formatting and focused lint**

```bash
cargo fmt --all -- --check
./script/clippy -p agent_threads -p remote -p terminal
```

Expected: both commands exit zero with no new warnings.

- [x] **Step 2: Run affected crate suites**

```bash
cargo test -p agent_threads --lib
cargo test -p terminal --lib
cargo test -p project --lib terminals
cargo test -p remote --lib
```

Expected: all tests pass.

- [x] **Step 3: Build the local app bundle**

```bash
./script/bundle-tmp-app
```

Expected: `/tmp/Flint-Local.app` contains the fresh build. If the documented debug `sign_binary` step fails after the bundle was built, copy `target/<target-triple>/debug/bundle/osx/Flint.app` to a new temporary path, preserve the old `/tmp/Flint-Local.app` by renaming it, then copy the fresh bundle into place.

- [x] **Step 4: Perform the remote acceptance check**

Open a POSIX SSH project, launch two Agent Threads, record their remote lifecycle PIDs without printing environment values, close one thread, and confirm within three seconds that only its PID and process group are gone. Confirm the other thread still answers and retains its proxy route. Repeat once with `Not through Flint`.

- [x] **Step 5: Mark documentation implemented and commit**

Change the design status to `Implemented and verified`, check every completed plan checkbox, then run `git diff --check`.

```bash
git add docs/superpowers/specs/2026-07-19-agent-thread-remote-process-shutdown-design.md docs/superpowers/plans/2026-07-19-agent-thread-remote-process-shutdown.md
git commit -m "Document Agent Thread shutdown delivery"
```
