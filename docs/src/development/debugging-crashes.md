---
title: Debugging Crashes
description: "Guide to debugging Flint crashes locally."
---

# Debugging Crashes

When Flint crashes, its sidecar crash handler writes a compressed minidump and adjacent JSON metadata to the application logs directory. See [Local Diagnostics](../telemetry.md#finding-local-files) for paths on macOS, Linux, and Windows.

Flint does not upload these files. You can inspect, share, or delete them yourself. For an SSH session, crash artifacts stay on the remote host.

The file retains its `.dmp` extension but its contents are zstd-compressed. Decompress it and inspect it with Breakpad tools:

```sh
zstd -d <session>.dmp -o minidump.dmp
minidump-stackwalk minidump.dmp
```

Useful symbolication requires matching source and symbols for the Flint build. The adjacent `<session>.json` records the version, release channel, commit, panic information, and system/GPU details needed to identify that build.

If the crash is reproducible, running a local debug build under a debugger is usually more direct. See [Using a debugger](./debuggers.md#debugging-panics-and-crashes).
