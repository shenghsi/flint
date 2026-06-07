## Context

The terminal-first-fork changes (`f277d2d123` and `784dae66f6`) stripped collaboration/agent features from the title bar and hid most remaining title bar content via default settings. The title bar rendering code is structurally intact — `TitleBar::init` still observes new workspaces and calls `set_titlebar_item` — but the resulting bar may render as an empty transparent strip because:

1. `render_call_controls` and `render_collaborator_list` were removed, taking their children with them.
2. Default settings hide user picture, user menu, sign-in button, and onboarding banner.
3. The project name and branch picker remain, but if no git repo is open the bar has almost no visible content.

Additionally, `terminal.button` was set to `false`, hiding the terminal icon from the status bar — the primary affordance for the terminal-first workflow.

## Goals / Non-Goals

**Goals:**
- Ensure the title bar renders with visible, meaningful content on every project (with or without git)
- Restore the terminal icon in the status bar so users can toggle the terminal panel

**Non-Goals:**
- Re-introducing collaboration or agent features into the title bar
- Changing the title bar layout or height
- Modifying how center-pane terminals work (that remains unchanged from terminal-first-fork)

## Decisions

### 1. Restore `terminal.button` default to `true`

The terminal-first workflow relies on a discoverable terminal entry point. The status bar icon is the standard mechanism. Revert the default in `assets/settings/default.json` from `false` to `true`.

Rationale: Users who already have `terminal.button: false` in their personal settings are unaffected. Only the default for new/unchanged configurations changes.

### 2. Keep title bar content settings at their current defaults

Rather than reverting `show_user_picture`, `show_user_menu`, etc. back to `true`, leave them at their terminal-first-fork values. These are user-session features (sign-in, account menu) that are orthogonal to the title bar's structural visibility.

### 3. Ensure the title bar always shows project name

The project name is the one element that should always be visible. Verify that `show_project_items` remains `true` in defaults (it does) and that the `render_project_items` conditional in `TitleBar::render` is satisfied when a project is open. If the project name is rendering but the bar still appears empty, the issue is likely a missing height or the platform title bar not being drawn.

### 4. Diagnose and fix the actual rendering failure

The most likely cause is a runtime panic in the `TitleBar::new` constructor or in `Render::render` that is being swallowed. The `784dae66f6` commit removed fields (`screen_share_popover_handle`, `_diagnostics_subscription`) and their initialization. If the constructor still references removed types or has mismatched field counts, it would fail at compile time — but since the app runs, the issue may be in a dependent observer or subscription that fails silently at runtime.

Approach: Run the app with `RUST_BACKTRACE=1` and inspect stderr for panics in the title_bar crate. If no panic, check whether `titlebar_item` on the workspace is `Some` after init.

## Risks / Trade-offs

- [Users who explicitly set `terminal.button: false`] → Their setting is preserved; only defaults change.
- [The title bar issue may be platform-specific] → Test on macOS first since that's the primary platform for this fork.
