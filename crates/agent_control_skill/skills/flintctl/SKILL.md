---
name: flintctl
description: Use when creating or switching Git worktrees, creating or coordinating Flint Agent Threads, or reading, controlling, or waiting for Flint terminals. Outside a Flint Agent Thread, continue without Flint control commands.
---

<!-- flintctl-skill-version: 1 -->

# Flint control

First check whether `FLINT_AGENT_THREAD=1`. If it is not, do not use `flintctl`; continue the task normally.

Inside a Flint Agent Thread on macOS or Linux, find the release-matched control executable:

```sh
find "$HOME/Library/Application Support/Flint" "$HOME/.local/share/flint" -maxdepth 1 -name 'agent-control-*-executable.json' -exec cat {} \; 2>/dev/null
```

On Windows PowerShell, use the current sign-in session marker:

```powershell
$sessionId = (Get-Process -Id $PID).SessionId
$marker = Get-ChildItem -Path (Join-Path $env:LOCALAPPDATA 'Flint') -Filter "agent-control-*-$sessionId-executable.json" -File | Select-Object -First 1
$control = if ($marker) { (Get-Content -Raw $marker.FullName | ConvertFrom-Json).executable }
```

Use the `executable` value from the marker as `<flintctl>`. Do not assume that `flintctl` is on `PATH`.

After creating a worktree that this thread will own, run:

```sh
"<flintctl>" thread retie --worktree "<absolute-path>"
```

Use `"<flintctl>" thread create --help` before creating a sibling Agent Thread. Use `"<flintctl>" terminal --help` before terminal control. Keep control within the current workspace. If no marker exists or Flint rejects the caller, continue without Flint control.
