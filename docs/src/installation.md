---
title: Install Flint - macOS, Linux, Windows
description: Download and install Flint on macOS, Linux, or Windows. Includes Homebrew, direct download, and package manager options.
---

# Installing Flint

## Download Flint

### macOS

Get the latest stable builds via [the download page](https://flint.dev/download). If you want to download our preview build, you can find it on its [releases page](https://flint.dev/releases/preview). After the first manual installation, Flint will periodically check for install updates.

You can also install Flint stable via Homebrew:

```sh
brew install --cask flint
```

As well as Flint preview:

```sh
brew install --cask flint@preview
```

### Windows

Get the latest stable builds via [the download page](https://flint.dev/download). If you want to download our preview build, you can find it on its [releases page](https://flint.dev/releases/preview). After the first manual installation, Flint will periodically check for install updates.

Additionally, you can install Flint using winget:

```sh
winget install -e --id FlintIndustries.Flint
```

### Linux

For most Linux users, the easiest way to install Flint is through our installation script:

```sh
curl -f https://flint.dev/install.sh | sh
```

You can now optionally specify a **version** of Flint to install using the `ZED_VERSION` environment variable:

```sh
# Install the latest stable version (default)
curl -f https://flint.dev/install.sh | sh

# Install a specific version
curl -f https://flint.dev/install.sh | ZED_VERSION=0.216.0 sh
```

To install the preview build, which receives updates about a week ahead of stable:

```sh
curl -f https://flint.dev/install.sh | ZED_CHANNEL=preview sh
```

This script supports `x86_64` and `AArch64`, as well as common Linux distributions: Ubuntu, Arch, Debian, RedHat, CentOS, Fedora, and more.

If Flint is installed using this installation script, it can be uninstalled at any time by running the shell command `flint --uninstall`. The shell will then prompt you whether you'd like to keep your preferences or delete them. After making a choice, you should see a message that Flint was successfully uninstalled.

If this script is insufficient for your use case, you run into problems running Flint, or there are errors in uninstalling Flint, please see our [Linux-specific documentation](./linux.md).

## System Requirements

### macOS

Flint supports the following macOS releases:

| Version       | Codename | Apple Status   | Flint Status          |
| ------------- | -------- | -------------- | ------------------- |
| macOS 26.x    | Tahoe    | Supported      | Supported           |
| macOS 15.x    | Sequoia  | Supported      | Supported           |
| macOS 14.x    | Sonoma   | Supported      | Supported           |
| macOS 13.x    | Ventura  | Supported      | Supported           |
| macOS 12.x    | Monterey | EOL 2024-09-16 | Supported           |
| macOS 11.x    | Big Sur  | EOL 2023-09-26 | Partially Supported |
| macOS 10.15.x | Catalina | EOL 2022-09-12 | Partially Supported |

The macOS releases labelled "Partially Supported" (Big Sur and Catalina) do not support screen sharing via Flint Collaboration. These features use the [LiveKit SDK](https://livekit.io) which relies upon [ScreenCaptureKit.framework](https://developer.apple.com/documentation/screencapturekit/) only available on macOS 12 (Monterey) and newer.

#### Mac Hardware

Flint supports machines with Intel (x86_64) or Apple (aarch64) processors that meet the above macOS requirements:

- MacBook Pro (Early 2015 and newer)
- MacBook Air (Early 2015 and newer)
- MacBook (Early 2016 and newer)
- Mac Mini (Late 2014 and newer)
- Mac Pro (Late 2013 or newer)
- iMac (Late 2015 and newer)
- iMac Pro (all models)
- Mac Studio (all models)

### Linux

Flint supports 64-bit Intel/AMD (x86_64) and 64-bit Arm (aarch64) processors.

Flint requires a Vulkan 1.3 driver and the following desktop portals:

- `org.freedesktop.portal.FileChooser`
- `org.freedesktop.portal.OpenURI`
- `org.freedesktop.portal.Secret` or `org.freedesktop.Secrets`

### Windows

Flint supports the following Windows releases:
| Version | Flint Status |
| ------------------------- | ------------------- |
| Windows 11, version 22H2 and later | Supported |
| Windows 10, version 1903 and later | Supported |

A 64-bit operating system is required to run Flint.

#### Windows Hardware

Flint supports machines with x64 (Intel, AMD) or Arm64 (Qualcomm) processors that meet the following requirements:

- Graphics: A GPU that supports DirectX 11 (most PCs from 2012+).
- Driver: Current NVIDIA/AMD/Intel/Qualcomm driver (not the Microsoft Basic Display Adapter).

### FreeBSD

Not yet available as an official download. Can be built [from source](./development/freebsd.md).

### Web

Not supported at this time. See our [Platform Support issue](https://github.com/zed-industries/flint/issues/5391).
