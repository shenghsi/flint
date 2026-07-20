# Through-Flint Agent Launch and Menu Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every Codex and Claude new/resume action use the pinned managed CLI in Through-Flint mode while reducing the dropdown to ordinary launch options and remote sign-out.

**Architecture:** `store.rs` becomes the single source of truth for whether the selected workspace route requires managed execution. New-thread command construction is shared by configured and managed launches so arguments, environment, and Claude session IDs remain identical; `panel.rs` only decides whether the explicit managed row is meaningful for the current route.

**Tech Stack:** Rust, GPUI tasks and context menus, Flint remote-agent routing, managed-agent provisioning, Cargo tests.

## Global Constraints

- Change Through-Flint behavior only.
- Keep Not-through-Flint menu, ambient launch, explicit managed row, and resume behavior unchanged.
- Apply the Through-Flint rule equally to Codex and Claude.
- Every new-thread entry point, resume option, and automatic restoration must use the pinned managed executable through Flint.
- Keep provisioning status in the existing popup notification; do not render a separate managed/status row in Through-Flint mode.
- Repeated launch attempts must not start duplicate provisioning.
- Do not launch Claude or contact Anthropic during automated verification.

---

### Task 1: Shared Through-Flint route policy and managed resume

**Files:**
- Modify: `crates/agent_threads/src/store.rs:800-960`
- Test: `crates/agent_threads/src/store.rs` test module

**Interfaces:**
- Consumes: `settings::RemoteAgentRoute` and `current_remote_agent_route`.
- Produces: `uses_managed_agent_route(route: Option<settings::RemoteAgentRoute>) -> bool`, shared by new threads, resume/restoration, and credentials; plus `pub(crate) fn workspace_uses_through_flint(workspace: &Workspace, cx: &App) -> bool` for panel visibility.

- [x] **Step 1: Make the existing resume regression require both agents**

Replace the Codex-only assertion with:

```rust
#[test]
fn only_through_flint_resume_uses_managed_resolution_for_both_agents() {
    for kind in agent_kind_registry() {
        assert!(uses_managed_resume(
            &kind,
            Some(settings::RemoteAgentRoute::ThroughFlint)
        ));
        assert!(!uses_managed_resume(
            &kind,
            Some(settings::RemoteAgentRoute::NotThroughFlint)
        ));
        assert!(!uses_managed_resume(&kind, None));
    }
}
```

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p agent_threads only_through_flint_resume_uses_managed_resolution_for_both_agents
```

Expected: FAIL for Claude in Through-Flint mode because `uses_managed_resume` is Codex-only.

- [x] **Step 3: Introduce the shared route predicate**

Add:

```rust
fn uses_managed_agent_route(route: Option<settings::RemoteAgentRoute>) -> bool {
    route == Some(settings::RemoteAgentRoute::ThroughFlint)
}

pub(crate) fn workspace_uses_through_flint(workspace: &Workspace, cx: &App) -> bool {
    uses_managed_agent_route(current_remote_agent_route(workspace, cx))
}
```

Make both existing predicates delegate to it:

```rust
fn uses_managed_resume(
    _kind: &AgentKindDefinition,
    route: Option<settings::RemoteAgentRoute>,
) -> bool {
    uses_managed_agent_route(route)
}

fn uses_managed_credential_command(route: Option<settings::RemoteAgentRoute>) -> bool {
    uses_managed_agent_route(route)
}
```

Keep the `kind` parameter on the resume wrapper because resume callers already
pass it and the signature documents that the policy applies to each registered
agent.

- [x] **Step 4: Generalize the route-change error copy**

Change the shared error to cover new and resumed sessions:

```rust
anyhow::bail!("the agent route changed while preparing the session; launch it again")
```

Update `required_resume_route_rejects_a_route_change` to expect that exact text.

- [x] **Step 5: Run focused resume and route tests and verify GREEN**

Run:

```bash
cargo test -p agent_threads only_through_flint_resume_uses_managed_resolution_for_both_agents
cargo test -p agent_threads required_resume_route_rejects_a_route_change
```

Expected: both tests pass. The automatic restoration path already calls `resume_thread_task`, so it now shares the same managed selection for Claude without another dispatch branch.

- [x] **Step 6: Commit the resume policy slice**

```bash
git add crates/agent_threads/src/store.rs
git commit -m "Route Claude resume through Flint"
```

---

### Task 2: Shared new-thread command construction

**Files:**
- Modify: `crates/agent_threads/src/store.rs:560-610`
- Modify: `crates/agent_threads/src/store.rs:950-990`
- Test: `crates/agent_threads/src/store.rs` test module

**Interfaces:**
- Consumes: `AgentKindDefinition::session_id_flag`, `AgentLaunchCommand`, launch-option arguments, managed executable path, and `apply_self_update_policy`.
- Produces: `NewThreadLaunch { command: AgentLaunchCommand, session_id: Option<SharedString> }` and `build_new_thread_launch(kind, base, extra_args, managed_executable) -> NewThreadLaunch`.

- [x] **Step 1: Write failing configured/managed command-equivalence tests**

Add:

```rust
#[test]
fn new_thread_builder_preserves_options_environment_and_managed_executable() {
    for kind in agent_kind_registry() {
        let mut environment = HashMap::default();
        environment.insert("EXISTING".to_string(), "value".to_string());
        let base = AgentLaunchCommand {
            command: Some(format!("ambient-{}", kind.id)),
            args: vec!["base".to_string()],
            env: environment,
            ..AgentLaunchCommand::default()
        };
        let managed_executable = PathBuf::from(format!("/managed/{}/cli", kind.id));
        let option_arguments = kind.resume_options[0].args.clone();

        let launch = build_new_thread_launch(
            &kind,
            &base,
            &option_arguments,
            Some(&managed_executable),
        );

        assert_eq!(launch.command.command.as_deref(), managed_executable.to_str());
        assert_eq!(
            launch.command.env.get("EXISTING").map(String::as_str),
            Some("value")
        );
        assert!(launch.command.args.starts_with(&[
            "base".to_string(),
            option_arguments[0].clone(),
        ]));
        let mut expected = launch.command.clone();
        apply_self_update_policy(&mut expected, &kind);
        assert_eq!(launch.command, expected);
    }
}

#[test]
fn managed_claude_new_thread_keeps_its_generated_session_id() {
    let kind = agent_kind_registry()
        .into_iter()
        .find(|kind| kind.id == "claude")
        .expect("Claude should be registered");
    let launch = build_new_thread_launch(
        &kind,
        &AgentLaunchCommand::default(),
        &[],
        Some(std::path::Path::new("/managed/claude")),
    );

    let session_id = launch.session_id.expect("Claude launch should have a session id");
    assert!(launch.command.args.ends_with(&[
        "--session-id".to_string(),
        session_id.to_string(),
    ]));
}
```

- [x] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p agent_threads new_thread_builder
cargo test -p agent_threads managed_claude_new_thread_keeps_its_generated_session_id
```

Expected: compilation fails because `NewThreadLaunch` and `build_new_thread_launch` do not exist.

- [x] **Step 3: Implement the pure launch builder**

Add near the existing command builders:

```rust
struct NewThreadLaunch {
    command: AgentLaunchCommand,
    session_id: Option<SharedString>,
}

fn build_new_thread_launch(
    kind: &AgentKindDefinition,
    base: &AgentLaunchCommand,
    extra_args: &[String],
    managed_executable: Option<&std::path::Path>,
) -> NewThreadLaunch {
    let mut command = base.clone();
    command.args.extend(extra_args.iter().cloned());
    let session_id = kind.session_id_flag.map(|flag| {
        let session_id = SharedString::from(uuid::Uuid::new_v4().to_string());
        command.args.push(flag.to_string());
        command.args.push(session_id.to_string());
        session_id
    });
    if let Some(managed_executable) = managed_executable {
        command.command = Some(managed_executable.to_string_lossy().into_owned());
        apply_self_update_policy(&mut command, kind);
    }
    NewThreadLaunch {
        command,
        session_id,
    }
}
```

- [x] **Step 4: Replace configured and managed command duplication**

Replace the configured command assembly in `launch_new_thread` with:

```rust
let base = AgentThreadSettings::get_global(cx)
    .command_for_kind(kind.id)
    .clone();
let launch = build_new_thread_launch(kind, &base, extra_args, None);
spawn_thread(
    workspace,
    kind,
    kind.label.clone(),
    launch.command,
    launch.session_id,
    window,
    cx,
);
```

Inside the `ManagedAgentPreparation::Ready` update closure, replace its command
assembly with:

```rust
let base = AgentThreadSettings::get_global(cx)
    .command_for_kind(kind.id)
    .clone();
let launch = build_new_thread_launch(
    &kind,
    &base,
    &extra_args,
    Some(&prepared.installation.executable_path),
);
spawn_thread(
    workspace,
    &kind,
    SharedString::from(format!("New {} thread", kind.label)),
    launch.command,
    launch.session_id,
    window,
    cx,
);
```

Do not alter the explicit managed function's Not-through-Flint routing in this
task.

- [x] **Step 5: Run the focused tests and full command-builder tests**

Run:

```bash
cargo test -p agent_threads new_thread
cargo test -p agent_threads managed_credential_commands
cargo test -p agent_threads managed_resume_replaces
```

Expected: all matching tests pass.

- [x] **Step 6: Commit the command-construction slice**

```bash
git add crates/agent_threads/src/store.rs
git commit -m "Share managed new-thread command construction"
```

---

### Task 3: Route-aware ordinary new-thread dispatch

**Files:**
- Modify: `crates/agent_threads/src/store.rs:560-620`
- Modify: `crates/agent_threads/src/store.rs:950-990`
- Modify: `crates/agent_threads/src/store.rs:1190-1245`
- Test: `crates/agent_threads/src/store.rs` test module

**Interfaces:**
- Consumes: `uses_managed_agent_route`, `current_remote_agent_route`, `prepare_managed_agent`, `build_new_thread_launch`, `RequiredAgentRoute`, and `spawn_thread_task_for_route`.
- Produces: ordinary `launch_new_thread` dispatch that selects managed preparation only for Through Flint, plus `launch_managed_thread_for_route(..., required_route: Option<RequiredAgentRoute>)` for shared orchestration.

- [x] **Step 1: Write the failing new-thread dispatch-policy test**

Add the test against the intended dispatch seam before defining the enum or
function:

```rust
#[test]
fn new_thread_launch_route_is_managed_only_through_flint() {
    assert_eq!(
        new_thread_launch_route(Some(settings::RemoteAgentRoute::ThroughFlint)),
        NewThreadLaunchRoute::ManagedThroughFlint
    );
    assert_eq!(
        new_thread_launch_route(Some(settings::RemoteAgentRoute::NotThroughFlint)),
        NewThreadLaunchRoute::Configured
    );
    assert_eq!(new_thread_launch_route(None), NewThreadLaunchRoute::Configured);
}
```

The production seam introduced after RED is:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewThreadLaunchRoute {
    Configured,
    ManagedThroughFlint,
}

fn new_thread_launch_route(
    route: Option<settings::RemoteAgentRoute>,
) -> NewThreadLaunchRoute;
```

Assert `ManagedThroughFlint` only for Through Flint and `Configured` otherwise.

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p agent_threads new_thread_launch_route_is_managed_only_through_flint
```

Expected: compilation fails because `NewThreadLaunchRoute` and
`new_thread_launch_route` do not exist.

- [x] **Step 3: Implement route dispatch and required-route spawning**

Implement the enum/helper as:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewThreadLaunchRoute {
    Configured,
    ManagedThroughFlint,
}

fn new_thread_launch_route(
    route: Option<settings::RemoteAgentRoute>,
) -> NewThreadLaunchRoute {
    if uses_managed_agent_route(route) {
        NewThreadLaunchRoute::ManagedThroughFlint
    } else {
        NewThreadLaunchRoute::Configured
    }
}
```

Extract the old configured body to `launch_configured_thread`. Make public
`launch_new_thread` choose:

```rust
match new_thread_launch_route(current_remote_agent_route(workspace, cx)) {
    NewThreadLaunchRoute::Configured => {
        launch_configured_thread(workspace, kind, extra_args, window, cx)
    }
    NewThreadLaunchRoute::ManagedThroughFlint => launch_managed_thread_for_route(
        workspace,
        kind,
        extra_args,
        Some(RequiredAgentRoute(settings::RemoteAgentRoute::ThroughFlint)),
        window,
        cx,
    ),
}
```

Extract the existing managed orchestration behind this exact interface:

```rust
fn launch_managed_thread_for_route(
    workspace: &mut Workspace,
    kind: &AgentKindDefinition,
    extra_args: &[String],
    required_route: Option<RequiredAgentRoute>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
)
```

Keep public `launch_managed_thread` as a Not-through-Flint-compatible wrapper
that passes `None`. In the ready update closure, call
`build_new_thread_launch`, then create the task with:

```rust
spawn_thread_task_for_route(
    workspace,
    &kind,
    SharedString::from(format!("New {} thread", kind.label)),
    launch.command,
    launch.session_id,
    required_route,
    window,
    cx,
)
```

Await that task in the surrounding async block. On error, call the existing
`workspace.show_error` path. Preserve `Cancelled` and `AlreadyInProgress` as
no-launch outcomes.

- [x] **Step 4: Run focused dispatch, provisioning, and route-guard tests**

Run:

```bash
cargo test -p agent_threads new_thread_launch_route_is_managed_only_through_flint
cargo test -p agent_threads managed_agent
cargo test -p agent_threads required_resume_route_rejects_a_route_change
```

Expected: all matching tests pass. Existing `ManagedAgentPreparation::AlreadyInProgress` behavior continues showing the active notification without launching or downloading twice.

- [x] **Step 5: Commit the route-aware new-thread slice**

```bash
git add crates/agent_threads/src/store.rs
git commit -m "Route new agent threads through Flint"
```

---

### Task 4: Through-Flint dropdown simplification

**Files:**
- Modify: `crates/agent_threads/src/panel.rs:550-675`
- Test: `crates/agent_threads/src/panel.rs` test module

**Interfaces:**
- Consumes: `store::workspace_uses_through_flint`, `managed_available`, and the existing popup managed-provisioning notification.
- Produces: `show_explicit_managed_launch(managed_available: bool, through_flint: bool) -> bool`, used to render the explicit managed row only outside Through-Flint mode.

- [x] **Step 1: Write the failing menu-visibility test**

Add:

```rust
#[test]
fn explicit_managed_launch_is_hidden_only_in_through_flint_mode() {
    assert!(!show_explicit_managed_launch(true, true));
    assert!(show_explicit_managed_launch(true, false));
    assert!(!show_explicit_managed_launch(false, true));
    assert!(!show_explicit_managed_launch(false, false));
}
```

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p agent_threads explicit_managed_launch_is_hidden_only_in_through_flint_mode
```

Expected: compilation fails because `show_explicit_managed_launch` does not exist.

- [x] **Step 3: Implement the menu policy and avoid hidden status reads**

Add:

```rust
fn show_explicit_managed_launch(managed_available: bool, through_flint: bool) -> bool {
    managed_available && !through_flint
}
```

Compute `through_flint` by calling `store::workspace_uses_through_flint` on the
upgraded workspace. Construct
`managed_label` only when `show_explicit_managed_launch` is true, so an active
provisioning state is never appended to a hidden row. Render the existing
explicit managed entry only when that label is present. Leave ordinary
new-thread option rows and remote sign-out unchanged; their existing calls now
reach the centralized route-aware store dispatch.

Use this shape before building the menu closure:

```rust
let through_flint = workspace.upgrade().is_some_and(|workspace| {
    store::workspace_uses_through_flint(workspace.read(cx), cx)
});
let managed_label = show_explicit_managed_launch(managed_available, through_flint)
    .then(|| {
        workspace.upgrade().map_or_else(
            || SharedString::from(format!("New — Flint-managed {}", kind.label)),
            |workspace| store::managed_agent_launch_label(workspace.read(cx), &kind, cx),
        )
    });
```

Replace `if managed_available` with `if let Some(managed_label) = managed_label`
around the existing entry.

- [x] **Step 4: Run focused menu and launch-option tests**

Run:

```bash
cargo test -p agent_threads explicit_managed_launch_is_hidden_only_in_through_flint_mode
cargo test -p agent_threads launch_option
cargo test -p agent_threads remote_credential_menus_only_offer_sign_out
```

Expected: all matching tests pass.

- [x] **Step 5: Commit the menu slice**

```bash
git add crates/agent_threads/src/panel.rs
git commit -m "Simplify Through-Flint agent menus"
```

---

### Task 5: Verification, documentation, and local app bundle

**Files:**
- Modify: `docs/superpowers/specs/2026-07-20-through-flint-agent-launch-menu-design.md`
- Modify: `docs/superpowers/plans/2026-07-20-through-flint-agent-launch-menu-implementation.md`

**Interfaces:**
- Consumes: completed route-aware new/resume behavior and menu policy.
- Produces: an automated verification record, explicit live-Claude deferral, and a fresh `/tmp/Flint-Local.app` for manual Codex validation.

- [x] **Step 1: Run complete automated verification**

Run:

```bash
cargo test -p agent_threads
cargo fmt --all -- --check
./script/clippy -p agent_threads
git diff --check
```

Expected: every command exits successfully.

- [x] **Step 2: Build and verify the local app**

Run:

```bash
./script/bundle-tmp-app
```

If the documented debug-build release-path defect stops the script after the
fresh bundle is created, preserve the old `/tmp/Flint-Local.app`, copy
`target/aarch64-apple-darwin/debug/bundle/osx/Flint.app` into its place, compare
the source and copied `Contents/MacOS/flint` SHA-256 digests, and run
`codesign --verify --deep --strict /tmp/Flint-Local.app`.

- [x] **Step 3: Record verified scope**

Set the design status to `Implemented and automatically verified`, record exact
test/check results, and mark completed plan checkboxes. State that live Claude
validation remains deferred and that Not-through-Flint was protected by unit
policy tests rather than changed.

- [x] **Step 4: Commit the verification record**

```bash
git add docs/superpowers/specs/2026-07-20-through-flint-agent-launch-menu-design.md docs/superpowers/plans/2026-07-20-through-flint-agent-launch-menu-implementation.md
git commit -m "Document Through-Flint launch verification"
```
