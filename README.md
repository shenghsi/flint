# Flint

Flint is a terminal-first fork of [Zed](https://github.com/zed-industries/zed), the high-performance, multiplayer code editor from the creators of [Atom](https://github.com/atom/atom) and [Tree-sitter](https://github.com/tree-sitter/tree-sitter).

Flint inherits Zed's fast, GPU-accelerated rendering and AI capabilities while shipping with terminal-first defaults and its own identity.

---

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
