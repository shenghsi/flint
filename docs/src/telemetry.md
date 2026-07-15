---
title: Local Diagnostics
description: "Crash, hang, and latency diagnostics retained on your computer."
---

# Local Diagnostics

Flint does not send usage telemetry, crash reports, hang reports, or diagnostic artifacts to Flint, Zed, Sentry, or another service.

For self-debugging, Flint keeps a small set of artifacts on the machine where the issue occurred:

- `Flint.log` contains application logs.
- A crash creates a compressed `<session>.dmp` minidump and adjacent `<session>.json` metadata file.
- Hang detection keeps at most three recent `hang-*.miniprof.json` traces.
- The input-latency diagnostic command renders its report locally.

These files remain local until you inspect, share, or delete them. SSH remote-server crashes remain on the remote host; Flint does not collect them automatically.

## Finding Local Files

Run {#action flint::OpenLog} from the command palette to open the recent log, or {#action flint::RevealLogInFileManager} to reveal the full log directory.

Default log directories are:

- macOS: `~/Library/Logs/Flint/`
- Linux: `$XDG_DATA_HOME/flint/logs/`, or `~/.local/share/flint/logs/` when `XDG_DATA_HOME` is unset
- Windows: `%LOCALAPPDATA%\Flint\logs\`

Hang traces are stored in the `hang_traces` directory under Flint's data directory. SSH remote-server logs and reliability artifacts use the corresponding Flint data directory on the remote host.

## Inspecting a Crash

The `.dmp` file is zstd-compressed despite retaining its `.dmp` extension. Decompress it before using standard minidump tools:

```sh
zstd -d <session>.dmp -o minidump.dmp
minidump-stackwalk minidump.dmp
```

The adjacent `<session>.json` contains build and system metadata. Full native symbolication requires symbols matching the recorded Flint build and commit.

See [Debugging Crashes](./development/debugging-crashes.md) for local analysis guidance.
