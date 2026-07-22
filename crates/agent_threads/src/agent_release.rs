use remote::RemotePlatform;
use std::{collections::HashSet, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentArtifactFormat {
    Raw,
    TarGz {
        executable_path: &'static str,
    },
    TarGzBundle {
        root_path: &'static str,
        executable_path: &'static str,
    },
    ZipBundle {
        root_path: &'static str,
        executable_path: &'static str,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSourceVerification {
    Sha256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentVersionMatcher {
    Codex { version: &'static str },
    Claude { version: &'static str },
    Pi { version: &'static str },
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
            Self::Pi { version } => output.trim() == version,
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

macro_rules! claude_release {
    ($os:expr, $arch:expr, $libc:expr, $platform:literal, $name:literal, $digest:literal) => {
        AgentRelease {
            version: "2.1.205",
            target: RemotePlatform {
                os: $os,
                arch: $arch,
                libc: $libc,
            },
            source_url: concat!(
                "https://downloads.claude.ai/claude-code-releases/2.1.205/",
                $platform,
                "/",
                $name
            ),
            source_sha256: $digest,
            source_verification: AgentSourceVerification::Sha256,
            executable_sha256: $digest,
            artifact: AgentArtifactFormat::Raw,
            executable_name: $name,
            version_matcher: AgentVersionMatcher::Claude { version: "2.1.205" },
            self_update_environment: &[("DISABLE_UPDATES", "1")],
        }
    };
}

pub const CLAUDE_RELEASES: &[AgentRelease] = &[
    claude_release!(
        remote::RemoteOs::MacOs,
        remote::RemoteArch::Aarch64,
        None,
        "darwin-arm64",
        "claude",
        "33e28624c5ae84f2bd7d2d8761e5d2e77997ba965cb11b6448de6b6e2c566f9c"
    ),
    claude_release!(
        remote::RemoteOs::MacOs,
        remote::RemoteArch::X86_64,
        None,
        "darwin-x64",
        "claude",
        "4299a3f48551ef365f2d056f24d87e84b822c4c10b6acc46979446b7b5c60ceb"
    ),
    claude_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::Aarch64,
        Some(remote::RemoteLibc::Glibc),
        "linux-arm64",
        "claude",
        "c1874c85bcd3a88b70439fd50ff5910b7e6ac5371c14dd49d4ccc2878a592d09"
    ),
    claude_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::X86_64,
        Some(remote::RemoteLibc::Glibc),
        "linux-x64",
        "claude",
        "dd8734c0b6a503fe1d17425184e57b397c30bb0337a33f1470d9985febfe5b09"
    ),
    claude_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::Aarch64,
        Some(remote::RemoteLibc::Musl),
        "linux-arm64-musl",
        "claude",
        "a8cd2a626d7d0b5fb3516164a4cf3b4acbbadb053a5b1b2a2462ccbd2ebf6bde"
    ),
    claude_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::X86_64,
        Some(remote::RemoteLibc::Musl),
        "linux-x64-musl",
        "claude",
        "20018df16e75f4287c3bfb088e04019452cf262f66ee43041e285113c4e479d8"
    ),
    claude_release!(
        remote::RemoteOs::Windows,
        remote::RemoteArch::X86_64,
        None,
        "win32-x64",
        "claude.exe",
        "f09120889098672074e7c5166d5474da0c5482f2bec898b3510cacd9c1fefa42"
    ),
    claude_release!(
        remote::RemoteOs::Windows,
        remote::RemoteArch::Aarch64,
        None,
        "win32-arm64",
        "claude.exe",
        "9a86e5acbc584ab7c1b684f1cc1bf5c7bddd6afd4817c0d2c2113d15bfbff0a9"
    ),
];

macro_rules! codex_archive_release {
    ($os:expr, $arch:expr, $libc:expr, $target:literal, $source_digest:literal, $executable_digest:literal) => {
        AgentRelease {
            version: "0.144.6",
            target: RemotePlatform {
                os: $os,
                arch: $arch,
                libc: $libc,
            },
            source_url: concat!(
                "https://github.com/openai/codex/releases/download/rust-v0.144.6/codex-",
                $target,
                ".tar.gz"
            ),
            source_sha256: $source_digest,
            source_verification: AgentSourceVerification::Sha256,
            executable_sha256: $executable_digest,
            artifact: AgentArtifactFormat::TarGz {
                executable_path: concat!("codex-", $target),
            },
            executable_name: "codex",
            version_matcher: AgentVersionMatcher::Codex { version: "0.144.6" },
            self_update_environment: &[],
        }
    };
}

macro_rules! codex_windows_release {
    ($arch:expr, $target:literal, $digest:literal) => {
        AgentRelease {
            version: "0.144.6",
            target: RemotePlatform {
                os: remote::RemoteOs::Windows,
                arch: $arch,
                libc: None,
            },
            source_url: concat!(
                "https://github.com/openai/codex/releases/download/rust-v0.144.6/codex-",
                $target,
                ".exe"
            ),
            source_sha256: $digest,
            source_verification: AgentSourceVerification::Sha256,
            executable_sha256: $digest,
            artifact: AgentArtifactFormat::Raw,
            executable_name: "codex.exe",
            version_matcher: AgentVersionMatcher::Codex { version: "0.144.6" },
            self_update_environment: &[],
        }
    };
}

pub const CODEX_RELEASES: &[AgentRelease] = &[
    codex_archive_release!(
        remote::RemoteOs::MacOs,
        remote::RemoteArch::Aarch64,
        None,
        "aarch64-apple-darwin",
        "023590f828bc9507ac61132ee35e74d3c5d33fb5ba3e1ca4fc2e013a2f71a3d7",
        "80a3933d11a9d13ef806aa24f7bb8afc9169cfe4e9b09d6da6a92922cbde9cff"
    ),
    codex_archive_release!(
        remote::RemoteOs::MacOs,
        remote::RemoteArch::X86_64,
        None,
        "x86_64-apple-darwin",
        "763c81a56ba24a4f6c2fd256ed7ee1775caeccd22537d28887de8f6864ac5947",
        "bd6ec7e28b4682e010f6bf3953166d2a2b178d50beb448c137d33d53450b2802"
    ),
    codex_archive_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::Aarch64,
        Some(remote::RemoteLibc::Glibc),
        "aarch64-unknown-linux-musl",
        "8eddae5e6c009dff9ba51ae1bfe3bdd9ff4c1ccc93a48cc6860db1cd9fdf11be",
        "57a159f67999794494a172e71c12c6b5a211542ea90c66ea2ce9e6ac1edec6b1"
    ),
    codex_archive_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::Aarch64,
        Some(remote::RemoteLibc::Musl),
        "aarch64-unknown-linux-musl",
        "8eddae5e6c009dff9ba51ae1bfe3bdd9ff4c1ccc93a48cc6860db1cd9fdf11be",
        "57a159f67999794494a172e71c12c6b5a211542ea90c66ea2ce9e6ac1edec6b1"
    ),
    codex_archive_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::X86_64,
        Some(remote::RemoteLibc::Glibc),
        "x86_64-unknown-linux-musl",
        "6a9def51a0ad8cea6684d8eb3bf033c89f33e3bc5cfe492f1a1e0a718451a1c6",
        "a31ae9450a26216eb1e7c53102fd42123dd675974310b0e2ca3aa4cb622a2c15"
    ),
    codex_archive_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::X86_64,
        Some(remote::RemoteLibc::Musl),
        "x86_64-unknown-linux-musl",
        "6a9def51a0ad8cea6684d8eb3bf033c89f33e3bc5cfe492f1a1e0a718451a1c6",
        "a31ae9450a26216eb1e7c53102fd42123dd675974310b0e2ca3aa4cb622a2c15"
    ),
    codex_windows_release!(
        remote::RemoteArch::X86_64,
        "x86_64-pc-windows-msvc",
        "4b76ded066d0239115ca97473d010c92072bc5c5550a45dd7cbebe1e9eb956a7"
    ),
    codex_windows_release!(
        remote::RemoteArch::Aarch64,
        "aarch64-pc-windows-msvc",
        "2a23cdd00332064c27d4aa453d33d7a66a060be89fff33b8a388ba4db7e4c620"
    ),
];

macro_rules! pi_tar_release {
    ($os:expr, $arch:expr, $libc:expr, $asset:literal, $source_digest:literal, $executable_digest:literal) => {
        AgentRelease {
            version: "0.81.1",
            target: RemotePlatform {
                os: $os,
                arch: $arch,
                libc: $libc,
            },
            source_url: concat!(
                "https://github.com/earendil-works/pi/releases/download/v0.81.1/",
                $asset
            ),
            source_sha256: $source_digest,
            source_verification: AgentSourceVerification::Sha256,
            executable_sha256: $executable_digest,
            artifact: AgentArtifactFormat::TarGzBundle {
                root_path: "pi",
                executable_path: "pi/pi",
            },
            executable_name: "pi",
            version_matcher: AgentVersionMatcher::Pi { version: "0.81.1" },
            self_update_environment: &[("PI_SKIP_VERSION_CHECK", "1"), ("PI_TELEMETRY", "0")],
        }
    };
}

macro_rules! pi_windows_release {
    ($arch:expr, $asset:literal, $source_digest:literal, $executable_digest:literal) => {
        AgentRelease {
            version: "0.81.1",
            target: RemotePlatform {
                os: remote::RemoteOs::Windows,
                arch: $arch,
                libc: None,
            },
            source_url: concat!(
                "https://github.com/earendil-works/pi/releases/download/v0.81.1/",
                $asset
            ),
            source_sha256: $source_digest,
            source_verification: AgentSourceVerification::Sha256,
            executable_sha256: $executable_digest,
            artifact: AgentArtifactFormat::ZipBundle {
                root_path: "",
                executable_path: "pi.exe",
            },
            executable_name: "pi.exe",
            version_matcher: AgentVersionMatcher::Pi { version: "0.81.1" },
            self_update_environment: &[("PI_SKIP_VERSION_CHECK", "1"), ("PI_TELEMETRY", "0")],
        }
    };
}

pub const PI_RELEASES: &[AgentRelease] = &[
    pi_tar_release!(
        remote::RemoteOs::MacOs,
        remote::RemoteArch::Aarch64,
        None,
        "pi-darwin-arm64.tar.gz",
        "a24834019ec02ee5a475ff1c5a5e9f838974191ba6adc4348f6e6475a7c7667b",
        "271b7a506398e4ece04c664c7723705d4fa874c98e7a62d7b289e1fa582cf3c9"
    ),
    pi_tar_release!(
        remote::RemoteOs::MacOs,
        remote::RemoteArch::X86_64,
        None,
        "pi-darwin-x64.tar.gz",
        "ecaed0ef0fcaeff2e475294fc34b2d7de4700434ab9df23cdb0fffd9cfadf5b8",
        "330434ba869a9582557be97ce5423afbdcc8553e16a9301cfdd9740ddc87c69b"
    ),
    pi_tar_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::Aarch64,
        Some(remote::RemoteLibc::Glibc),
        "pi-linux-arm64.tar.gz",
        "c049e132c85466224d57d19f7924909b0c0fdbc9bed8e091ddc361830704b392",
        "5d098361cdcbb3abad53988d692bcc6c2619bfd8875e8256903f448c54e82a5c"
    ),
    pi_tar_release!(
        remote::RemoteOs::Linux,
        remote::RemoteArch::X86_64,
        Some(remote::RemoteLibc::Glibc),
        "pi-linux-x64.tar.gz",
        "1f6e23d9ec0668a13cea9c786e3d54c1fc679b8e22e7f6bfade0349f4807cbf2",
        "e69d579772ed139458a144f165e2e0f05d50a1a86bf7557bf02eef1f75691505"
    ),
    pi_windows_release!(
        remote::RemoteArch::Aarch64,
        "pi-windows-arm64.zip",
        "875dfb42e2ad20e81430365cce48c5ddcab560c3b9ee474d2d5c7ff6345269eb",
        "7d3fbae1d56b5f95607b0162f65c1e6793333d4ff2df01a7a89ef30ea44c9611"
    ),
    pi_windows_release!(
        remote::RemoteArch::X86_64,
        "pi-windows-x64.zip",
        "6c46cca1fa94234982e56dc60a453d4bc57dc45efc2e16f97bbc6eace7a7de60",
        "1421d967cc6497c0585f057414a9a7da8a69b339fc001dc49461c419acec9d85"
    ),
];

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
                && !is_safe_archive_path(path)
            {
                anyhow::bail!(
                    "{} {} has an invalid archive executable path",
                    self.agent_id,
                    release.version
                );
            }
            if let AgentArtifactFormat::TarGzBundle {
                root_path,
                executable_path,
            }
            | AgentArtifactFormat::ZipBundle {
                root_path,
                executable_path,
            } = release.artifact
            {
                if (!root_path.is_empty() && !is_safe_archive_path(root_path))
                    || Path::new(executable_path)
                        .strip_prefix(root_path)
                        .ok()
                        .filter(|path| *path == Path::new(release.executable_name))
                        .is_none()
                {
                    anyhow::bail!(
                        "{} {} has an invalid bundle layout",
                        self.agent_id,
                        release.version
                    );
                }
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
    pub(crate) fn executable_path(self) -> Option<&'static str> {
        match self {
            Self::Raw => None,
            Self::TarGz { executable_path }
            | Self::TarGzBundle {
                executable_path, ..
            }
            | Self::ZipBundle {
                executable_path, ..
            } => Some(executable_path),
        }
    }
}

fn is_safe_archive_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\\') {
        return false;
    }
    let path = std::path::Path::new(path);
    path.components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
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
    fn release_may_use_kind_level_argument_update_suppression() {
        let mut release = fixture_release(linux_glibc_target());
        release.self_update_environment = &[];
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &["https://github.com/openai/codex/releases/download/"],
            std::slice::from_ref(&release),
        );

        catalog
            .validate()
            .expect("kind-level launch arguments may suppress updates");
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
    fn pi_version_matcher_accepts_exact_version_only() {
        let matcher = AgentVersionMatcher::Pi { version: "0.81.1" };

        assert!(matcher.matches("0.81.1\n"));
        assert!(!matcher.matches("0.81.0"));
        assert!(!matcher.matches("pi 0.81.1"));
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

    #[test]
    fn pinned_claude_release_catalog_is_valid_and_covers_supported_targets() {
        let catalog = AgentReleaseCatalog::new(
            "claude",
            &["https://downloads.claude.ai/claude-code-releases/"],
            CLAUDE_RELEASES,
        );

        catalog
            .validate()
            .expect("pinned Claude release catalog should be valid");
        assert_eq!(CLAUDE_RELEASES.len(), 8);
        assert!(catalog.release_for(linux_glibc_target()).is_some());
    }

    #[test]
    fn pinned_codex_release_catalog_is_valid_and_covers_supported_targets() {
        let catalog = AgentReleaseCatalog::new(
            "codex",
            &[
                "https://github.com/openai/codex/releases/download/",
                "https://release-assets.githubusercontent.com/",
            ],
            CODEX_RELEASES,
        );

        catalog
            .validate()
            .expect("pinned Codex release catalog should be valid");
        assert_eq!(CODEX_RELEASES.len(), 8);
        assert!(catalog.release_for(linux_glibc_target()).is_some());
    }

    #[test]
    fn pinned_pi_release_catalog_is_valid_and_covers_published_targets() {
        let catalog = AgentReleaseCatalog::new(
            "pi",
            &[
                "https://github.com/earendil-works/pi/releases/download/",
                "https://release-assets.githubusercontent.com/",
            ],
            PI_RELEASES,
        );

        catalog
            .validate()
            .expect("pinned Pi release catalog should be valid");
        assert_eq!(PI_RELEASES.len(), 6);
        assert!(catalog.release_for(linux_glibc_target()).is_some());
        assert!(
            catalog
                .release_for(RemotePlatform {
                    os: RemoteOs::Linux,
                    arch: RemoteArch::X86_64,
                    libc: Some(RemoteLibc::Musl),
                })
                .is_none()
        );
    }
}
