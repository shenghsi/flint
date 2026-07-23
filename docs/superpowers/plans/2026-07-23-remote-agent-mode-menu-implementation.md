# Remote Agent Mode Menu Implementation Plan

**Goal:** Keep remote agent selection strictly aligned with the selected route
and expose Pi's existing visibility setting in the Settings UI.

**Design:** Remove the explicit managed-agent dropdown path. Gate remote
credential actions on Tunneled routing, while leaving the store's existing
route-authoritative launch, resume, credential, and egress behavior intact.
Add the missing Pi setting beside the existing agent visibility controls.

## Task 1: Lock the menu policy to Tunneled routing

**Files:**

- Modify: `crates/agent_threads/src/panel.rs`

1. Replace the existing managed-row visibility test with a failing test that
   allows remote credential actions only when both the workspace is remote and
   its route is Tunneled.
2. Run the focused test and confirm it fails because Direct currently exposes
   sign-out.
3. Add the smallest route-aware credential-menu predicate.
4. Remove the explicit Flint-managed menu row and its panel-only helpers.
5. Run the focused panel tests.

## Task 2: Lock the Direct/Tunneled execution boundary

**Files:**

- Verify: `crates/agent_threads/src/store.rs`

1. Run the existing route-selection tests for new threads, resume, credentials,
   route-change rejection, and tunneled egress.
2. Strengthen a test only if the Direct ambient-command or Tunneled
   managed-command invariant lacks coverage.
3. Do not change production routing unless a failing invariant exposes a
   separate defect.

## Task 3: Add Hide Pi to the Settings UI

**Files:**

- Modify: `crates/settings_ui/src/page_data.rs`

1. Add a failing Settings UI test that expects Hide Codex, Hide Claude, and
   Hide Pi to map to their exact JSON paths.
2. Run the focused test and confirm it fails because Hide Pi is absent.
3. Add the Hide Pi setting item beside the existing controls.
4. Run the focused Settings UI tests.

## Task 4: Verify and deliver

1. Run `cargo test -p agent_threads`.
2. Run `cargo test -p settings_ui`.
3. Run `cargo fmt --all -- --check`.
4. Run `./script/clippy -p agent_threads -p settings_ui`.
5. Commit the implementation, push the feature branch, and open a pull request
   with the required release-notes section.
