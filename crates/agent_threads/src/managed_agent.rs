use crate::{agent_release::AgentRelease, artifact_cache::AgentArtifactCache};
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use remote::{
    ConnectionSharing, Interactive, RemoteArch, RemoteConnection, RemoteLibc, RemoteOs,
    RemotePlatform,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use util::paths::{PathStyle, RemotePathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAgentArtifact {
    pub path: PathBuf,
    pub executable_sha256: String,
}

#[async_trait(?Send)]
pub trait AgentArtifactSource {
    async fn acquire(&self, release: &AgentRelease) -> Result<VerifiedAgentArtifact>;
}

pub struct CachedAgentArtifactSource {
    cache: AgentArtifactCache,
    official_source_prefixes: &'static [&'static str],
}

impl CachedAgentArtifactSource {
    pub fn new(
        http_client: Arc<dyn http_client::HttpClient>,
        official_source_prefixes: &'static [&'static str],
    ) -> Self {
        Self {
            cache: AgentArtifactCache::for_app(http_client),
            official_source_prefixes,
        }
    }
}

#[async_trait(?Send)]
impl AgentArtifactSource for CachedAgentArtifactSource {
    async fn acquire(&self, release: &AgentRelease) -> Result<VerifiedAgentArtifact> {
        let path = self
            .cache
            .acquire(release, self.official_source_prefixes)
            .await?;
        Ok(VerifiedAgentArtifact {
            path,
            executable_sha256: release.executable_sha256.to_string(),
        })
    }
}

#[async_trait(?Send)]
pub trait RemoteAgentHost {
    async fn app_data_directory(&self) -> Result<PathBuf>;
    async fn create_private_directory(&self, path: &Path) -> Result<()>;
    async fn upload_file(&self, source: &Path, destination: &Path) -> Result<()>;
    async fn write_file(&self, destination: &Path, content: &[u8]) -> Result<()>;
    async fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>>;
    async fn path_exists(&self, path: &Path) -> Result<bool>;
    async fn file_sha256(&self, path: &Path) -> Result<String>;
    async fn set_file_executable(&self, path: &Path) -> Result<()>;
    async fn run_version(&self, executable: &Path) -> Result<String>;
    async fn commit_directory(&self, source: &Path, destination: &Path) -> Result<()>;
    async fn remove_path(&self, path: &Path, recursive: bool) -> Result<()>;
}

pub struct RemoteClientAgentHost {
    connection: Arc<dyn RemoteConnection>,
    proto_client: rpc::AnyProtoClient,
    path_style: PathStyle,
}

impl RemoteClientAgentHost {
    pub fn new(remote_client: &remote::RemoteClient) -> Result<Self> {
        Ok(Self {
            connection: remote_client
                .remote_connection()
                .context("remote connection is unavailable")?,
            proto_client: remote_client.proto_client(),
            path_style: remote_client.path_style(),
        })
    }

    fn remote_path(&self, path: &Path) -> RemotePathBuf {
        RemotePathBuf::new(path.to_string_lossy().into_owned(), self.path_style)
    }
}

#[async_trait(?Send)]
impl RemoteAgentHost for RemoteClientAgentHost {
    async fn app_data_directory(&self) -> Result<PathBuf> {
        let response = self
            .proto_client
            .request(proto::GetRemoteAppDataDirectory {})
            .await?;
        if response.path.is_empty() {
            anyhow::bail!("remote server returned an empty application data directory");
        }
        Ok(PathBuf::from(response.path))
    }

    async fn create_private_directory(&self, path: &Path) -> Result<()> {
        self.proto_client
            .request(proto::CreatePrivateRemoteDirectory {
                path: path.to_string_lossy().into_owned(),
            })
            .await?;
        Ok(())
    }

    async fn upload_file(&self, source: &Path, destination: &Path) -> Result<()> {
        self.connection
            .upload_file_now(source.to_path_buf(), self.remote_path(destination))
            .await
    }

    async fn write_file(&self, destination: &Path, content: &[u8]) -> Result<()> {
        let temporary_path =
            std::env::temp_dir().join(format!("flint-agent-receipt-{}.json", uuid::Uuid::new_v4()));
        smol::fs::write(&temporary_path, content)
            .await
            .context("failed to stage local agent receipt")?;
        let upload_result = self.upload_file(&temporary_path, destination).await;
        if let Err(cleanup_error) = smol::fs::remove_file(&temporary_path).await {
            log::warn!(
                "failed to remove staged local agent receipt {}: {cleanup_error}",
                temporary_path.display()
            );
        }
        upload_result
    }

    async fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        let metadata = self
            .proto_client
            .request(proto::GetPathMetadata {
                project_id: proto::REMOTE_SERVER_PROJECT_ID,
                path: path.to_string_lossy().into_owned(),
            })
            .await?;
        if !metadata.exists {
            return Ok(None);
        }
        let response = self
            .proto_client
            .request(proto::ReadRemoteFile {
                dev_server_id: proto::REMOTE_SERVER_PROJECT_ID,
                path: path.to_string_lossy().into_owned(),
            })
            .await?;
        Ok(Some(response.content))
    }

    async fn path_exists(&self, path: &Path) -> Result<bool> {
        Ok(self
            .proto_client
            .request(proto::GetPathMetadata {
                project_id: proto::REMOTE_SERVER_PROJECT_ID,
                path: path.to_string_lossy().into_owned(),
            })
            .await?
            .exists)
    }

    async fn file_sha256(&self, path: &Path) -> Result<String> {
        let response = self
            .proto_client
            .request(proto::ComputeRemoteFileSha256 {
                path: path.to_string_lossy().into_owned(),
            })
            .await?;
        if response.sha256.len() != 64 {
            anyhow::bail!("remote server returned an invalid SHA-256 digest");
        }
        Ok(response.sha256)
    }

    async fn set_file_executable(&self, path: &Path) -> Result<()> {
        self.proto_client
            .request(proto::SetRemoteFileExecutable {
                path: path.to_string_lossy().into_owned(),
            })
            .await?;
        Ok(())
    }

    async fn run_version(&self, executable: &Path) -> Result<String> {
        const MAX_VERSION_OUTPUT_BYTES: usize = 64 * 1024;

        let command = self.connection.build_command(
            Some(executable.to_string_lossy().into_owned()),
            &["--version".to_string()],
            &collections::HashMap::default(),
            None,
            None,
            Interactive::No,
            ConnectionSharing::Shared,
        )?;
        let mut process = util::command::new_command(&command.program);
        process.args(&command.args).envs(&command.env);
        let output = process.output().await?;
        if !output.status.success() {
            anyhow::bail!("remote agent version check exited with {}", output.status);
        }
        if output.stdout.len() > MAX_VERSION_OUTPUT_BYTES {
            anyhow::bail!("remote agent version output exceeded the size limit");
        }
        String::from_utf8(output.stdout).context("remote agent version output was not UTF-8")
    }

    async fn commit_directory(&self, source: &Path, destination: &Path) -> Result<()> {
        self.proto_client
            .request(proto::RenameRemotePath {
                source: source.to_string_lossy().into_owned(),
                destination: destination.to_string_lossy().into_owned(),
            })
            .await?;
        Ok(())
    }

    async fn remove_path(&self, path: &Path, recursive: bool) -> Result<()> {
        self.proto_client
            .request(proto::RemoveRemotePath {
                path: path.to_string_lossy().into_owned(),
                recursive,
                ignore_if_missing: true,
            })
            .await?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedAgentInstallation {
    pub executable_path: PathBuf,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedAgentReceipt {
    agent_id: String,
    version: String,
    target: String,
    executable_name: String,
    executable_path: PathBuf,
    executable_sha256: String,
}

pub struct ManagedAgentProvisioner<A, H> {
    artifacts: A,
    remote_host: H,
}

impl<A, H> ManagedAgentProvisioner<A, H> {
    pub fn new(artifacts: A, remote_host: H) -> Self {
        Self {
            artifacts,
            remote_host,
        }
    }
}

impl<A, H> ManagedAgentProvisioner<A, H>
where
    A: AgentArtifactSource,
    H: RemoteAgentHost,
{
    pub async fn install(
        &self,
        agent_id: &str,
        release: &AgentRelease,
    ) -> Result<ManagedAgentInstallation> {
        validate_path_component(agent_id, "agent id")?;
        validate_path_component(release.version, "agent version")?;
        validate_path_component(release.executable_name, "agent executable name")?;

        let artifact = self.artifacts.acquire(release).await?;
        if artifact.executable_sha256 != release.executable_sha256 {
            anyhow::bail!("verified artifact digest does not match the pinned release");
        }

        let app_data_directory = self.remote_host.app_data_directory().await?;
        let remote_path_style = match release.target.os {
            RemoteOs::Windows => PathStyle::Windows,
            RemoteOs::Linux | RemoteOs::MacOs => PathStyle::Posix,
        };
        if !util::paths::is_absolute(
            app_data_directory.to_string_lossy().as_ref(),
            remote_path_style,
        ) {
            anyhow::bail!("remote application data directory is not absolute");
        }
        let target_name = target_name(release.target)?;
        let version_root = app_data_directory
            .join("agents")
            .join(agent_id)
            .join(release.version);
        let destination = version_root.join(&target_name);
        let destination_executable = destination.join(release.executable_name);
        let receipt_path = destination.join("receipt.json");
        if let Some(receipt_content) = self.remote_host.read_file(&receipt_path).await?
            && let Ok(receipt) = serde_json::from_slice::<ManagedAgentReceipt>(&receipt_content)
            && receipt
                == managed_receipt(
                    agent_id,
                    release,
                    &target_name,
                    destination_executable.clone(),
                )
            && self
                .remote_host
                .file_sha256(&destination_executable)
                .await
                .is_ok_and(|digest| digest == release.executable_sha256)
            && self
                .remote_host
                .run_version(&destination_executable)
                .await
                .is_ok_and(|output| release.version_matcher.matches(output.trim()))
        {
            return Ok(ManagedAgentInstallation {
                executable_path: destination_executable,
                version: release.version.to_string(),
            });
        }

        let staging = version_root.join(format!(".{target_name}.staging-{}", uuid::Uuid::new_v4()));
        let rollback =
            version_root.join(format!(".{target_name}.rollback-{}", uuid::Uuid::new_v4()));
        let staged_executable = staging.join(release.executable_name);
        let staged_receipt = staging.join("receipt.json");
        let receipt = serde_json::to_vec(&managed_receipt(
            agent_id,
            release,
            &target_name,
            destination_executable.clone(),
        ))?;

        let mut rollback_created = false;

        let result = async {
            self.remote_host
                .create_private_directory(&staging)
                .await
                .context("failed to create remote agent staging directory")?;
            self.remote_host
                .upload_file(&artifact.path, &staged_executable)
                .await
                .context("failed to upload verified agent executable")?;
            let remote_digest = self
                .remote_host
                .file_sha256(&staged_executable)
                .await
                .context("failed to verify uploaded agent executable")?;
            if remote_digest != release.executable_sha256 {
                anyhow::bail!("uploaded agent executable failed remote digest verification");
            }
            self.remote_host
                .set_file_executable(&staged_executable)
                .await
                .context("failed to make remote agent executable")?;
            let version_output = self
                .remote_host
                .run_version(&staged_executable)
                .await
                .context("failed to run the uploaded agent version check")?;
            if !release.version_matcher.matches(version_output.trim()) {
                anyhow::bail!("uploaded agent reported an unexpected version");
            }
            self.remote_host
                .write_file(&staged_receipt, &receipt)
                .await
                .context("failed to write remote agent receipt")?;
            if self.remote_host.path_exists(&destination).await? {
                self.remote_host
                    .commit_directory(&destination, &rollback)
                    .await
                    .context("failed to preserve the prior remote agent installation")?;
                rollback_created = true;
            }
            self.remote_host
                .commit_directory(&staging, &destination)
                .await
                .context("failed to commit remote agent installation")?;
            Ok(ManagedAgentInstallation {
                executable_path: destination_executable,
                version: release.version.to_string(),
            })
        }
        .await;

        if result.is_err()
            && let Err(cleanup_error) = self.remote_host.remove_path(&staging, true).await
        {
            log::warn!(
                "failed to clean remote agent staging directory {}: {cleanup_error:#}",
                staging.display()
            );
        }
        if result.is_err() && rollback_created {
            match self.remote_host.path_exists(&destination).await {
                Ok(true) => {
                    if let Err(cleanup_error) =
                        self.remote_host.remove_path(&destination, true).await
                    {
                        log::warn!(
                            "failed to remove incomplete remote agent installation {}: {cleanup_error:#}",
                            destination.display()
                        );
                    }
                }
                Ok(false) => {}
                Err(metadata_error) => {
                    log::warn!(
                        "failed to inspect incomplete remote agent installation {}: {metadata_error:#}",
                        destination.display()
                    );
                }
            }
            if let Err(restore_error) = self
                .remote_host
                .commit_directory(&rollback, &destination)
                .await
            {
                log::error!(
                    "failed to restore prior remote agent installation {}: {restore_error:#}",
                    destination.display()
                );
            }
        } else if result.is_ok()
            && rollback_created
            && let Err(cleanup_error) = self.remote_host.remove_path(&rollback, true).await
        {
            log::warn!(
                "failed to remove remote agent rollback directory {}: {cleanup_error:#}",
                rollback.display()
            );
        }
        result
    }
}

fn managed_receipt(
    agent_id: &str,
    release: &AgentRelease,
    target: &str,
    executable_path: PathBuf,
) -> ManagedAgentReceipt {
    ManagedAgentReceipt {
        agent_id: agent_id.to_string(),
        version: release.version.to_string(),
        target: target.to_string(),
        executable_name: release.executable_name.to_string(),
        executable_path,
        executable_sha256: release.executable_sha256.to_string(),
    }
}

fn validate_path_component(component: &str, name: &str) -> Result<()> {
    let path = Path::new(component);
    if component.is_empty()
        || path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        anyhow::bail!("{name} is not a safe path component");
    }
    Ok(())
}

fn target_name(target: RemotePlatform) -> Result<String> {
    let os = match target.os {
        RemoteOs::Linux => "linux",
        RemoteOs::MacOs => "macos",
        RemoteOs::Windows => "windows",
    };
    let architecture = match target.arch {
        RemoteArch::X86_64 => "x86_64",
        RemoteArch::Aarch64 => "aarch64",
    };
    let libc = match (target.os, target.libc) {
        (RemoteOs::Linux, Some(RemoteLibc::Glibc)) => "-glibc",
        (RemoteOs::Linux, Some(RemoteLibc::Musl)) => "-musl",
        (RemoteOs::Linux, Some(RemoteLibc::Unknown)) => "-unknown-libc",
        (RemoteOs::Linux, None) => anyhow::bail!("Linux agent target has no libc"),
        (_, _) => "",
    };
    Ok(format!("{os}-{architecture}{libc}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_release::{AgentArtifactFormat, AgentSourceVerification, AgentVersionMatcher};
    use remote::{RemoteArch, RemoteLibc, RemoteOs, RemotePlatform};
    use sha2::Digest as _;
    use std::{
        cell::{Cell, RefCell},
        collections::{HashMap, HashSet},
        rc::Rc,
    };

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn release() -> AgentRelease {
        AgentRelease {
            version: "1.2.3",
            target: RemotePlatform {
                os: RemoteOs::Linux,
                arch: RemoteArch::X86_64,
                libc: Some(RemoteLibc::Musl),
            },
            source_url: "https://official.example/agent",
            source_sha256: DIGEST,
            source_verification: AgentSourceVerification::Sha256,
            executable_sha256: DIGEST,
            artifact: AgentArtifactFormat::Raw,
            executable_name: "agent",
            version_matcher: AgentVersionMatcher::Codex { version: "1.2.3" },
            self_update_environment: &[("DISABLE_UPDATES", "1")],
        }
    }

    #[derive(Clone)]
    struct FakeArtifacts {
        events: Rc<RefCell<Vec<String>>>,
    }

    #[async_trait(?Send)]
    impl AgentArtifactSource for FakeArtifacts {
        async fn acquire(&self, _release: &AgentRelease) -> Result<VerifiedAgentArtifact> {
            self.events.borrow_mut().push("acquire".to_string());
            Ok(VerifiedAgentArtifact {
                path: PathBuf::from("/local/verified-agent"),
                executable_sha256: DIGEST.to_string(),
            })
        }
    }

    #[derive(Clone)]
    struct FakeRemoteHost {
        events: Rc<RefCell<Vec<String>>>,
        files: Rc<RefCell<HashMap<PathBuf, String>>>,
        paths: Rc<RefCell<HashSet<PathBuf>>>,
        fail_staging_commit: Rc<Cell<bool>>,
    }

    #[async_trait(?Send)]
    impl RemoteAgentHost for FakeRemoteHost {
        async fn app_data_directory(&self) -> Result<PathBuf> {
            Ok(PathBuf::from("/remote/flint"))
        }

        async fn create_private_directory(&self, path: &Path) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("mkdir:{}", path.display()));
            self.paths.borrow_mut().insert(path.to_path_buf());
            Ok(())
        }

        async fn upload_file(&self, _source: &Path, destination: &Path) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("upload:{}", destination.display()));
            self.files
                .borrow_mut()
                .insert(destination.to_path_buf(), DIGEST.to_string());
            Ok(())
        }

        async fn write_file(&self, destination: &Path, content: &[u8]) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("write:{}", destination.display()));
            self.files.borrow_mut().insert(
                destination.to_path_buf(),
                hex::encode(sha2::Sha256::digest(content)),
            );
            Ok(())
        }

        async fn read_file(&self, _path: &Path) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }

        async fn path_exists(&self, path: &Path) -> Result<bool> {
            Ok(self.paths.borrow().contains(path))
        }

        async fn file_sha256(&self, path: &Path) -> Result<String> {
            self.events
                .borrow_mut()
                .push(format!("digest:{}", path.display()));
            self.files
                .borrow()
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing file"))
        }

        async fn set_file_executable(&self, path: &Path) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("chmod:{}", path.display()));
            Ok(())
        }

        async fn run_version(&self, path: &Path) -> Result<String> {
            self.events
                .borrow_mut()
                .push(format!("version:{}", path.display()));
            Ok("codex 1.2.3".to_string())
        }

        async fn commit_directory(&self, source: &Path, destination: &Path) -> Result<()> {
            self.events.borrow_mut().push(format!(
                "commit:{}:{}",
                source.display(),
                destination.display()
            ));
            if source
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".staging-"))
                && self.fail_staging_commit.replace(false)
            {
                anyhow::bail!("injected staging commit failure");
            }
            self.paths.borrow_mut().remove(source);
            self.paths.borrow_mut().insert(destination.to_path_buf());
            Ok(())
        }

        async fn remove_path(&self, path: &Path, _recursive: bool) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("remove:{}", path.display()));
            self.paths.borrow_mut().remove(path);
            Ok(())
        }
    }

    #[test]
    fn verifies_before_upload_and_commits_only_after_remote_validation() {
        smol::block_on(async {
            let events = Rc::new(RefCell::new(Vec::new()));
            let provisioner = ManagedAgentProvisioner::new(
                FakeArtifacts {
                    events: events.clone(),
                },
                FakeRemoteHost {
                    events: events.clone(),
                    files: Rc::new(RefCell::new(HashMap::new())),
                    paths: Rc::new(RefCell::new(HashSet::new())),
                    fail_staging_commit: Rc::new(Cell::new(false)),
                },
            );

            let installation = provisioner
                .install("codex", &release())
                .await
                .expect("installation should succeed");

            assert_eq!(installation.version, "1.2.3");
            assert_eq!(
                installation.executable_path,
                PathBuf::from("/remote/flint/agents/codex/1.2.3/linux-x86_64-musl/agent")
            );
            let events = events.borrow();
            let acquire = events.iter().position(|event| event == "acquire").unwrap();
            let upload = events
                .iter()
                .position(|event| event.starts_with("upload:"))
                .unwrap();
            let digest = events
                .iter()
                .position(|event| event.starts_with("digest:"))
                .unwrap();
            let chmod = events
                .iter()
                .position(|event| event.starts_with("chmod:"))
                .unwrap();
            let version = events
                .iter()
                .position(|event| event.starts_with("version:"))
                .unwrap();
            let commit = events
                .iter()
                .position(|event| event.starts_with("commit:"))
                .unwrap();
            assert!(acquire < upload && upload < digest && digest < chmod);
            assert!(chmod < version && version < commit);
        });
    }

    #[test]
    fn failed_commit_restores_the_prior_installation() {
        smol::block_on(async {
            let events = Rc::new(RefCell::new(Vec::new()));
            let destination = PathBuf::from("/remote/flint/agents/codex/1.2.3/linux-x86_64-musl");
            let paths = Rc::new(RefCell::new(HashSet::from([destination.clone()])));
            let provisioner = ManagedAgentProvisioner::new(
                FakeArtifacts {
                    events: events.clone(),
                },
                FakeRemoteHost {
                    events: events.clone(),
                    files: Rc::new(RefCell::new(HashMap::new())),
                    paths: paths.clone(),
                    fail_staging_commit: Rc::new(Cell::new(true)),
                },
            );

            let error = provisioner
                .install("codex", &release())
                .await
                .expect_err("injected commit failure should fail installation");

            assert!(error.to_string().contains("failed to commit"));
            assert!(paths.borrow().contains(&destination));
            assert!(
                events
                    .borrow()
                    .iter()
                    .any(|event| { event.starts_with("commit:") && event.contains(".rollback-") })
            );
        });
    }
}
