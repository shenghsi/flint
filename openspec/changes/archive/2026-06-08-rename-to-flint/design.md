## Context

This fork has diverged from Zed with terminal-first defaults, external CLI agents, and stripped product surfaces. It now needs its own identity: "Flint". The rename touches the binary, app identity, filesystem paths, URL schemes, menus, and packaging — but must preserve extension compatibility.

## Goals / Non-Goals

**Goals:**
- Binary runs as `flint` with its own config/data directories (`~/.config/flint/`)
- App menus, about dialog, and window chrome show "Flint"
- URL scheme changes from `zed://` to `flint://`
- Existing Zed extensions load without modification
- Separate app icons

**Non-Goals:**
- Creating a new extension registry (keep using Zed's)
- Renaming the WIT namespace `zed:extension` (breaks extensions)
- Renaming the `.zed/` project settings folder (breaks existing projects)
- Renaming env vars like `ZED_SERVER_URL` (internal, not user-facing)
- Renaming the `zed` action namespace in `zed_actions`
- Auto-update from Zed's infrastructure (fork manages its own updates)

## Decisions

### 1. Keep WIT namespace as `zed:extension`

The WIT `package zed:extension` is baked into every compiled extension's Wasm component type. Changing it to `flint:extension` would break all existing Zed extensions at load time. Since this fork wants extensions to work, the WIT namespace stays unchanged. This is an internal implementation detail — users never see it.

Alternative considered: Fork the WIT namespace and maintain a parallel extension ecosystem. Rejected because it duplicates effort for no user benefit.

### 2. Keep `.zed/` project settings folder

Projects contain `.zed/settings.json` and `.zed/tasks.json`. Renaming to `.flint/` would break every existing project. Keep `.zed/` for compatibility. The `local_settings_folder_name()` function returns `.zed` and stays unchanged.

### 3. Keep env vars as `ZED_*`

Environment variables like `ZED_SERVER_URL`, `ZED_RELEASE_CHANNEL`, `ZED_ALLOW_EMULATED_GPU` are internal/debug tools. Renaming them provides no user value and breaks existing scripts. Keep them.

### 4. Disable auto-update

The auto-update system points to Zed's GitHub releases and cloud infrastructure. Since this is an independent fork, disable auto-update by default. Remove or stub the update check rather than pointing at a non-existent Flint release server.

### 5. Keep extension download from Zed's API

Extensions are fetched from `https://api.zed.dev/extensions` via the `server_url` setting. Keep this default so users can install extensions from Zed's registry. The extension host code doesn't need changes.

## Risks / Trade-offs

- [Config directory migration] → Users with existing `~/.config/zed/` settings need to manually copy to `~/.config/flint/`. Document this in a migration note.
- [WIT namespace stays "zed"] → Internally inconsistent branding. Acceptable trade-off for extension compatibility.
- [`.zed/` folder stays] → Projects show a "zed" folder in Flint. Acceptable since many tools use vendor-specific dotfolders.
- [Auto-update disabled] → Users need to build from source or manage updates manually until a Flint release pipeline exists.
