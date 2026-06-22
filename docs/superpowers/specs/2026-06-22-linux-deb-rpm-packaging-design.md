# Linux DEB and RPM Packaging Design

## Context

Flint's Linux release is currently a portable tarball containing the editor,
CLI launcher, bundled shared libraries, desktop entry, and application icons.
The tarball works when installed through `script/install.sh`, but users who run
the extracted binary directly do not get complete desktop integration on
Wayland. Wayland compositors resolve the window's application ID through an
installed desktop entry, so the dock falls back to a generic icon when
`dev.flint.Flint.desktop` and its icon are not registered in the XDG system
directories.

Flint will continue to publish the portable tarball and will additionally
publish native DEB and RPM packages for x86_64 and aarch64 Linux systems.

## Goals

- Publish DEB and RPM packages for every Linux release architecture.
- Install the desktop entry and icons where Linux desktop environments discover
  them automatically.
- Reuse the files already staged by `script/bundle-linux`.
- Keep one package definition for DEB and RPM.
- Preserve the existing portable tarball and install script.
- Build packages in both release and nightly workflows.
- Fail CI when a package is missing required application or desktop-integration
  files.

## Non-Goals

- Host an APT or DNF/YUM repository.
- Sign DEB or RPM packages.
- Replace distribution-maintained Flint packages.
- Remove or change the portable tarball.
- Add post-install network access or automatic repository configuration.
- Support Linux architectures other than x86_64 and aarch64.

## Package Builder

Use a pinned release of `nfpm` to generate both formats from one configuration.
The CI workflow will install the pinned binary and verify its published
checksum before running it. Local packaging will use the same helper script and
configuration.

`nfpm` is preferred over separate `dpkg-deb` and `rpmbuild` implementations
because the package metadata and file mapping stay identical across formats.
Using `cargo-deb` and `cargo-generate-rpm` would also duplicate metadata and
would not naturally consume the complete staged Linux bundle.

## Build Flow

`script/bundle-linux` remains responsible for compiling Flint and staging the
complete application tree. Packaging is a separate step so the tarball build
does not require `nfpm`.

The Linux release jobs will run:

1. `script/bundle-linux`
2. A Linux package script that extracts the generated tarball into a temporary
   staging directory.
3. `nfpm package --packager deb`
4. `nfpm package --packager rpm`
5. Package-content validation.

The package script will derive the version and release channel from the same
repository sources used by `script/bundle-linux`. It will reject unknown
channels and unsupported architectures rather than producing ambiguously named
packages.

## Installed Layout

The packages will preserve the bundle's internal layout under a system-owned
application directory:

```text
/usr/lib/flint/flint.app/bin/flint
/usr/lib/flint/flint.app/libexec/flint-editor
/usr/lib/flint/flint.app/lib/*
/usr/lib/flint/flint.app/licenses.md
/usr/bin/flint
/usr/share/applications/dev.flint.Flint.desktop
/usr/share/icons/hicolor/512x512/apps/flint.png
/usr/share/icons/hicolor/1024x1024/apps/flint.png
```

Non-stable channels will use their existing channel-specific application
directory and desktop ID:

| Channel | Package name | Application directory | Desktop ID |
| --- | --- | --- | --- |
| stable | `flint` | `flint.app` | `dev.flint.Flint` |
| preview | `flint-preview` | `flint-preview.app` | `dev.flint.Flint-Preview` |
| nightly | `flint-nightly` | `flint-nightly.app` | `dev.flint.Flint-Nightly` |
| dev | `flint-dev` | `flint-dev.app` | `dev.flint.Flint-Dev` |

`/usr/bin/flint` will be a package-owned symbolic link to the CLI launcher in
the application directory. Channel packages will conflict with one another
because they intentionally provide the same command name. This matches the
current installer behavior and avoids inventing new channel-specific CLI
commands.

The packaged desktop entry will use absolute paths:

```ini
TryExec=/usr/bin/flint
Exec=/usr/bin/flint %U
Icon=flint
```

The desktop filename will match the runtime Wayland app ID and X11 `WM_CLASS`.
The icon theme lookup name remains `flint`, with the channel-specific artwork
selected during staging.

## Package Metadata

Metadata will come from the existing Flint crate and release channel:

- Version: `script/get-crate-version flint`
- License: `GPL-3.0-or-later`
- Description: Flint's existing package description
- Maintainer: Flint Team
- Homepage: the Flint repository

Architecture mapping will be explicit:

| Build architecture | DEB architecture | RPM architecture |
| --- | --- | --- |
| `x86_64` | `amd64` | `x86_64` |
| `aarch64` | `arm64` | `aarch64` |

The packages will not declare broad shared-library dependencies inferred from
the build host. Flint already bundles the non-system libraries required by the
portable release, while baseline system requirements remain documented in the
Linux installation guide.

## Release Artifacts

Each Linux build job will upload three application artifacts for its
architecture:

```text
flint-linux-x86_64.tar.gz
flint-linux-x86_64.deb
flint-linux-x86_64.rpm
```

or:

```text
flint-linux-aarch64.tar.gz
flint-linux-aarch64.deb
flint-linux-aarch64.rpm
```

Release and nightly aggregation jobs will download and publish all six Linux
application artifacts. Their expected-asset validation will include the four
new package files.

The filenames intentionally retain Flint's existing architecture vocabulary,
even though the package metadata uses each format's native architecture name.

## Package Lifecycle

No maintainer script is required for launching Flint. If package validation on
supported runners confirms the commands are available, post-install and
post-remove hooks may refresh the desktop and icon caches using
`update-desktop-database` and `gtk-update-icon-cache`. Missing cache utilities
must not make installation or removal fail because desktop environments can
discover the files without those cache refreshes.

Upgrading a package replaces the versioned contents at the same paths. Removing
it removes only package-owned files and links.

## Validation

The packaging script will inspect both generated formats before upload.

For DEB:

- `dpkg-deb --info` reports the expected package name, version, and architecture.
- `dpkg-deb --contents` contains the editor, CLI launcher, bundled libraries,
  desktop entry, both icons, license, and `/usr/bin/flint` link.

For RPM:

- `rpm -qip` reports the expected package name, version, and architecture.
- `rpm -qlp` contains the same required paths.

Shared validation will also:

- Parse the packaged desktop entry with `desktop-file-validate` when available.
- Assert that its filename equals the runtime application ID plus `.desktop`.
- Assert that `Exec`, `TryExec`, and `Icon` reference installed package paths or
  icon names.
- Extract each package into a temporary root and verify that `/usr/bin/flint`
  resolves to the packaged CLI launcher.
- Verify the PNG files are non-empty and recognized as PNG images.

CI will test package contents rather than installing into the GitHub runner's
root filesystem. This keeps validation deterministic and avoids mutating the
runner beyond installing packaging tools.

## Error Handling

Packaging failures are release failures. The package script will use strict
shell error handling and produce actionable errors for:

- Missing tarball or staged application files.
- Unsupported channel or architecture.
- Missing or checksum-mismatched `nfpm`.
- Invalid desktop metadata.
- Missing package contents.
- Failure to inspect either generated package.

The script will remove its temporary staging directory on exit. Existing
tarball generation remains usable when `nfpm` is not installed because native
package generation is invoked separately.

## Documentation

The Linux installation guide will list DEB and RPM downloads as the preferred
manual installation for supported distributions. It will retain tarball
instructions for portable and custom-prefix installs and explain that native
packages provide automatic dock, launcher, MIME, and icon registration.

Example installation commands:

```sh
sudo apt install ./flint-linux-x86_64.deb
sudo dnf install ./flint-linux-x86_64.rpm
```

## Risks and Mitigations

- **Package metadata diverges from the tarball:** Generate packages only from
  the tarball produced in the same job and validate required paths.
- **DEB and RPM architecture names differ:** Keep an explicit mapping in the
  package script and test metadata for both architectures.
- **A packaging-tool release changes output:** Pin `nfpm` and verify its
  checksum.
- **Desktop environments cache stale metadata:** Install files in standard XDG
  paths and use non-fatal cache refresh hooks only when supported.
- **Channel packages overwrite the same CLI command:** Declare package
  conflicts explicitly; this preserves current single-channel command
  semantics.
- **Package installation expands the supported Linux compatibility promise:**
  Document that DEB/RPM packaging changes installation integration, not Flint's
  existing glibc and GPU requirements.

