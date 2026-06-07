## Why

After the terminal-first-fork changes, the app runs but the title bar is not visible and the terminal icon is missing from the bottom status bar. Both are critical chrome elements — the title bar provides project context and window controls, and the terminal icon is the primary entry point for the terminal-first workflow.

## What Changes

- Restore the title bar to a visible, functional state. The title bar code is intact but the `784dae66f6` commit removed collaboration-dependent rendering code (`render_call_controls`, `render_collaborator_list`, `ActiveCall` observer) that the `TitleBar::new` constructor previously relied on. The constructor may be failing silently or the bar renders but with zero visible content (user picture, menu, sign-in, and onboarding banner were all hidden via default settings in `f277d2d123`).
- Restore the terminal icon in the bottom status bar by reverting the `terminal.button` default from `false` back to `true`. This is the primary affordance for launching terminals in the terminal-first workflow.

## Capabilities

### New Capabilities

_None_

### Modified Capabilities

- `focused-product-surface`: The title bar was classified as a retained surface but is not currently functional. Requirements need updating to reflect that the title bar must render meaningfully without collaboration/agent features.

## Impact

- `assets/settings/default.json` — revert `terminal.button` to `true`
- `crates/title_bar/src/title_bar.rs` — investigate and fix the constructor/render path so the bar is visible
- `crates/terminal_view/src/terminal_panel.rs` — no code changes expected; the `button` setting already gates icon visibility
