# Flint

Flint is a terminal-first fork of [Zed](https://github.com/zed-industries/zed) built for developers who use tools such as Codex and Claude Code from the command line.

It keeps Zed's fast, GPU-accelerated editor, language support, Git tooling, and extension ecosystem while replacing the built-in AI product with a focused workspace for terminal-based coding agents.

---

## Why Flint?

### Added

- **First-class terminals:** New terminals open as tabs in the center workspace by default, alongside files and diffs.
- **Codex and Claude Code threads:** Launch either CLI directly in a terminal-backed thread using its existing authentication, configuration, and subscription.
- **Agent Threads panel:** Organize Codex and Claude Code sessions, reopen recent threads, and resume work using titles discovered from each agent's local history.
- **Configurable agent workflows:** Set commands, arguments, environment variables, working directories, visibility, panel location, and default resume options.
- **Faster change review:** Open project changes directly from the editor toolbar and review agent-generated work with Zed's Git and diff views.

### Removed

Flint does not ship Zed's native agent and chat interface, hosted AI models, model-provider configuration, Copilot or edit predictions, account and billing UI, or real-time collaboration and calls. The result is a smaller, local-first product surface that leaves agent behavior and credentials with the CLI tools you already use.

## Try Flint

Download the latest build for macOS, Linux, or Windows from [GitHub Releases](https://github.com/shenghsi/flint/releases/latest).

### macOS

After moving Flint into `/Applications`, remove the quarantine attribute so macOS will allow the unsigned app to open:

```sh
xattr -cr /Applications/Flint.app
```

### Linux

Install Flint into `~/.local` (no root required, and in-app auto-update works):

```sh
curl -f https://raw.githubusercontent.com/shenghsi/flint/main/script/install.sh | sh
```

If `~/.local/bin` isn't already on your `PATH`, add it so you can launch Flint with `flint`.

The `.deb` and `.rpm` packages install Flint system-wide under `/usr/lib/flint`. Those builds are managed by your package manager, so in-app auto-update is disabled — update them with `apt`, `dnf`, etc.

### Developing Flint

- [Building Flint for macOS](./docs/src/development/macos.md)
- [Building Flint for Linux](./docs/src/development/linux.md)
- [Building Flint for Windows](./docs/src/development/windows.md)

### Extensions

Flint is compatible with the [Zed extension registry](https://zed.dev/extensions). Extensions install and work without modification.

### Licensing

Flint source code is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked.

License information for third party dependencies must be correctly provided for CI to pass.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/flint-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/flint-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).

### Acknowledgements

Flint is built on top of [Zed](https://github.com/zed-industries/zed) by Zed Industries. We are grateful for their open-source contribution.
