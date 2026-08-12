---
title: Building Flint for macOS
description: "Guide to building Flint for macOS development."
---

# Building Flint for macOS

## Repository

Clone the [Flint repository](https://github.com/shenghsi/flint).

## Dependencies

- Install [rustup](https://www.rust-lang.org/tools/install)

- Install [Xcode](https://apps.apple.com/us/app/xcode/id497799835?mt=12) from the macOS App Store or from the [Apple Developer](https://developer.apple.com/download/all/) website. The Apple Developer download requires a developer account.

> Launch Xcode after installation and install the macOS components (the default option).

- Install [Xcode command line tools](https://developer.apple.com/xcode/resources/)

  ```sh
  xcode-select --install
  ```

- Ensure that the Xcode command line tools are using your newly installed copy of Xcode:

  ```sh
  sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
  sudo xcodebuild -license accept
  ```

- Install `cmake` (required by [a dependency](https://docs.rs/wasmtime-c-api-impl/latest/wasmtime_c_api/))

  ```sh
  brew install cmake
  ```

## Building Flint from Source

Once you have the dependencies installed, you can build Flint using [Cargo](https://doc.rust-lang.org/cargo/).

For a debug build:

```sh
cargo run
```

For a release build:

```sh
cargo run --release
```

And to run the tests:

```sh
cargo test --workspace
```

## Visual Regression Tests

Flint includes visual checks that render localized UI previews with the macOS
Metal renderer. The runner saves narrow and wide screenshots for review.

### Prerequisites

The visual test runner needs macOS and Metal support. It does not need Screen
Recording permission because it uses a headless renderer.

### Running Visual Tests

```sh
cargo run -p flint --bin flint_visual_test_runner --features visual-tests
```

### Baseline Images

Screenshots are written to `target/visual_tests/` by default. Set
`VISUAL_TEST_OUTPUT_DIR` to write them elsewhere.

#### Initial Setup

Generate the localized previews:

```sh
cargo run -p flint --bin flint_visual_test_runner --features visual-tests
```

Review the `chinese-localization-narrow.png` and
`chinese-localization-wide.png` output files.

#### Updating Baselines

To write the preview images to a temporary directory:

```sh
VISUAL_TEST_OUTPUT_DIR=/tmp/flint-visual-tests \
  cargo run -p flint --bin flint_visual_test_runner --features visual-tests
```


## Troubleshooting

### Error compiling metal shaders

```sh
error: failed to run custom build command for gpui v0.1.0 (/Users/path/to/flint)`**

xcrun: error: unable to find utility "metal", not a developer tool or in PATH
```

Try `sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer`

If you're on macOS 26, try `xcodebuild -downloadComponent MetalToolchain`.
If that command fails, run `xcodebuild -runFirstLaunch` and try downloading the toolchain again.

### Cargo errors claiming that a dependency is using unstable features

Try `cargo clean` and `cargo build`.

### Error: 'dispatch/dispatch.h' file not found

If you encounter an error similar to:

```sh
src/platform/mac/dispatch.h:1:10: fatal error: 'dispatch/dispatch.h' file not found

Caused by:
  process didn't exit successfully

  --- stdout
  cargo:rustc-link-lib=framework=System
  cargo:rerun-if-changed=src/platform/mac/dispatch.h
  cargo:rerun-if-env-changed=TARGET
  cargo:rerun-if-env-changed=BINDGEN_EXTRA_CLANG_ARGS_aarch64-apple-darwin
  cargo:rerun-if-env-changed=BINDGEN_EXTRA_CLANG_ARGS_aarch64_apple_darwin
  cargo:rerun-if-env-changed=BINDGEN_EXTRA_CLANG_ARGS
```

This file is part of Xcode. Make sure the Xcode command line tools are installed and the path is set correctly:

```sh
xcode-select --install
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
```

Additionally, set the `BINDGEN_EXTRA_CLANG_ARGS` environment variable:

```sh
export BINDGEN_EXTRA_CLANG_ARGS="--sysroot=$(xcrun --show-sdk-path)"
```

Then clean and rebuild the project:

```sh
cargo clean
cargo run
```

### Tests failing due to `Too many open files (os error 24)`

This error seems to be caused by OS resource constraints. Installing and running tests with `cargo-nextest` should resolve the issue.

- `cargo install cargo-nextest --locked`
- `cargo nextest run --workspace --no-fail-fast`

## Tips & Tricks

### Avoiding continual rebuilds

If Flint continually rebuilds root crates, you may be opening the Flint codebase itself in your development build.

This causes problems because `cargo run` exports a bunch of environment
variables which are picked up by the `rust-analyzer` that runs in the development
build of Flint. These environment variables are in turn passed to `cargo check`, which
invalidates the build cache of some of the crates we depend on.

To avoid this, run the built binary against a different project, for example `cargo run ~/path/to/other/project`.

### Speeding up verification

If you build Flint frequently, macOS may keep verifying new builds, which can add a few seconds to each iteration.

To fix this, you can:

- Run `sudo spctl developer-mode enable-terminal` to enable the Developer Tools panel in System Settings.
- In System Settings, search for "Developer Tools" and add your terminal (e.g. iTerm or Ghostty) to the list under "Allow applications to use developer tools"
- Restart your terminal.

Thanks to the nextest developers for publishing [this](https://nexte.st/docs/installation/macos/#gatekeeper).
