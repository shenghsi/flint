# Agent-Initiated Worktree Control: Windows Support (plan only, not implemented)

## Context

`docs/superpowers/specs/2026-08-07-worktree-tied-agent-threads-panel-design.md`
("Stage 2") explicitly scoped agent-initiated worktree control
(`flint-agent-control retie-thread` / `create-thread`) to **local-only, Unix-only**,
calling out "Windows named-pipe transport" as a named non-goal — a deliberate choice to
avoid re-growing the complexity that sank an earlier, larger attempt at this same
feature. Since that doc was written, the implementation itself was reworked: the
original per-thread bearer-token design was replaced with kernel-verified
peer-credential authentication (`LOCAL_PEERPID`/`SO_PEERCRED` + process-ancestry
walking, with a cwd/kind fallback for CLIs like Codex that delegate shell execution to
a detached daemon), and a nudge feature was added that offers to write the worktree
discovery instructions into a CLI's global instructions file
(`crates/agent_threads/src/instructions.rs`).

This document is a plan for closing the Windows gap in a **later, separate pass** — it
proposes a design and inventories the concrete work, but implements nothing. It also
records a verification finding from writing this plan: two `pub(crate)` items added
during the peer-credential rework (`AgentThreadStore::live_terminal_pids` and
`live_terminal_worktree_roots`, plus their backing `_control_server_task` field and
`hold_control_server_task` setter) were reachable *only* from `#[cfg(unix)]`-gated
call sites without being `#[cfg(unix)]`-gated themselves — `pub(crate)` items with no
in-crate caller on a given platform are flagged by rustc's `dead_code` lint, which
`script/clippy.ps1`'s `--deny warnings` turns into a hard Windows CI failure. This has
been fixed directly (not deferred to this plan) by adding `#[cfg(unix)]` to each of
those items, verified with a minimal dependency-free reproduction cross-checked against
`x86_64-pc-windows-msvc` (the full workspace can't be cross-compiled from this macOS
environment — unrelated crates in the dependency tree require the MSVC toolchain's
`lib.exe`/`windows.h`, which aren't installed here). Confirm this reasoning holds by
watching `clippy_windows` on this branch's next real CI run (`.github/workflows/run_tests.yml`),
since no CI run has executed for this branch yet.

## What's already cross-platform vs. Unix-only today

Cross-platform, no changes needed for Windows:
- The worktree-tie concept itself (`AgentThreadMetadata::tied_worktree_root`,
  `resolve_tied_worktree`, retie/persistence, panel filtering) — `store.rs`, `panel.rs`,
  `history.rs`. None of this is platform-gated; it works identically on Windows today.
- `agent_control_protocol` (`crates/agent_control_protocol/src/agent_control_protocol.rs`):
  plain serde request/response types, `socket_path()`/`executable_location_path()`
  (both `paths::data_dir()`-based, already cross-platform). No unix-specific code.
- `agent_control_cli` as a *workspace member*: it already compiles on Windows today.
  `run()` has a `#[cfg(not(unix))]` stub (`agent_control_cli.rs:125-131`) that returns
  `Err("flint-agent-control is not supported on this platform")` rather than failing to
  build — this was a deliberate requirement from the original design doc ("a crate whose
  only `main` is `#[cfg(unix)]` fails to build with 'main function not found'"). CI's
  `clippy_windows` already exercises this; it just never does anything.

Unix-only, and the actual gap:
- `crates/agent_threads/src/control.rs`: the whole module is `#[cfg(unix)]`-gated
  (`agent_threads.rs:6-7`). Contains the socket server (`run_server`, `control.rs:90`),
  peer-PID resolution (`get_peer_pid`, `control.rs:206-224`, with separate
  `LOCAL_PEERPID`/`SO_PEERCRED` branches), and the ancestry/cwd/kind resolver
  (`resolve_caller_thread`/`resolve_by_cwd`, `control.rs:260-388`).
- `crates/agent_threads/src/instructions.rs`: also wholly `#[cfg(unix)]`-gated
  (`agent_threads.rs:11-12`) — the nudge-to-add-instructions feature.
- `agent_control_cli`'s actual client logic: `mod unix { ... }` (`agent_control_cli.rs:168+`),
  everything inside is Unix-only (`std::os::unix::net::UnixStream`).
- `util::get_flint_agent_control_path` (`util.rs:355-386`): macOS and Linux/FreeBSD
  branches only; every other target hits `anyhow::bail!("unsupported platform...")` at
  `util.rs:370`. Doesn't fail to *compile* on Windows (it's a plain `pub fn`, not
  platform-gated, just returns a runtime error) — but it's also never called from any
  Windows code path today, so the error never surfaces.
- Bundling: `script/bundle-mac:327` and `script/bundle-linux:126` each copy
  `flint-agent-control` into their bundle. `script/bundle-windows.ps1:137-141` builds
  and copies `Flint.exe`, `cli.exe`, `auto_update_helper.exe` — no
  `flint-agent-control.exe` step exists.

## Proposed design for a Windows pass

### Open question to resolve first: transport

Two candidate transports, in the order they should be evaluated:

1. **AF_UNIX on Windows.** Windows 10 (build 17063+) supports `AF_UNIX` sockets at the
   OS level. If the `net`/`async-net`/`smol` stack this codebase already uses
   (`net::async_net::{UnixListener, UnixStream}` in `control.rs`) turns out to support
   `AF_UNIX` on the `windows` target, the *entire* existing socket server and wire
   protocol could be reused as-is on Windows, and only peer-identification (below)
   would need a Windows-specific implementation. This has not been verified — check
   directly (a small standalone crate compiled for `x86_64-pc-windows-msvc`, same
   technique used to verify the dead-code fix above, would confirm or rule this out
   without needing a real Windows machine).
2. **Windows named pipes**, if (1) doesn't pan out. This is what the original design
   doc anticipated ("Windows named-pipe transport" as the named non-goal). Would need a
   new `#[cfg(windows)]` server implementation in `control.rs` (`CreateNamedPipeW`-based,
   likely via the `windows-sys` crate, since `smol`'s ecosystem doesn't have first-class
   named-pipe support the way `tokio::net::windows::named_pipe` does) and a matching
   `#[cfg(windows)] mod windows` client in `agent_control_cli.rs` alongside the existing
   `mod unix`.

### Peer identification

- If AF_UNIX works on Windows: unclear whether Windows exposes an `SO_PEERCRED`-style
  credential query over `AF_UNIX` sockets at all — this needs its own check, since
  Windows's `AF_UNIX` support is newer and less complete than Linux/macOS's. If it
  doesn't, named pipes become necessary regardless of (1) above, just for
  identification rather than transport.
- Named pipes have a direct, well-documented equivalent:
  [`GetNamedPipeClientProcessId`](https://learn.microsoft.com/windows/win32/api/namedpipeapi/nf-namedpipeapi-getnamedpipeclientprocessid),
  giving the connecting process's PID exactly like `LOCAL_PEERPID`/`SO_PEERCRED` do
  today (`control.rs:206-219`). The ancestry-walk and cwd/kind-fallback logic
  (`resolve_caller_thread`/`resolve_by_cwd`, already built on the cross-platform
  `sysinfo` crate) needs no changes at all once a peer PID is obtained — this part of
  the Unix work is directly reusable.

### Instructions text

The `find ... -exec cat ... \;` discovery command in
`instructions::WORKTREE_INSTRUCTIONS_BLOCK` doesn't run under `cmd.exe` or plain
Windows PowerShell. Before writing a Windows equivalent, confirm what shell Codex CLI
and Claude Code actually invoke tool/shell commands through on Windows (PowerShell,
`cmd.exe`, or a bundled POSIX-compatible shell) — the answer determines whether a
PowerShell-specific snippet is even the right thing to add, or whether the existing
Unix-style command already works unmodified for those CLIs' Windows shell layer.
`global_instructions_path` (`instructions.rs`) itself needs no change: `paths::home_dir()`
already resolves correctly on Windows, and `~/.codex/AGENTS.md`-style dotfile
conventions are preserved by these CLIs on Windows too (not verified for this specific
claim — check against each CLI's own docs, same as the OpenCode/Pi paths already in
`instructions.rs` were verified against their docs rather than assumed).

### Executable delivery

- Add a Windows branch to `util::get_flint_agent_control_path` (`util.rs:363-372`),
  mirroring `get_flint_cli_path`'s own existing Windows handling if it has one, or
  established via the same `current_exe()`-relative-candidate-list pattern.
- `script/bundle-windows.ps1`: build `flint-agent-control.exe` alongside the existing
  `flint.exe`/`cli.exe`/`auto_update_helper.exe` build (`bundle-windows.ps1:137`) and
  copy it into `$innoDir` the same way (`:139-141`), then confirm it's included in
  whatever signing/packaging step covers the other `.exe` files there.

## Testing

Same conventions as the Unix work: real `#[gpui::test]`s against `control.rs`'s logic
functions where possible (the ancestry/cwd/kind resolver is platform-agnostic once a
peer PID is available, so its existing Unix tests already cover most of the resolution
logic — only the peer-PID-acquisition and transport layers need Windows-specific
tests). Any Windows-only test module should be gated the same way the module it tests
is (`#[cfg(windows)]`), and `clippy_windows` should be treated as a real gate, not an
afterthought — this document exists partly because that gate wasn't being watched.

## Explicit non-goals for this plan (and this document doesn't implement any of the above)

Remote-SSH transport for either platform, per the original design doc, remains out of
scope here too. This document is planning-only per the request that produced it — no
code in this pass beyond the `#[cfg(unix)]` dead-code fix described above, which was a
correctness fix for the *existing* Unix-only design (keeping Windows building cleanly),
not Windows feature work.
