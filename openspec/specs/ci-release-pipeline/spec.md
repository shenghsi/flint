## ADDED Requirements

### Requirement: Release workflow triggers on version tags
The system SHALL provide a GitHub Actions workflow (`release.yml`) that triggers on pushes of tags matching `v*`. The workflow SHALL build Flint for all 6 platform targets and publish artifacts to a GitHub Release.

#### Scenario: Push a stable version tag
- **WHEN** a tag matching `v*` (not ending in `-pre`) is pushed
- **THEN** the workflow creates a draft GitHub Release, builds all 6 platform targets, uploads artifacts, and publishes the release

#### Scenario: Push a preview version tag
- **WHEN** a tag matching `v*` and ending in `-pre` is pushed
- **THEN** the workflow creates a draft GitHub Release, builds all 6 platform targets, uploads artifacts, and publishes the release as a pre-release

### Requirement: Release workflow builds macOS targets
The system SHALL build macOS binaries for both `aarch64-apple-darwin` and `x86_64-apple-darwin` targets on standard GitHub-hosted macOS runners. The build SHALL produce a `.dmg` file for each architecture.

#### Scenario: Build macOS ARM64 release
- **WHEN** the release workflow runs
- **THEN** a job on a macOS runner builds `Flint-aarch64.dmg` using `script/bundle-mac aarch64-apple-darwin`

#### Scenario: Build macOS x86_64 release
- **WHEN** the release workflow runs
- **THEN** a job on a macOS runner builds `Flint-x86_64.dmg` using `script/bundle-mac x86_64-apple-darwin`

### Requirement: Release workflow builds Linux targets
The system SHALL build Linux binaries for both `aarch64-unknown-linux-gnu` and `x86_64-unknown-linux-gnu` targets. The build SHALL produce a `.tar.gz` archive for each architecture.

#### Scenario: Build Linux ARM64 release
- **WHEN** the release workflow runs
- **THEN** a job on a Linux runner builds `flint-linux-aarch64.tar.gz` using `script/bundle-linux`

#### Scenario: Build Linux x86_64 release
- **WHEN** the release workflow runs
- **THEN** a job on a Linux runner builds `flint-linux-x86_64.tar.gz` using `script/bundle-linux`

### Requirement: Release workflow builds Windows targets
The system SHALL build Windows binaries for both `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` targets. The build SHALL produce an `.exe` installer for each architecture.

#### Scenario: Build Windows x86_64 release
- **WHEN** the release workflow runs
- **THEN** a job on a Windows runner builds `Flint-x86_64.exe` using `script/bundle-windows.ps1 -Architecture x86_64`

#### Scenario: Build Windows ARM64 release
- **WHEN** the release workflow runs
- **THEN** a job on a Windows runner builds `Flint-aarch64.exe` using `script/bundle-windows.ps1 -Architecture aarch64`

### Requirement: Release workflow produces remote server binaries
The system SHALL produce compressed remote server binaries for all platforms alongside the main application bundles. This includes `flint-remote-server-macos-{arch}.gz`, `flint-remote-server-linux-{arch}.tar.gz`, and `flint-remote-server-windows-{arch}.zip` for each architecture.

#### Scenario: Remote server artifacts are uploaded
- **WHEN** the release workflow completes bundle jobs
- **THEN** all remote server artifacts are uploaded to the GitHub Release alongside the main application bundles

### Requirement: Release workflow publishes to GitHub Releases
The system SHALL upload all built artifacts to a GitHub Release associated with the triggering tag. The release SHALL include all 12 expected artifacts.

#### Scenario: All artifacts uploaded to release
- **WHEN** all bundle jobs complete successfully
- **THEN** a job downloads all artifacts and uploads them to the GitHub Release using `gh release upload`

#### Scenario: Artifact count validation
- **WHEN** artifacts are uploaded to the release
- **THEN** the workflow validates that all 12 expected artifacts are present before publishing

### Requirement: Nightly workflow produces rolling builds
The system SHALL provide a GitHub Actions workflow (`release_nightly.yml`) that runs on a schedule and/or manual trigger, builds all 6 platform targets, and updates a rolling `nightly` GitHub Release with the latest artifacts.

#### Scenario: Scheduled nightly build
- **WHEN** the nightly schedule triggers (once daily)
- **THEN** the workflow builds all 6 platform targets and replaces artifacts on the `nightly` GitHub Release

#### Scenario: Manual nightly trigger
- **WHEN** the workflow is manually triggered via `workflow_dispatch`
- **THEN** the workflow builds all 6 platform targets and replaces artifacts on the `nightly` GitHub Release

### Requirement: Bundle scripts handle missing external services gracefully
The bundle scripts (`script/bundle-mac`, `script/bundle-linux`, `script/bundle-windows.ps1`) SHALL not fail when external services (Sentry, DigitalOcean Spaces, Apple notarization, Azure signing) are not configured. Missing secrets SHALL result in skipping those steps, not failing the build.

#### Scenario: Build without Sentry token
- **WHEN** `SENTRY_AUTH_TOKEN` is not set
- **THEN** debug symbol upload steps are skipped and the build succeeds

#### Scenario: Build without Apple signing secrets
- **WHEN** Apple certificate secrets (`MACOS_CERTIFICATE`, `MACOS_CERTIFICATE_PASSWORD`, `APPLE_NOTARIZATION_KEY`, `APPLE_NOTARIZATION_KEY_ID`, `APPLE_NOTARIZATION_ISSUER_ID`) are not all present
- **THEN** the macOS bundle falls back to ad-hoc signing and produces an unsigned DMG

#### Scenario: Build without Azure signing secrets
- **WHEN** Azure signing secrets (`AZURE_SIGNING_TENANT_ID`, `AZURE_SIGNING_CLIENT_ID`, `AZURE_SIGNING_CLIENT_SECRET`) are not all present
- **THEN** the Windows bundle skips code signing and produces an unsigned installer

### Requirement: Release workflow uses standard GitHub-hosted runners
The release workflow SHALL use standard GitHub-hosted runners only. No custom or self-hosted runners SHALL be required.

#### Scenario: All builds use standard runners
- **WHEN** the release workflow runs
- **THEN** all jobs use standard GitHub-hosted runner types (`macos-15`, `macos-13`, `ubuntu-24.04`, `ubuntu-24.04-arm`, `windows-2022`, `windows-11-arm`)

### Requirement: Build caching uses GitHub Actions cache
The system SHALL use GitHub Actions cache as the sccache storage backend instead of Cloudflare R2. The cache SHALL be scoped per target platform to stay within GitHub's size limits.

#### Scenario: Subsequent builds use cache
- **WHEN** a release build runs after a previous build has populated the cache
- **THEN** sccache hits cached Rust compilation artifacts and build time is reduced

### Requirement: Workflows run without repository owner restrictions
The release and nightly workflows SHALL NOT contain `github.repository_owner` conditions. They SHALL run on any repository they are defined in.

#### Scenario: Workflow triggers on fork
- **WHEN** a version tag is pushed on any repository with these workflows
- **THEN** the release workflow triggers and runs without being gated by repository owner checks
