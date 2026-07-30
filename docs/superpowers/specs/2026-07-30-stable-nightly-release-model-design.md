# Stable and Nightly Release Model

## Goal

Return Flint to a two-channel distribution model sourced from `main`:

- Stable releases are tagged and built directly from `main`.
- Nightly builds use the latest commit on `main`.
- Preview releases and the separate `stable` release branch are no longer used.

## Release behavior

`crates/flint/RELEASE_CHANNEL` on `main` is `stable`. Stable releases use plain
`vX.Y.Z` tags whose tagged commit is on `main`. The release workflow rejects
preview tags ending in `-pre` and does not auto-publish preview releases.

The existing `nightly` tag and GitHub release remain moving references. Before
compiling, every nightly build job changes its checkout's release-channel file
to `nightly`, ensuring that artifacts built from a stable-channel `main` commit
identify themselves as Flint Nightly. The nightly workflow moves the `nightly`
tag to the latest `main` commit and replaces the assets on the existing
`nightly` release.

The nightly workflow runs daily at 03:00 Asia/Taipei, which is 19:00 UTC on the
previous calendar day. It skips rebuilding when the `nightly` tag already
points to the current `main` commit.

Installed Nightly builds automatically check the moving `nightly` release for
updates every six hours. Stable builds retain the existing daily update check
against the latest non-prerelease GitHub release.

## Preview retirement

Preview is disabled as a distributed channel:

- Release automation no longer accepts or publishes `vX.Y.Z-pre` releases.
- Installer and release documentation no longer offer Preview builds.
- Release scripts describe `main` as the stable release branch.

The internal `ReleaseChannel::Preview` variant remains temporarily for
compatibility with existing settings, identifiers, and stored user state. No
Preview artifacts are produced. Removing the variant and its compatibility
surface is outside this change.

The remote `stable` branch is not deleted by this repository change. It becomes
an inactive historical branch and can be removed separately after the new
workflow has produced verified Stable and Nightly artifacts.

## Verification

Automated coverage verifies:

- plain stable tags are accepted for commits configured as `stable`;
- `-pre` tags are rejected;
- nightly artifacts are compiled with the `nightly` channel;
- Nightly's update polling interval is six hours;
- Preview is no longer selectable through Flint's installer.

Workflow and shell syntax checks, focused Rust tests, formatting, and scoped
clippy run before the pull request is opened.

