# Linux DEB and RPM Packaging Implementation Plan

## Summary

Add native Linux packages without changing Flint's compiled application or
portable tarball. A new packaging script will consume the tarball produced by
`script/bundle-linux`, map its contents into system paths, generate DEB and RPM
artifacts through a pinned `nfpm`, and validate both packages before release.

## Files

Create:

- `script/package-linux`
- `script/install-nfpm`
- `script/test-package-linux`
- `crates/flint/resources/linux/nfpm.yaml.in`

Modify:

- `.github/workflows/release.yml`
- `.github/workflows/release_nightly.yml`
- `.github/workflows/run_bundling.yml`
- `docs/src/linux.md`

Do not modify:

- `script/bundle-linux`
- `script/install.sh`
- Runtime window identity or icon code

## 1. Add a deterministic package-builder installer

Files:

- Create `script/install-nfpm`

Implement a small strict-mode shell script that:

- Pins one `nfpm` version and the SHA-256 checksums for its Linux x86_64 and
  aarch64 archives.
- Maps `uname -m` to the corresponding upstream archive name.
- Rejects unsupported architectures.
- Downloads the archive into a temporary directory.
- Verifies the selected archive checksum before extraction.
- Installs `nfpm` into a caller-specified directory, defaulting to
  `target/tools/nfpm/bin`.
- Prints the installed executable path for callers.
- Reuses an existing executable only when `nfpm --version` matches the pinned
  version.

The script must not use `sudo` or write outside the repository by default.

Validation:

```sh
script/install-nfpm
target/tools/nfpm/bin/nfpm --version
```

Expected result: the reported version equals the pinned version. Corrupting the
expected checksum in a temporary copy of the script must make installation
fail before extraction.

## 2. Define one package template for both formats

Files:

- Create `crates/flint/resources/linux/nfpm.yaml.in`

Define shared metadata and file mappings using values supplied by
`script/package-linux`:

- Package name, version, architecture, description, license, maintainer, and
  homepage.
- Conflict entries for the other Flint channel package names.
- The staged application tree copied to
  `/usr/lib/flint/$APP_DIRECTORY`.
- A symbolic link from `/usr/bin/flint` to the staged CLI launcher.
- The channel-specific desktop file copied to `/usr/share/applications`.
- The 512px and 1024px icons copied to the matching hicolor directories.

Keep format-specific configuration limited to architecture values supplied by
the caller. Do not add inferred library dependencies or mandatory maintainer
scripts.

The template must not contain unresolved channel-specific constants after
rendering.

## 3. Write the failing package integration test

Files:

- Create `script/test-package-linux`

Before implementing `script/package-linux`, add an executable strict-mode test
script that creates minimal fixture tarballs with the same directory shape as
`script/bundle-linux`. Each channel fixture should include:

- Executable dummy CLI and editor files.
- One dummy bundled library.
- The desktop entry matching that fixture's release channel.
- Valid 512px and 1024px PNG files copied from existing Flint resources.
- A license file.

Run the package command before implementing it and confirm the test fails
because `script/package-linux` does not exist. Once implementation begins, run
the package command against the stable fixture and assert:

- Both expected artifact files are created.
- DEB metadata reports package `flint` and the host's mapped architecture.
- RPM metadata reports package `flint` and the host's mapped architecture.
- Both package file lists contain the editor, CLI, library, desktop entry,
  icons, license, and `/usr/bin/flint`.
- Extracted `/usr/bin/flint` resolves to
  `/usr/lib/flint/flint.app/bin/flint`.
- The desktop file uses `/usr/bin/flint` for `TryExec` and `Exec`, and uses
  `flint` for `Icon`.

Run a second fixture case for preview and assert the package name,
application-directory suffix, desktop ID, and conflicts are channel-specific.

Add negative cases for:

- Unsupported architecture.
- Unknown release channel.
- Missing editor binary.
- Missing desktop entry.
- Missing icon.

Run the test once and confirm it fails because `script/package-linux` does not
exist.

## 4. Implement Linux package generation

Files:

- Create `script/package-linux`
- Use `crates/flint/resources/linux/nfpm.yaml.in`

Implement a strict-mode script with:

```text
Usage: script/package-linux [--archive PATH] [--output-dir PATH]
                            [--architecture ARCH] [--channel CHANNEL]
```

Defaults:

- Archive:
  `target/release/flint-linux-$(uname -m).tar.gz`
- Output directory: `target/release`
- Architecture: `uname -m`
- Release channel: `crates/flint/RELEASE_CHANNEL`
- Version: `script/get-crate-version flint`
- `nfpm`: `NFPM` when set, otherwise `target/tools/nfpm/bin/nfpm`

The script will:

1. Validate the channel and map it to package name, application directory,
   desktop ID, and conflict list.
2. Map the host architecture independently for DEB and RPM metadata.
3. Verify that the archive exists.
4. Extract it into a temporary directory with cleanup registered through
   `trap`.
5. Verify all required source files before invoking `nfpm`.
6. Copy the bundled desktop entry into package staging and rewrite only:
   - `TryExec=/usr/bin/flint`
   - `Exec=/usr/bin/flint ...`
   - Preserve desktop actions while rewriting their `Exec` values.
7. Render one temporary `nfpm` configuration per format so DEB and RPM receive
   their native architecture names while all other metadata and file mappings
   remain shared.
8. Generate:
   - `flint-linux-$(uname -m).deb`
   - `flint-linux-$(uname -m).rpm`
9. Inspect package metadata and contents, failing on any mismatch.
10. Extract each package into separate temporary roots with `dpkg-deb -x` and
    `rpm2cpio`/`cpio`, then validate the symlink, desktop entry, and PNG files.
11. Run `desktop-file-validate` when installed; otherwise emit a clear skip
    message.

Use `dpkg-deb --info` and `dpkg-deb --contents` for DEB inspection. Use
`rpm -qip` and `rpm -qlp` for RPM inspection. Treat missing inspection commands
as a packaging failure in CI; document them as local prerequisites.

Avoid silent error suppression. Optional validation must use explicit command
availability checks.

Run:

```sh
script/install-nfpm
script/test-package-linux
```

Expected result: all stable, preview, and failure-mode fixture cases pass.

## 5. Add package generation to Linux bundling workflows

Files:

- Modify `.github/workflows/release.yml`
- Modify `.github/workflows/release_nightly.yml`
- Modify `.github/workflows/run_bundling.yml`

For each Linux architecture job:

1. Install RPM, `cpio`, and desktop-file inspection tools through the existing
   Ubuntu package manager step or a focused setup step.
2. Run `script/install-nfpm`.
3. Run `script/test-package-linux` before the production package build.
4. Run `script/bundle-linux`.
5. Run `script/package-linux`.
6. Upload the DEB and RPM as separate named artifacts alongside the tarball.

Keep package generation in the architecture-native jobs so package metadata and
artifact names match the actual build architecture.

Update release aggregation to move these files into `release-artifacts/`:

```text
flint-linux-aarch64.deb
flint-linux-aarch64.rpm
flint-linux-x86_64.deb
flint-linux-x86_64.rpm
```

Update release expected-asset validation to require all four files.

Update nightly aggregation to publish the same files with `--clobber`.

In `run_bundling.yml`, upload the new artifacts so ordinary bundling CI catches
packaging failures before a release workflow runs.

Validation:

```sh
actionlint .github/workflows/release.yml
actionlint .github/workflows/release_nightly.yml
actionlint .github/workflows/run_bundling.yml
```

Also inspect workflow diffs to ensure both architectures and all three
workflows use identical package steps.

## 6. Document native package installation

Files:

- Modify `docs/src/linux.md`

In the manual-download section:

- Present DEB for Debian/Ubuntu and RPM for Fedora/RHEL-compatible systems as
  the preferred downloads.
- Give architecture-specific links matching the release artifact names.
- Show local-file installation commands:

```sh
sudo apt install ./flint-linux-x86_64.deb
sudo dnf install ./flint-linux-x86_64.rpm
```

- State that native packages install the launcher, desktop entry, MIME
  registration, and application icons.
- Retain the tarball instructions under a portable/custom-location subsection.
- Retain the existing glibc and GPU compatibility requirements.

Run the repository's documentation formatting or link checks if available.

## 7. Final verification

Run:

```sh
script/install-nfpm
script/test-package-linux
./script/shellcheck-scripts error
git diff --check
```

On a Linux x86_64 host, additionally run the full path:

```sh
script/bundle-linux
script/package-linux
dpkg-deb --info target/release/flint-linux-x86_64.deb
rpm -qip target/release/flint-linux-x86_64.rpm
```

If the current development host is not Linux, rely on the fixture test for
local validation and require the `run_bundling.yml` Linux jobs to pass before
merging.

## Acceptance Criteria

- Linux x86_64 and aarch64 release jobs produce tarball, DEB, and RPM
  application artifacts.
- Release and nightly GitHub releases publish all six Linux application
  artifacts.
- Installing a native package puts the desktop file at a filename matching
  Flint's runtime app ID.
- The installed desktop entry launches `/usr/bin/flint` and resolves the
  channel-specific Flint icon through the hicolor icon theme.
- DEB and RPM contain equivalent application files and metadata.
- Stable, preview, nightly, and dev metadata map to their existing application
  IDs and package names.
- Channel packages explicitly conflict because they provide the same
  `/usr/bin/flint`.
- The portable tarball and existing installer remain unchanged.
- Package fixture tests, shell checks, workflow lint, and package-content
  validation pass.
