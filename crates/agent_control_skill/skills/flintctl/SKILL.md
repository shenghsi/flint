---
name: flintctl
description: Use when creating or switching Git worktrees, creating or coordinating Flint Agent Threads, or reading, controlling, or waiting for Flint terminals. Outside a controllable Flint terminal, continue without Flint control commands.
---

<!-- flintctl-skill-version: 2 -->

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
