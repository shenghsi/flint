# Shared SSH Reverse Forward Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Through-Flint agent egress use the remote project's existing OpenSSH connection on macOS and Linux so load-balanced SSH aliases cannot place the reverse proxy on the wrong login node.

**Architecture:** `RemotePortForward` will own a one-shot asynchronous closer rather than assuming every forward is represented by a long-lived child process. POSIX SSH creates the forward with `ssh -O forward` through Flint's shared `ControlPath`, retains an exact `ssh -O cancel` operation in the handle, and schedules that cancellation on drop; Windows keeps the existing dedicated child process.

**Tech Stack:** Rust, OpenSSH multiplex control commands, `gpui::BackgroundExecutor`, `futures`, existing `remote` crate tests.

## Global Constraints

- Apply shared-ControlMaster reverse forwarding only when Flint runs on macOS or Linux.
- Keep Windows on the existing dedicated `ssh -N -R` implementation.
- Bind the remote listener only to `127.0.0.1` and the local target only to `127.0.0.1`.
- Cancel a dynamic forward with the original port-zero `-R` request, not the allocated remote port.
- Never expose command environments or proxy capability URLs in errors or logs.
- Do not change local OAuth callback-forward behavior.
- Follow red-green-refactor and commit after each independently testable task.

---

### Task 1: Give `RemotePortForward` an owned asynchronous close operation

**Files:**
- Modify: `crates/remote/src/remote_client.rs:124-146`
- Test: `crates/remote/src/remote_client.rs` test module

**Interfaces:**
- Consumes: `gpui::BackgroundExecutor`, `futures::future::BoxFuture<'static, anyhow::Result<()>>`.
- Produces: `RemotePortForward::with_closer(remote_port, executor, closer)` and exactly-once explicit/drop cleanup.

- [x] **Step 1: Write failing lifecycle tests**

Add `AtomicUsize` to the test-only atomic imports. Add GPUI tests that use an
`Arc<AtomicUsize>` closer rather than a subprocess:

```rust
#[gpui::test]
async fn remote_port_forward_close_runs_closer_once(cx: &mut TestAppContext) {
    let close_count = Arc::new(AtomicUsize::new(0));
    let forward = RemotePortForward::with_closer(
        43123,
        cx.background_executor.clone(),
        {
            let close_count = close_count.clone();
            move || {
                async move {
                    close_count.fetch_add(1, SeqCst);
                    Ok(())
                }
                .boxed()
            }
        },
    );

    forward.close().await.expect("close should succeed");
    cx.run_until_parked();

    assert_eq!(close_count.load(SeqCst), 1);
}

#[gpui::test]
async fn dropping_remote_port_forward_schedules_closer_once(cx: &mut TestAppContext) {
    let close_count = Arc::new(AtomicUsize::new(0));
    let forward = RemotePortForward::with_closer(
        43123,
        cx.background_executor.clone(),
        {
            let close_count = close_count.clone();
            move || {
                async move {
                    close_count.fetch_add(1, SeqCst);
                    Ok(())
                }
                .boxed()
            }
        },
    );

    drop(forward);
    cx.run_until_parked();

    assert_eq!(close_count.load(SeqCst), 1);
}
```

Add a third test whose closer returns `Err(anyhow!("cancel failed"))` and assert that `close().await` returns that error.

- [x] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test -p remote remote_port_forward_
```

Expected: compilation fails because `RemotePortForward::with_closer` does not exist.

- [x] **Step 3: Implement the minimal owned closer**

Replace the process-only field with:

```rust
type RemotePortForwardCloser =
    Box<dyn FnOnce() -> BoxFuture<'static, Result<()>> + Send + 'static>;

pub struct RemotePortForward {
    remote_port: u16,
    closer: Option<RemotePortForwardCloser>,
    executor: BackgroundExecutor,
}
```

Provide `with_closer`. Keep `new(remote_port, process, executor)` as a constructor that wraps the existing process kill/status sequence in a closer so the Windows implementation keeps the same semantics.

`close(mut self)` takes the closer and awaits it. `Drop` takes the closer once, spawns it on the stored executor, logs a redacted error with `log::error!`, and detaches the task. Taking the `Option` prevents an explicit close from triggering duplicate drop cleanup.

Use these signatures and lifecycle implementation:

```rust
pub(crate) fn with_closer(
    remote_port: u16,
    executor: BackgroundExecutor,
    closer: impl FnOnce() -> BoxFuture<'static, Result<()>> + Send + 'static,
) -> Self;

pub fn new(
    remote_port: u16,
    process: util::command::Child,
    executor: BackgroundExecutor,
) -> Self;

pub async fn close(mut self) -> Result<()> {
    let Some(closer) = self.closer.take() else {
        return Ok(());
    };
    closer().await
}

impl Drop for RemotePortForward {
    fn drop(&mut self) {
        let Some(closer) = self.closer.take() else {
            return;
        };
        self.executor
            .spawn(async move {
                if let Err(error) = closer().await {
                    log::error!("failed to close remote port forward: {error}");
                }
            })
            .detach();
    }
}
```

- [x] **Step 4: Run focused and crate tests and verify GREEN**

Run:

```bash
cargo test -p remote remote_port_forward_
cargo test -p remote
```

Expected: all tests pass.

- [x] **Step 5: Commit**

```bash
git add crates/remote/src/remote_client.rs
git commit -m "remote: Own asynchronous port-forward cleanup"
```

---

### Task 2: Specify POSIX shared-master forward and cancellation commands

**Files:**
- Modify: `crates/remote/src/transport/ssh.rs:50-105`
- Test: `crates/remote/src/transport/ssh.rs` test module

**Interfaces:**
- Consumes: `SshSocket::ssh_command_options(ConnectionSharing::Shared)`.
- Produces: `reverse_port_forward_arguments`, `cancel_reverse_port_forward_arguments`, and `parse_allocated_remote_forward_port` behavior used by Task 3.

- [x] **Step 1: Replace the old dedicated-command expectation with failing POSIX tests**

Rename `reverse_forward_is_dedicated_and_loopback_only` to `reverse_forward_uses_shared_master_and_is_loopback_only`. Assert that creation contains:

```rust
assert!(arguments.windows(2).any(|args| args == ["-O", "forward"]));
assert!(arguments.windows(2).any(|args| {
    args == ["-o", "ControlPath=/tmp/flint-ssh-socket"]
}));
assert!(!arguments.iter().any(|argument| argument == "ControlPath=none"));
assert!(arguments.windows(2).any(|args| {
    args == ["-R", "127.0.0.1:0:127.0.0.1:43123"]
}));
```

Add `reverse_forward_cancellation_uses_shared_master_and_original_dynamic_request`, asserting `-O cancel`, the shared `ControlPath`, and the same port-zero `-R` value.

Extend the parser test:

```rust
assert_eq!(parse_allocated_remote_forward_port("43123\n"), Some(43123));
assert_eq!(parse_allocated_remote_forward_port("not-a-port\n"), None);
```

Keep the local-forward test unchanged and still expecting `ControlPath=none`.

- [x] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test -p remote transport::ssh::tests::reverse_forward_
cargo test -p remote transport::ssh::tests::parses_only_the_allocated_reverse_forward_port_message
```

Expected: creation still contains `ControlPath=none`, cancellation helper is missing, and bare allocated-port output is rejected.

- [x] **Step 3: Implement platform-specific argument builders and parsing**

On non-Windows platforms, build creation with `ConnectionSharing::Shared`, `-O forward`, `ExitOnForwardFailure=yes`, and the loopback dynamic `-R` request. Build cancellation with the same shared options, `-O cancel`, and the identical `-R` request.

Retain the existing dedicated `-N -T -v` creation arguments under `#[cfg(windows)]`.

Update port parsing to accept either trimmed bare `u16` output from `ssh -O forward` or the existing verbose `Allocated port ...` diagnostic used by the Windows path. Reject zero and out-of-range values.

- [x] **Step 4: Run focused and crate tests and verify GREEN**

Run:

```bash
cargo test -p remote transport::ssh::tests::reverse_forward_
cargo test -p remote transport::ssh::tests::parses_only_the_allocated_reverse_forward_port_message
cargo test -p remote
```

Expected: all tests pass; the existing local-forward test still proves callback routing is dedicated.

- [x] **Step 5: Commit**

```bash
git add crates/remote/src/transport/ssh.rs
git commit -m "remote: Define shared SSH reverse forwarding"
```

---

### Task 3: Execute POSIX forward creation and owned cancellation

**Files:**
- Modify: `crates/remote/src/transport/ssh.rs:567-618`
- Test: `crates/remote/src/transport/ssh.rs` test module

**Interfaces:**
- Consumes: Task 1's `RemotePortForward::with_closer` and Task 2's creation/cancellation argument builders.
- Produces: `SshRemoteConnection::open_reverse_port_forward` using the existing project ControlMaster on macOS/Linux.

- [x] **Step 1: Add failing command-result tests at an extracted helper seam**

Extract a pure result parser and add these POSIX-only tests. Import
`std::os::unix::process::ExitStatusExt` in the test module and construct output
with the following helper:

```rust
#[cfg(not(windows))]
fn ssh_output(exit_code: i32, stdout: &[u8], stderr: &[u8]) -> std::process::Output {
    use std::os::unix::process::ExitStatusExt as _;

    std::process::Output {
        status: std::process::ExitStatus::from_raw(exit_code << 8),
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
    }
}

#[cfg(not(windows))]
#[test]
fn shared_reverse_forward_result_requires_success_and_a_valid_port() {
    let success = ssh_output(0, b"43123\n", b"");
    assert_eq!(shared_reverse_forward_port(&success).unwrap(), 43123);
}

#[cfg(not(windows))]
#[test]
fn shared_reverse_forward_result_surfaces_redacted_ssh_failure() {
    let failure = ssh_output(1, b"", b"remote port forwarding failed");
    let error = shared_reverse_forward_port(&failure).unwrap_err();
    assert!(error.to_string().contains("remote port forwarding failed"));
}
```

Use the crate's platform-appropriate `ExitStatusExt` helpers inside the test module. Add a malformed-success case and assert it reports that OpenSSH returned no valid remote port.

- [x] **Step 2: Run the helper tests and verify RED**

Run:

```bash
cargo test -p remote shared_reverse_forward_result_
```

Expected: compilation fails because `shared_reverse_forward_port` does not exist.

- [x] **Step 3: Implement one-shot POSIX creation and cancellation**

For non-Windows clients:

1. Run the creation arguments with stdout and stderr captured, `kill_on_drop(true)`, and the existing 15-second executor timeout.
2. Require a successful exit and parse the allocated port from stdout with `shared_reverse_forward_port`.
3. Capture cloned SSH environment, cancellation arguments, and executor in a `RemotePortForward::with_closer` closure.
4. In the closer, run `ssh -O cancel` with captured stdout/stderr, require a successful status, and return an error containing only bounded SSH stderr on failure.

Bound diagnostics with an explicit constant and helper:

```rust
const SSH_FORWARD_DIAGNOSTIC_LIMIT: usize = 8 * 1024;

fn ssh_forward_diagnostic(stderr: &[u8]) -> String {
    let length = stderr.len().min(SSH_FORWARD_DIAGNOSTIC_LIMIT);
    let bounded = stderr.get(..length).unwrap_or(stderr);
    String::from_utf8_lossy(bounded).trim().to_string()
}
```

For Windows, preserve the existing verbose, long-lived dedicated child and construct it with `RemotePortForward::new(remote_port, process, executor.clone())`.

Do not log the `Command` value or environment in either path.

- [x] **Step 4: Run focused and regression tests and verify GREEN**

Run:

```bash
cargo test -p remote shared_reverse_forward_result_
cargo test -p remote transport::ssh::tests
cargo test -p remote
cargo test -p agent_threads egress
cargo test -p agent_threads
```

Expected: all tests pass.

- [x] **Step 5: Commit**

```bash
git add crates/remote/src/transport/ssh.rs crates/remote/src/remote_client.rs
git commit -m "remote: Route reverse forwards through shared SSH"
```

---

### Task 4: Validate quality and build the test app

**Files:**
- Modify: `docs/superpowers/plans/2026-07-19-shared-ssh-reverse-forward.md` checkbox state only

**Interfaces:**
- Consumes: completed implementation from Tasks 1-3.
- Produces: formatted, linted, tested code and a fresh `/tmp/Flint-Local.app` for live validation.

- [x] **Step 1: Run formatting and lint checks**

Run:

```bash
cargo fmt --all -- --check
./script/clippy -p remote -p agent_threads
```

Expected: both commands succeed with no warnings.

- [x] **Step 2: Run final regression tests**

Run:

```bash
cargo test -p remote
cargo test -p agent_threads
```

Expected: all tests pass.

- [x] **Step 3: Build and install the local test app**

Run:

```bash
./script/bundle-tmp-app
```

If the known debug-script bug fails only at `target/aarch64-apple-darwin/release/remote_server`, verify that the fresh debug bundle exists and copy it manually:

```bash
cp -R target/aarch64-apple-darwin/debug/bundle/osx/Flint.app /tmp/Flint-Local.app
```

Compare SHA-256 hashes of the target and `/tmp` executables before claiming the app is current.

- [x] **Step 4: Perform live acceptance on the load-balanced host**

Open a project Through Flint, launch Flint-managed Codex, and verify without printing proxy credentials:

```bash
pgrep -af codex
```

Use the existing shared SSH ControlPath to confirm the advertised proxy port is listening on the same `hostname` as the remote project. Send a Codex prompt and confirm there is no WebSocket/HTTPS `Connection refused`. Close the Agent Thread and confirm both the Codex PID and reverse listener disappear after bounded cleanup.

- [x] **Step 5: Mark the plan complete and commit documentation state**

Update the checkboxes in this file only after each command or live check succeeds, then run:

```bash
git add docs/superpowers/plans/2026-07-19-shared-ssh-reverse-forward.md
git commit -m "Document shared SSH forward verification"
```
