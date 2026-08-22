## 1. Diagnose title bar rendering failure

- [x] 1.1 Run the app with `RUST_BACKTRACE=1` and inspect stderr for panics in the `title_bar` crate
- [x] 1.2 If no panic, add a temporary log in `TitleBar::new` and `Render::render` to confirm the constructor runs and `render` is called
- [x] 1.3 Check whether `workspace.titlebar_item()` returns `Some` after workspace init by inspecting the `titlebar_item` field

## 2. Fix title bar visibility

- [x] 2.1 Fix the root cause identified in step 1 (constructor panic, missing subscription, silent error, etc.)
- [x] 2.2 Verify the title bar renders with visible content (project name) when opening a project with a git repo
- [x] 2.3 Verify the title bar renders with visible content when opening a project without a git repo

## 3. Restore terminal icon in status bar

- [x] 3.1 In `assets/settings/default.json`, change `"button": false` to `"button": true` under the `"terminal"` section
- [x] 3.2 Launch the app and confirm the terminal icon appears in the bottom status bar
- [x] 3.3 Click the terminal icon and confirm it toggles the terminal panel dock

## 4. Verify end-to-end

- [x] 4.1 Open a project, confirm title bar shows project name and terminal icon is visible in status bar
- [x] 4.2 Run `./script/clippy` and confirm no new warnings or errors
