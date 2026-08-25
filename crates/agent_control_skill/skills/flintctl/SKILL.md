---
name: flintctl
description: Use when creating or switching Git worktrees, creating or coordinating Flint Agent Threads, or reading, controlling, or waiting for Flint terminals. Outside a controllable Flint terminal, continue without Flint control commands.
---

<!-- flintctl-skill-version: 4 -->

# Flint control

On macOS or Linux, find the release-matched control executable:

```sh
find "$HOME/Library/Application Support/Flint" "$HOME/.local/share/flint" -maxdepth 1 -name 'agent-control-*-executable.json' -exec cat {} \; 2>/dev/null
```

The release-matched control socket is beside the marker and has the same `agent-control-<channel>` stem with a `.sock` suffix. If no matching marker or socket exists, continue the task without Flint control.

On Windows PowerShell, use the current sign-in session marker:

```powershell
$sessionId = (Get-Process -Id $PID).SessionId
$marker = Get-ChildItem -Path (Join-Path $env:LOCALAPPDATA 'Flint') -Filter "agent-control-*-$sessionId-executable.json" -File | Select-Object -First 1
$control = if ($marker) { (Get-Content -Raw $marker.FullName | ConvertFrom-Json).executable }
$pipe = Get-ChildItem -Path '\\.\pipe\' -Filter "flint-agent-control-*-$sessionId" | Select-Object -First 1
```

If the marker or named pipe is absent, continue the task without Flint control. Use the `executable` value from the marker as `<flintctl>`. Do not assume that `flintctl` is on `PATH`.

Run `"<flintctl>" terminal current --json`. A successful result permits terminal commands. Use thread commands only when the result has `is_agent_thread: true`. If the connection fails, the protocol is incompatible, or Flint reports that the caller is not recognized, continue without Flint control. A result with `is_agent_thread: false` still permits terminal commands.

After creating a worktree that this thread will own, run:

```sh
"<flintctl>" thread retie --worktree "<absolute-path>"
```

Use `"<flintctl>" thread create --help` before creating a sibling Agent Thread. Use `"<flintctl>" terminal --help` before terminal control. Keep control within the current workspace. Do not use `TERM_PROGRAM`, `ZED_TERM`, or another environment variable to decide whether Flint control is available.

Use `terminal split` when the user names a direction, asks for a split or pane, or needs to see the old and new terminals at the same time. A split creates a new rectangular region with its own shell and a draggable divider, like a tmux pane. Use exactly one of `--current` or `--terminal <terminal-id>`, and always give `--direction left`, `right`, `up`, or `down`.

Use `terminal open` for a plain terminal request without a direction, and for an ambiguous request. It creates a new shell as a background tab in the caller's existing pane. It does not create a new visible region and has no tmux equivalent. Use `--focus` only when the user asks to switch to the new terminal. Never substitute `terminal open --focus` when the user asks to see both terminals at the same time.

Use `thread create` only when the user asks for another coding agent or the work needs a delegated Agent Thread. Preserve the caller's working directory unless the user gives another directory. Do not use `--focus` unless the user asks for focus. Use `--split <direction>` only with `--worktree current`; it is invalid with `--worktree new`. Do not create a worktree or Agent Thread when a plain shell terminal is sufficient. Give `--agent` one of the known agent ids: `codex`, `claude`, `pi`, or `opencode`.

Use `terminal current --json` or `terminal list --json` to get a `<terminal-id>` before targeting a terminal with `read`, `send-text`, `send-key`, `run`, `wait-output`, or `split --terminal`. `terminal list` shows every other terminal in the caller's workspace; add `--all` to include the caller's own terminal too.

Use `terminal read <terminal-id>` to see what a terminal has produced. The default `--source recent` returns the last physical lines of output, including scrollback. Use `--source visible` for only what the terminal renders right now -- prefer this over `recent` when a repainting program (a shell prompt, a progress bar, a TUI) would otherwise show stacked fragments instead of the real content. Use `--source detection` to judge whether an agent looks idle or busy; it always returns a snapshot sized to the terminal's current row count and ignores `--lines`. After a first read, pass `--since <cursor>` with the cursor from that read's `--json` output to fetch only the output appended since, instead of rereading the same tail.

Use `terminal wait-output <terminal-id> --match "<text>"` or `--regex "<pattern>"` to block until matching output appears, instead of polling `terminal read` in a loop. The default timeout is 30 seconds; raise `--timeout` for a command expected to take longer.

Use `terminal send-text <terminal-id> "<text>"` to type text without pressing a key, `terminal send-key <terminal-id> <key>...` to send named keys (`enter`, `escape`, `ctrl-c`, `alt-left`, arrow keys, `f1`-`f12`), and `terminal run <terminal-id> "<command>"` to type a full command and press enter in one call. None of these three wait for a response; follow up with `terminal read` or `terminal wait-output` to see the result.
