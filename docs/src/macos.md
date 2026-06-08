---
title: Flint on macOS
description: "Flint is developed primarily on macOS, making it a first-class platform with full feature support."
---

# Flint on macOS

Flint is developed primarily on macOS, making it a first-class platform with full feature support.

## Installing Flint

Download Flint from the [download page](https://flint.dev/download). The download is a `.dmg` file—open it and drag Flint to your Applications folder.

For the preview build, which receives updates about a week ahead of stable, visit the [preview releases page](https://flint.dev/releases/preview).

After installation, Flint checks for updates automatically and prompts you when a new version is available.

### Homebrew

You can also install Flint using Homebrew:

```sh
brew install --cask flint
```

For the preview version:

```sh
brew install --cask flint@preview
```

### Building from Source

To build Flint from source, see the [macOS development documentation](./development/macos.md).

## System Requirements

- macOS 10.15.7 (Catalina) or later
- Apple Silicon (M1/M2/M3/M4) or Intel processor

Flint uses Metal for GPU-accelerated rendering, which is available on all supported macOS versions.

## Installing the CLI

Flint includes a command-line tool for opening files and projects from Terminal. To install it:

1. Open Flint
2. Open the command palette with `Cmd+Shift+P`
3. Run {#action cli::InstallCliBinary}

This creates a `flint` command in `/usr/local/bin`. You can then open files and folders:

```sh
flint .                    # Open current folder
flint file.txt             # Open a file
flint project/ file.txt    # Open a folder and a file
```

See the [CLI Reference](./reference/cli.md) for all available options.

## Uninstall

1. Quit Flint if it's running
2. Drag Flint from Applications to the Trash
3. Optionally, remove your settings and extensions:

```sh
rm -rf ~/.config/flint
rm -rf ~/Library/Application\ Support/Flint
rm -rf ~/Library/Caches/Flint
rm -rf ~/Library/Logs/Flint
rm -rf ~/Library/Saved\ Application\ State/dev.flint.Flint.savedState
```

If you installed the CLI, remove it with:

```sh
rm /usr/local/bin/flint
```

## Troubleshooting

### Flint won't open or shows "damaged" warning

If macOS reports that Flint is damaged or can't be opened, it's likely a Gatekeeper issue. Try:

1. Right-click (or Control-click) on Flint in Applications
2. Select "Open" from the context menu
3. Click "Open" in the dialog that appears

This tells macOS to trust the application.

If that doesn't work, remove the quarantine attribute:

```sh
xattr -cr /Applications/Flint.app
```

### CLI command not found

If the `flint` command isn't available after installation:

1. Check that `/usr/local/bin` is in your PATH
2. Try reinstalling the CLI via {#action cli::InstallCliBinary} in the command palette
3. Open a new terminal window to reload your PATH

### GPU or rendering issues

Flint uses Metal for rendering. If you experience graphical glitches:

1. Ensure macOS is up to date
2. Restart your Mac to reset the GPU state
3. Check Activity Monitor for GPU pressure from other apps

### High memory or CPU usage

If Flint uses more resources than expected:

1. Check for runaway language servers in the terminal output ({#action flint::OpenLog})
2. Try disabling extensions one by one to identify conflicts
3. For large projects, consider using [project settings](./reference/all-settings.md#file-scan-exclusions) to exclude unnecessary folders from indexing

For additional help, see the [Troubleshooting guide](./troubleshooting.md) or visit the [Flint Discord](https://discord.gg/flint-community).
