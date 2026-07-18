use remote::RemotePlatform;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentArtifactFormat {
    Raw,
    TarGz { executable_path: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSourceVerification {
    Sha256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentVersionMatcher {
    Codex { version: &'static str },
    Claude { version: &'static str },
}

impl AgentVersionMatcher {
    pub fn matches(self, output: &str) -> bool {
        let mut fields = output.split_whitespace();
        match self {
            Self::Codex { version } => {
                fields.next().is_some_and(|agent| {
                    agent.eq_ignore_ascii_case("codex") || agent.eq_ignore_ascii_case("codex-cli")
                }) && fields.next() == Some(version)
            }
            Self::Claude { version } => {
                let fields = output.split_whitespace().collect::<Vec<_>>();
                matches!(
                    fields.as_slice(),
                    [found_version, "(Claude", "Code)"] if *found_version == version
                ) || matches!(
                    fields.as_slice(),
                    ["Claude", "Code", found_version] if *found_version == version
                )
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentRelease {
    pub version: &'static str,
    pub target: RemotePlatform,
    pub source_url: &'static str,
    pub source_sha256: &'static str,
    pub source_verification: AgentSourceVerification,
    pub executable_sha256: &'static str,
    pub artifact: AgentArtifactFormat,
    pub executable_name: &'static str,
    pub version_matcher: AgentVersionMatcher,
    pub self_update_environment: &'static [(&'static str, &'static str)],
}

pub struct AgentReleaseCatalog<'a> {
    agent_id: &'static str,
    official_source_prefixes: &'static [&'static str],
    releases: &'a [AgentRelease],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentSelfUpdatePolicy {
    pub environment: &'static [(&'static str, &'static str)],
    pub arguments: &'static [&'static str],
}

impl<'a> AgentReleaseCatalog<'a> {
    pub fn new(
        agent_id: &'static str,
        official_source_prefixes: &'static [&'static str],
        releases: &'a [AgentRelease],
    ) -> Self {
        Self {
            agent_id,
            official_source_prefixes,
            releases,
        }
    }

    pub fn release_for(&self, target: RemotePlatform) -> Option<&'a AgentRelease> {
        self.releases
            .iter()
            .find(|release| release.target == target)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let mut releases = HashSet::new();
        for release in self.releases {
            if release.self_update_environment.is_empty() {
                anyhow::bail!(
                    "{} {} has no self-update suppression environment",
                    self.agent_id,
                    release.version
                );
            }
            if !is_sha256(release.source_sha256) {
                anyhow::bail!(
                    "{} {} has an invalid source SHA-256",
                    self.agent_id,
                    release.version
                );
            }
            if !is_sha256(release.executable_sha256) {
                anyhow::bail!(
                    "{} {} has an invalid executable SHA-256",
                    self.agent_id,
                    release.version
                );
            }
            if let Some(path) = release.artifact.executable_path()
                && !is_safe_archive_path(path, release.executable_name)
            {
                anyhow::bail!(
                    "{} {} has an invalid archive executable path",
                    self.agent_id,
                    release.version
                );
            }
            if !source_is_official(release.source_url, self.official_source_prefixes) {
                anyhow::bail!(
                    "{} {} does not use an official HTTPS source",
                    self.agent_id,
                    release.version
                );
            }
            if !releases.insert((release.version, release.target)) {
                anyhow::bail!(
                    "duplicate {} {} release for {:?}",
                    self.agent_id,
                    release.version,
                    release.target
                );
            }
        }
        Ok(())
    }
}

pub fn source_is_official(source: &str, official_source_prefixes: &[&str]) -> bool {
    let Ok(source) = url::Url::parse(source) else {
        return false;
    };
    if source.scheme() != "https" {
        return false;
    }
    official_source_prefixes.iter().any(|prefix| {
        url::Url::parse(prefix).is_ok_and(|prefix| source.as_str().starts_with(prefix.as_str()))
    })
}

impl AgentArtifactFormat {
    fn executable_path(self) -> Option<&'static str> {
        match self {
            Self::Raw => None,
            Self::TarGz { executable_path } => Some(executable_path),
        }
    }
}

fn is_safe_archive_path(path: &str, executable_name: &str) -> bool {
    if path.is_empty() || path.contains('\\') {
        return false;
    }
    let path = std::path::Path::new(path);
    path.components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
        && path.file_name().is_some_and(|name| name == executable_name)
}

fn is_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_kind_registry;
    use remote::{RemoteArch, RemoteLibc, RemoteOs, RemotePlatform};

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn linux_glibc_target() -> RemotePlatform {
        RemotePlatform {
            os: RemoteOs::Linux,
            arch: RemoteArch::X86_64,
            libc: Some(RemoteLibc::Glibc),
        }
    }

    fn fixture_release(target: RemotePlatform) -> AgentRelease {
        AgentRelease {
            version: "1.2.3",
            target,
            source_url: "https://github.com/openai/codex/releases/download/v1.2.3/codex",
            source_sha256: DIGEST,
            source_verification: AgentSourceVerification::Sha256,
            executable_sha256: DIGEST,
            artifact: AgentArtifactFormat::Raw,
            executable_name: "codex",
            version_matcher: AgentVersionMatcher::Codex { version: "1.2.3" },
            self_update_environment: &[("CODEX_DISABLE_UPDATE", "1")],
        }
    }

    #[test]
    fn supported_target_resolves_to_one_pinned_release() {
        let target = linux_glibc_target();
        let releases = [fixture_release(target)];
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &["https://github.com/openai/codex/releases/download/"],
            &releases,
        );

        let release = catalog
            .release_for(target)
            .expect("supported target should have a release");

        assert_eq!(release.version, "1.2.3");
        assert_eq!(release.source_verification, AgentSourceVerification::Sha256);
    }

    #[test]
    fn duplicate_version_and_target_is_rejected() {
        let target = linux_glibc_target();
        let releases = [fixture_release(target), fixture_release(target)];
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &["https://github.com/openai/codex/releases/download/"],
            &releases,
        );

        let error = catalog
            .validate()
            .expect_err("duplicate release should be rejected");

        assert!(error.to_string().contains("duplicate codex 1.2.3"));
    }

    #[test]
    fn non_https_source_is_rejected() {
        let mut release = fixture_release(linux_glibc_target());
        release.source_url = "http://github.com/openai/codex/releases/download/v1.2.3/codex";
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &["https://github.com/openai/codex/releases/download/"],
            std::slice::from_ref(&release),
        );

        let error = catalog
            .validate()
            .expect_err("non-HTTPS source should be rejected");

        assert!(error.to_string().contains("official HTTPS source"));
    }

    #[test]
    fn source_path_cannot_escape_the_official_release_prefix() {
        let mut release = fixture_release(linux_glibc_target());
        release.source_url =
            "https://github.com/openai/codex/releases/download/../../attacker/artifact";
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &["https://github.com/openai/codex/releases/download/"],
            std::slice::from_ref(&release),
        );

        let error = catalog
            .validate()
            .expect_err("normalized source outside the official prefix should be rejected");

        assert!(error.to_string().contains("official HTTPS source"));
    }

    #[test]
    fn malformed_source_sha256_is_rejected() {
        let mut release = fixture_release(linux_glibc_target());
        release.source_sha256 = "not-a-sha256";
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &["https://github.com/openai/codex/releases/download/"],
            std::slice::from_ref(&release),
        );

        let error = catalog
            .validate()
            .expect_err("malformed source digest should be rejected");

        assert!(error.to_string().contains("source SHA-256"));
    }

    #[test]
    fn malformed_executable_sha256_is_rejected() {
        let mut release = fixture_release(linux_glibc_target());
        release.executable_sha256 = "";
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &["https://github.com/openai/codex/releases/download/"],
            std::slice::from_ref(&release),
        );

        let error = catalog
            .validate()
            .expect_err("missing executable digest should be rejected");

        assert!(error.to_string().contains("executable SHA-256"));
    }

    #[test]
    fn archive_executable_path_cannot_escape_the_archive() {
        let mut release = fixture_release(linux_glibc_target());
        release.artifact = AgentArtifactFormat::TarGz {
            executable_path: "../codex",
        };
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &["https://github.com/openai/codex/releases/download/"],
            std::slice::from_ref(&release),
        );

        let error = catalog
            .validate()
            .expect_err("archive traversal path should be rejected");

        assert!(error.to_string().contains("archive executable path"));
    }

    #[test]
    fn empty_self_update_environment_is_rejected() {
        let mut release = fixture_release(linux_glibc_target());
        release.self_update_environment = &[];
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &["https://github.com/openai/codex/releases/download/"],
            std::slice::from_ref(&release),
        );

        let error = catalog
            .validate()
            .expect_err("missing self-update suppression should be rejected");

        assert!(error.to_string().contains("self-update suppression"));
    }

    #[test]
    fn codex_version_matcher_accepts_formatting_but_rejects_wrong_identity_or_version() {
        let matcher = AgentVersionMatcher::Codex { version: "1.2.3" };

        assert!(matcher.matches("codex-cli 1.2.3\n"));
        assert!(matcher.matches("Codex 1.2.3"));
        assert!(!matcher.matches("codex 1.2.4"));
        assert!(!matcher.matches("claude 1.2.3"));
    }

    #[test]
    fn claude_version_matcher_accepts_known_formats_but_rejects_wrong_identity_or_version() {
        let matcher = AgentVersionMatcher::Claude { version: "2.3.4" };

        assert!(matcher.matches("2.3.4 (Claude Code)\n"));
        assert!(matcher.matches("Claude Code 2.3.4"));
        assert!(!matcher.matches("2.3.5 (Claude Code)"));
        assert!(!matcher.matches("2.3.4 (Codex)"));
    }

    #[test]
    fn every_registered_agent_suppresses_self_updates_per_process() {
        for kind in agent_kind_registry() {
            let policy = kind.self_update_policy();
            assert!(
                !policy.environment.is_empty() || !policy.arguments.is_empty(),
                "{} should define self-update suppression",
                kind.id
            );
        }
    }
}
