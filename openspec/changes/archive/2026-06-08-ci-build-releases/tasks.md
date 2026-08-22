## 1. Make Bundle Scripts Resilient to Missing Services

- [x] 1.1 Update `script/bundle-mac` to skip Sentry debug symbol uploads when `SENTRY_AUTH_TOKEN` is not set (instead of failing)
- [x] 1.2 Update `script/bundle-linux` to skip Sentry debug symbol uploads when `SENTRY_AUTH_TOKEN` is not set
- [x] 1.3 Update `script/bundle-windows.ps1` to skip Sentry debug symbol uploads when `SENTRY_AUTH_TOKEN` is not set
- [x] 1.4 Verify `script/bundle-mac` already handles missing Apple signing secrets gracefully (ad-hoc signing fallback)
- [x] 1.5 Update `script/bundle-windows.ps1` to skip Azure code signing when `AZURE_SIGNING_TENANT_ID` is not set (produce unsigned installer)

## 2. Update sccache Setup for GitHub Actions Cache

- [x] 2.1 Update `script/setup-sccache` to support GitHub Actions cache backend (`gha`) as an alternative to Cloudflare R2, using `ACTIONS_CACHE_URL` and `ACTIONS_RUNTIME_TOKEN` env vars
- [x] 2.2 Update `script/setup-sccache.ps1` with the same GitHub Actions cache support

## 3. Create Release Workflow

- [x] 3.1 Create `.github/workflows/release.yml` that triggers on `v*` tags with no `repository_owner` gate
- [x] 3.2 Add test jobs: run tests on macOS (`macos-15`), Linux (`ubuntu-24.04`), and Windows (`windows-2022`)
- [x] 3.3 Add clippy jobs for all three platforms
- [x] 3.4 Add draft release creation job that uses `gh release create --draft`
- [x] 3.5 Add bundle job for macOS aarch64 on `macos-15` runner producing `Flint-aarch64.dmg`
- [x] 3.6 Add bundle job for macOS x86_64 on `macos-13` runner producing `Flint-x86_64.dmg`
- [x] 3.7 Add bundle job for Linux aarch64 on `ubuntu-24.04-arm` runner producing `flint-linux-aarch64.tar.gz`
- [x] 3.8 Add bundle job for Linux x86_64 on `ubuntu-24.04` runner producing `flint-linux-x86_64.tar.gz`
- [x] 3.9 Add bundle job for Windows x86_64 on `windows-2022` runner producing `Flint-x86_64.exe`
- [x] 3.10 Add bundle job for Windows aarch64 on `windows-11-arm` runner producing `Flint-aarch64.exe`
- [x] 3.11 Add upload job that downloads all 12 artifacts and uploads to the GitHub Release via `gh release upload`
- [x] 3.12 Add validation step that checks all 12 expected artifacts are present in the release
- [x] 3.13 Add auto-publish step for preview tags (tags ending in `-pre` are marked as pre-release)

## 4. Create Nightly Release Workflow

- [x] 4.1 Create `.github/workflows/release_nightly.yml` that triggers on schedule (once daily) and `workflow_dispatch`
- [x] 4.2 Add step to update the `nightly` git tag to the current commit
- [x] 4.3 Add bundle jobs for all 6 platform targets (same runners as release workflow)
- [x] 4.4 Add step to ensure a `nightly` GitHub Release exists (create if missing)
- [x] 4.5 Add upload step that replaces all artifacts on the rolling `nightly` release

## 5. Update Supporting Scripts

- [x] 5.1 Update `script/determine-release-channel` to work without `flint-industries` specific configuration
- [x] 5.2 Verify `script/generate-licenses` and `script/generate-licenses.ps1` work without external dependencies
- [x] 5.3 Verify `script/create-draft-release` works with the current repository (not hardcoded to `flint-industries/flint`)
