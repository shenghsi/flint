# Remote Agent Credential Menu and Sign-Out Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce remote Codex and Claude credential menus to sign-out only, and guarantee that Through-Flint sign-out uses the corresponding pinned managed CLI.

**Architecture:** `panel.rs` owns a small, testable policy describing which remote credential entries are visible. `store.rs` owns route selection, managed-agent preparation, command construction, and launch; pure command-building helpers provide deterministic regression seams while the existing provisioning and Through-Flint transport remain authoritative.

**Tech Stack:** Rust, GPUI context menus and tasks, Flint remote-agent routing, Flint managed-agent provisioning, Cargo tests.

## Global Constraints

- Remove remote sign-in, sign-in-status, and provider-management entries for Codex and Claude.
- Keep remote sign-out for both agents.
- Through-Flint sign-out must use the pinned managed executable and require the Through-Flint route.
- Not-through-Flint sign-out must keep using the configured command.
- Do not change provider-owned authentication storage.
- Do not perform live Claude validation in this iteration; automated tests must not launch Claude or contact Anthropic.

---

### Task 1: Sign-out-only remote credential menus

**Files:**
- Modify: `crates/agent_threads/src/panel.rs:648-756`
- Test: `crates/agent_threads/src/panel.rs` test module

**Interfaces:**
- Consumes: `AgentKindDefinition::id` and the existing `remote_available` menu condition.
- Produces: `remote_credential_menu_policy(kind: &AgentKindDefinition) -> RemoteCredentialMenuPolicy`, used only by remote credential menu rendering.

- [x] **Step 1: Write the failing menu-policy test**

Add a pure policy value with four named booleans to the test expectation, before defining it in production code:

```rust
#[test]
fn codex_and_claude_remote_credential_menus_only_offer_sign_out() {
    for kind in agent_kind_registry() {
        assert_eq!(
            remote_credential_menu_policy(&kind),
            RemoteCredentialMenuPolicy {
                sign_in: false,
                sign_in_status: false,
                sign_out: true,
                provider_management: false,
            },
            "{} remote credential menu policy",
            kind.id
        );
    }
}
```

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p agent_threads codex_and_claude_remote_credential_menus_only_offer_sign_out
```

Expected: compilation fails because `RemoteCredentialMenuPolicy` and `remote_credential_menu_policy` do not exist.

- [x] **Step 3: Add the minimal policy and apply it to menu rendering**

Define the private copyable policy beside the panel menu helpers:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RemoteCredentialMenuPolicy {
    sign_in: bool,
    sign_in_status: bool,
    sign_out: bool,
    provider_management: bool,
}

fn remote_credential_menu_policy(_kind: &AgentKindDefinition) -> RemoteCredentialMenuPolicy {
    RemoteCredentialMenuPolicy {
        sign_in: false,
        sign_in_status: false,
        sign_out: true,
        provider_management: false,
    }
}
```

In `deploy_new_thread_options_menu`, compute the policy inside `remote_available`, add the credential-section separator once, and guard each existing entry with its corresponding field. This preserves the sign-out confirmation copy and call site while hiding the other three actions for both registered kinds.

- [x] **Step 4: Run the focused test and verify GREEN**

Run:

```bash
cargo test -p agent_threads codex_and_claude_remote_credential_menus_only_offer_sign_out
```

Expected: PASS.

- [x] **Step 5: Commit the menu slice**

```bash
git add crates/agent_threads/src/panel.rs
git commit -m "Simplify remote agent credential menus"
```

---

### Task 2: Route-aware managed sign-out

**Files:**
- Modify: `crates/agent_threads/src/store.rs:595-620`
- Modify: `crates/agent_threads/src/store.rs:826-914`
- Test: `crates/agent_threads/src/store.rs` test module

**Interfaces:**
- Consumes: `current_remote_agent_route`, `prepare_managed_agent`, `spawn_thread_task_for_route`, `RequiredAgentRoute`, `AgentCredentialPolicy::logout_arguments`, and `apply_self_update_policy`.
- Produces: `build_credential_command(base: &AgentLaunchCommand, arguments: &[&str]) -> AgentLaunchCommand`, `build_managed_credential_command(kind: &AgentKindDefinition, base: &AgentLaunchCommand, arguments: &[&str], managed_executable: &std::path::Path) -> AgentLaunchCommand`, and `uses_managed_credential_command(route: Option<RemoteAgentRoute>) -> bool`.

- [x] **Step 1: Write failing command-selection tests**

Add tests that use only pure command values and pinned-path fixtures:

```rust
#[test]
fn configured_credential_command_keeps_the_ambient_executable() {
    let base = AgentLaunchCommand {
        command: Some("custom-codex".to_string()),
        args: vec!["ignored".to_string()],
        ..AgentLaunchCommand::default()
    };

    let command = build_credential_command(&base, &["logout"]);

    assert_eq!(command.command.as_deref(), Some("custom-codex"));
    assert_eq!(command.args, vec!["logout".to_string()]);
}

#[test]
fn managed_credential_commands_use_each_pinned_executable_and_update_policy() {
    for kind in agent_kind_registry() {
        let mut environment = HashMap::default();
        environment.insert("EXISTING".to_string(), "value".to_string());
        let base = AgentLaunchCommand {
            command: Some(format!("custom-{}", kind.id)),
            env: environment,
            ..AgentLaunchCommand::default()
        };
        let managed_executable = PathBuf::from(format!("/managed/{}/cli", kind.id));
        let arguments = kind.credential_policy().logout_arguments;

        let command = build_managed_credential_command(
            &kind,
            &base,
            arguments,
            &managed_executable,
        );

        assert_eq!(command.command.as_deref(), managed_executable.to_str());
        assert_eq!(command.env.get("EXISTING").map(String::as_str), Some("value"));
        let mut expected = build_credential_command(&base, arguments);
        expected.command = Some(managed_executable.to_string_lossy().into_owned());
        apply_self_update_policy(&mut expected, &kind);
        assert_eq!(command, expected);
    }
}

#[test]
fn only_through_flint_credential_commands_use_managed_provisioning() {
    assert!(uses_managed_credential_command(Some(
        settings::RemoteAgentRoute::ThroughFlint
    )));
    assert!(!uses_managed_credential_command(Some(
        settings::RemoteAgentRoute::NotThroughFlint
    )));
    assert!(!uses_managed_credential_command(None));
}
```

- [x] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p agent_threads credential_command
```

Expected: compilation fails because the three helper functions do not exist.

- [x] **Step 3: Implement pure command construction**

Add the helpers near the existing resume command builders:

```rust
fn build_credential_command(
    base: &AgentLaunchCommand,
    arguments: &[&str],
) -> AgentLaunchCommand {
    let mut command = base.clone();
    command.args = arguments
        .iter()
        .map(|argument| (*argument).to_string())
        .collect();
    command
}

fn build_managed_credential_command(
    kind: &AgentKindDefinition,
    base: &AgentLaunchCommand,
    arguments: &[&str],
    managed_executable: &std::path::Path,
) -> AgentLaunchCommand {
    let mut command = build_credential_command(base, arguments);
    command.command = Some(managed_executable.to_string_lossy().into_owned());
    apply_self_update_policy(&mut command, kind);
    command
}

fn uses_managed_credential_command(route: Option<settings::RemoteAgentRoute>) -> bool {
    route == Some(settings::RemoteAgentRoute::ThroughFlint)
}
```

- [x] **Step 4: Make credential launch route-aware**

Update `launch_credential_command` to read the selected route before launching. For Not-through-Flint or no remote route, build the configured command and pass the captured route as `RequiredAgentRoute` to `spawn_thread_task_for_route`.

For Through Flint, call `prepare_managed_agent`. On `Ready`, set the existing progress notification to `ManagedAgentProgressState::Launching`, construct the command with `build_managed_credential_command`, dismiss the managed-agent notification, and call `spawn_thread_task_for_route` with `Some(RequiredAgentRoute(RemoteAgentRoute::ThroughFlint))`. Await the returned launch task so transport errors reach `workspace.show_error`. Treat `Cancelled` and `AlreadyInProgress` the same way as `launch_managed_thread`: do not launch another command. Preserve all existing provisioning progress and confirmation behavior.

- [x] **Step 5: Run focused and crate tests and verify GREEN**

Run:

```bash
cargo test -p agent_threads credential_command
cargo test -p agent_threads
```

Expected: all tests pass; no provider CLI is launched.

- [x] **Step 6: Commit the managed sign-out slice**

```bash
git add crates/agent_threads/src/store.rs
git commit -m "Route remote agent sign-out through Flint"
```

---

### Task 3: Verification and design status

**Files:**
- Modify: `docs/superpowers/specs/2026-07-19-remote-agent-credential-menu-design.md`
- Modify: `docs/superpowers/plans/2026-07-19-remote-agent-credential-menu-implementation.md`

**Interfaces:**
- Consumes: the completed menu and command-routing behavior from Tasks 1 and 2.
- Produces: an implementation record with automated verification results and an explicit live-Claude deferral.

- [x] **Step 1: Run repository checks**

Run:

```bash
cargo fmt --all -- --check
./script/clippy -p agent_threads
git diff --check
```

Expected: all commands exit successfully.

- [x] **Step 2: Record completion without claiming live Claude validation**

Change the design status to `Implemented and automatically verified`; record the focused test, full crate test, formatting, and clippy results. Mark every completed plan checkbox. State explicitly that live Claude remote validation remains pending on the separate remote.

- [x] **Step 3: Commit the verification record**

```bash
git add docs/superpowers/specs/2026-07-19-remote-agent-credential-menu-design.md docs/superpowers/plans/2026-07-19-remote-agent-credential-menu-implementation.md
git commit -m "Document remote agent sign-out verification"
```
