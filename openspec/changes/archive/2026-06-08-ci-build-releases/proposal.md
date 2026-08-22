## Why

The Flint fork has comprehensive CI workflows, bundle scripts, and packaging resources already in place (renamed from Zed), but they reference infrastructure that doesn't exist for this fork: `flint-industries` GitHub org, DigitalOcean Spaces, Sentry projects, Apple developer certificates, Azure signing, WinGet publishing, etc. The user needs a working CI pipeline that builds Flint for macOS, Linux, and Windows and publishes GitHub Releases — without requiring Zed's enterprise infrastructure.

## What Changes

- Simplify `release.yml` to build all 6 platform targets and publish to GitHub Releases without Sentry, DigitalOcean, Slack, or Discord integrations
- Simplify `release_nightly.yml` to produce nightly builds and attach them to a rolling GitHub Release (no DigitalOcean Spaces)
- Update `run_tests.yml` to work with the user's actual GitHub repository
- Document required GitHub Secrets (Apple certificates for macOS, Azure signing for Windows, or provide unsigned fallbacks)
- Ensure all bundle scripts (`script/bundle-mac`, `script/bundle-linux`, `script/bundle-windows.ps1`) work without organization-specific infrastructure
- Remove or disable `after_release.yml` integrations that depend on flint.dev, Discord, Sentry, and WinGet (can be re-enabled later when those services exist)

## Capabilities

### New Capabilities

- `ci-release-pipeline`: GitHub Actions workflow that builds Flint for all platforms (macOS aarch64/x86_64, Linux aarch64/x86_64, Windows aarch64/x86_64) and publishes release artifacts to GitHub Releases

### Modified Capabilities

_None_
