## 1. Global replace

- [x] 1.1 Run case-aware global replace of "Zed" → "Flint" and "zed" → "flint" across all source files, Cargo.toml files, resource files, keymaps, settings, and docs (excluding `.git/`, `target/`, `Cargo.lock`, binary assets like PNGs/ICNS, and `openspec/changes/archive/`)
- [x] 1.2 Rename files that have "zed" in their name: `zed.desktop.in`, `zed.metainfo.xml.in`, `zed.iss`, `zed.entitlements`, etc.

## 2. Revert extension-critical files

- [x] 2.1 Revert all 10 WIT files under `crates/extension_api/wit/*/extension.wit` back to `package zed:extension` (global replace will have changed these to `flint:extension`, breaking all extensions)
- [x] 2.2 Revert `crates/paths/src/paths.rs` — keep `.zed` folder names (`.zed/settings.json`, `.zed/tasks.json`, `.zed_server`, `.zed_wsl_server`) unchanged for project compatibility
- [x] 2.3 Revert `crates/http_client/src/http_client.rs` — keep `zed.dev` → `api.zed.dev` URL mapping so extensions download from Zed's registry

## 3. Fix compilation

- [x] 3.1 Run `cargo check --workspace` and fix all compilation errors from the global replace (broken crate references, mismatched string literals, renamed imports, etc.)
- [x] 3.2 Fix any test compilation failures

## 4. Disable auto-update and update external URLs

- [x] 4.1 Set `"auto_update": false` in `assets/settings/default.json`
- [x] 4.2 Update feedback/report URLs to point to the fork's own repo (or disable feedback entirely)

## 5. Verify and smoke test

- [x] 5.1 Build with `cargo build --package flint --bin flint` and confirm success
- [ ] 5.2 Run `./target/debug/flint` and verify: title bar shows "Flint", menus show "About Flint", config at `~/.config/flint/`
- [ ] 5.3 Install an extension from Zed's registry and confirm it loads
- [ ] 5.4 Run `./script/clippy` and fix any new warnings
