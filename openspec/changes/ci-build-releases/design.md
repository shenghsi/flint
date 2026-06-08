## Context

The Flint fork inherited Zed's entire CI/CD pipeline, which is deeply integrated with enterprise infrastructure: Namespace Labs runners, DigitalOcean Spaces for artifact hosting, Cloudflare R2 for sccache, Sentry for debug symbols, Azure Trusted Signing for Windows, Apple notarization for macOS, and Slack/Discord/WinGet for post-release distribution. All workflow YAML files are generated from Rust source in `tooling/xtask/src/tasks/workflows/`.

None of this infrastructure exists for the Flint fork. The workflows currently gate on `github.repository_owner == 'flint-industries'`, meaning they won't trigger for a fork on a personal account. The bundle scripts hardcode Sentry uploads, the nightly pipeline uploads to DigitalOcean Spaces, and the release pipeline uses Namespace Labs custom runners.

The goal is a working release pipeline using only standard GitHub-hosted runners and GitHub Releases for artifact distribution — no external services required.

## Goals / Non-Goals

**Goals:**
- Working GitHub Actions release pipeline that builds Flint for all 6 targets (macOS aarch64/x86_64, Linux aarch64/x86_64, Windows aarch64/x86_64)
- Publish release artifacts to GitHub Releases
- Nightly builds published to a rolling GitHub Release
- Bundle scripts work without Sentry, DigitalOcean, or organization-specific services
- Optional code signing (graceful degradation when secrets aren't configured)

**Non-Goals:**
- Replicating Zed's full CI test matrix (that stays in `run_tests.yml` which can be simplified separately)
- DigitalOcean Spaces, Cloudflare R2, or Cachix integration
- Sentry debug symbol uploads or crash reporting
- Slack/Discord notifications
- WinGet, flint.dev, or Vercel deployments
- Namespace Labs custom runners
- Nix builds
- sccache with R2 backend (will use GitHub Actions cache instead)

## Decisions

### 1. Write workflows directly in YAML, not via xtask generator

The existing workflow YAML files are generated from Rust source in `tooling/xtask/src/tasks/workflows/`. This adds complexity without benefit for a fork. The new release and nightly workflows will be hand-written YAML files.

**Rationale:** Simpler to maintain, no Rust build dependency for CI changes, easier for contributors to understand.
**Alternative:** Keep xtask generation — rejected because it requires understanding the xtask system and adds indirection.

### 2. Use standard GitHub-hosted runners

Replace Namespace Labs custom runners (`namespace-profile-*`) and self-hosted Windows runners with standard GitHub-hosted runners:

| Target | Runner | Notes |
|--------|--------|-------|
| macOS aarch64 | `macos-15` | M-series, Apple Silicon |
| macOS x86_64 | `macos-13` | Last Intel macOS runner |
| Linux aarch64 | `ubuntu-24.04-arm` | ARM64 GitHub runner |
| Linux x86_64 | `ubuntu-24.04` | Standard Linux |
| Windows x86_64 | `windows-2022` | Standard Windows |
| Windows aarch64 | `windows-11-arm` | ARM64 Windows runner |

**Rationale:** Zero infrastructure cost, no self-hosted runner management, works for any fork.
**Alternative:** Self-hosted runners — rejected due to infrastructure burden.

### 3. Use GitHub Actions cache for sccache

Replace Cloudflare R2-backed sccache with GitHub Actions cache backend (`actions/cache` with sccache's `gha` storage).

**Rationale:** Built into GitHub Actions, no external service, free for public repos.
**Alternative:** No caching — too slow for Rust compilation. Keep R2 — requires Cloudflare account.

### 4. Sentry uploads become optional (no-op when token absent)

Bundle scripts already handle missing Sentry tokens gracefully on some platforms. Make this consistent: if `SENTRY_AUTH_TOKEN` is not set, skip Sentry uploads silently.

**Rationale:** No Sentry project exists for this fork. Debug symbols can be distributed as artifacts instead.

### 5. Code signing is optional with graceful degradation

- **macOS:** If Apple certificate secrets aren't configured, fall back to ad-hoc signing (already implemented in `bundle-mac`).
- **Windows:** If Azure signing secrets aren't configured, skip code signing entirely and produce unsigned binaries.
- **Linux:** No code signing needed (already unsigned).

**Rationale:** Users who want signed builds can configure secrets; unsigned builds still work for testing and personal use.

### 6. Nightly builds use a rolling GitHub Release

Create a `nightly` tag and release that gets updated each night with the latest build artifacts. No DigitalOcean Spaces needed.

**Rationale:** All artifacts in one place (GitHub Releases), no external storage service.
**Alternative:** GitHub Packages — more complex, not as user-friendly for downloads.

### 7. Remove repository owner gates

Remove the `github.repository_owner == 'flint-industries'` conditions so the workflows run on the fork.

**Rationale:** Workflows must trigger on the actual repository to be useful.

## Risks / Trade-offs

- **[Slower builds on standard runners]** → GitHub-hosted runners have less RAM/CPU than Namespace Labs large runners. Rust compilation will be slower. Mitigation: sccache with GitHub Actions cache helps on subsequent builds.
- **[Unsigned Windows binaries may trigger SmartScreen]** → Users will see "unknown publisher" warnings. Mitigation: Document how to configure Azure Trusted Signing for users who need it.
- **[No macOS notarization without Apple Developer account]** → Users must right-click → Open on first launch. Mitigation: Ad-hoc signing still works; document the workaround.
- **[Linux ARM runner availability]** → `ubuntu-24.04-arm` is relatively new. Mitigation: Fall back to cross-compilation on x86_64 if ARM runners are unavailable.
- **[GitHub Actions cache size limits]** → 10GB per repo, which may be tight for Rust compilation caches across 6 targets. Mitigation: Use per-target cache scopes and prune old caches.
- **[No crash reporting]** → Without Sentry, crash investigation is harder. Mitigation: Debug symbols are still available as release artifacts; users can report issues with stack traces.
