use crate::{
    agent_release::AgentRelease,
    artifact_cache::{AgentArtifactCache, AgentArtifactDownloadProgress},
};
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use futures::AsyncReadExt as _;
use remote::{
    ConnectionSharing, Interactive, RemoteArch, RemoteConnection, RemoteLibc, RemoteOs,
    RemotePlatform,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use util::paths::{PathStyle, RemotePathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAgentArtifact {
    pub path: PathBuf,
    pub bundle_root: Option<PathBuf>,
    pub executable_sha256: String,
}

#[async_trait(?Send)]
pub trait AgentArtifactSource {
    async fn acquire(&self, release: &AgentRelease) -> Result<VerifiedAgentArtifact>;
}

pub struct CachedAgentArtifactSource {
    cache: Arc<AgentArtifactCache>,
    official_source_prefixes: &'static [&'static str],
    report_progress: Option<Rc<dyn Fn(AgentArtifactDownloadProgress)>>,
}

impl CachedAgentArtifactSource {
    pub fn new(
        http_client: Arc<dyn http_client::HttpClient>,
        official_source_prefixes: &'static [&'static str],
    ) -> Self {
        Self {
            cache: Arc::new(AgentArtifactCache::for_app(http_client)),
            official_source_prefixes,
            report_progress: None,
        }
    }

    pub fn from_cache(
        cache: Arc<AgentArtifactCache>,
        official_source_prefixes: &'static [&'static str],
    ) -> Self {
        Self {
            cache,
            official_source_prefixes,
            report_progress: None,
        }
    }

    pub fn from_cache_with_progress(
        cache: Arc<AgentArtifactCache>,
        official_source_prefixes: &'static [&'static str],
        report_progress: impl Fn(AgentArtifactDownloadProgress) + 'static,
    ) -> Self {
        Self {
            cache,
            official_source_prefixes,
            report_progress: Some(Rc::new(report_progress)),
        }
    }
}

#[async_trait(?Send)]
impl AgentArtifactSource for CachedAgentArtifactSource {
    async fn acquire(&self, release: &AgentRelease) -> Result<VerifiedAgentArtifact> {
        let path = match self.report_progress.as_ref() {
            Some(report_progress) => {
                self.cache
                    .acquire_with_progress(release, self.official_source_prefixes, |progress| {
                        report_progress(progress)
                    })
                    .await?
            }
            None => {
                self.cache
                    .acquire(release, self.official_source_prefixes)
                    .await?
            }
        };
        Ok(VerifiedAgentArtifact {
            bundle_root: match release.artifact {
                crate::agent_release::AgentArtifactFormat::TarGzBundle { .. }
                | crate::agent_release::AgentArtifactFormat::ZipBundle { .. } => {
                    path.parent().map(Path::to_path_buf)
                }
                _ => None,
            },
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedAgentInstallPhase {
    CheckingRemote,
    Reusing,
    Uploading,
    VerifyingRemote,
    Installing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedAgentReceipt {
    agent_id: String,
    version: String,
    target: String,
    executable_name: String,
    executable_path: PathBuf,
    executable_sha256: String,
    source_sha256: String,
    files: Vec<ManagedAgentReceiptFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedAgentReceiptFile {
    relative_path: PathBuf,
    sha256: String,
}

struct ManagedAgentPaths {
    version_root: PathBuf,
    destination: PathBuf,
    destination_executable: PathBuf,
    receipt_path: PathBuf,
    target_name: String,
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
    async fn installation_paths(
        &self,
        agent_id: &str,
        release: &AgentRelease,
    ) -> Result<ManagedAgentPaths> {
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
        Ok(ManagedAgentPaths {
            version_root,
            destination,
            destination_executable,
            receipt_path,
            target_name,
        })
    }

    pub async fn find_installed_with_progress(
        &self,
        agent_id: &str,
        release: &AgentRelease,
        mut report_progress: impl FnMut(ManagedAgentInstallPhase),
    ) -> Result<Option<ManagedAgentInstallation>> {
        validate_path_component(agent_id, "agent id")?;
        validate_path_component(release.version, "agent version")?;
        validate_path_component(release.executable_name, "agent executable name")?;

        let paths = self.installation_paths(agent_id, release).await?;
        report_progress(ManagedAgentInstallPhase::CheckingRemote);
        if let Some(receipt_content) = self.remote_host.read_file(&paths.receipt_path).await?
            && let Ok(receipt) = serde_json::from_slice::<ManagedAgentReceipt>(&receipt_content)
            && receipt_matches_release(
                &receipt,
                agent_id,
                release,
                &paths.target_name,
                &paths.destination_executable,
            )
            && self
                .receipt_files_are_valid(&paths.destination, &receipt)
                .await
            && self
                .remote_host
                .run_version(&paths.destination_executable)
                .await
                .is_ok_and(|output| release.version_matcher.matches(output.trim()))
        {
            report_progress(ManagedAgentInstallPhase::Reusing);
            return Ok(Some(ManagedAgentInstallation {
                executable_path: paths.destination_executable,
                version: release.version.to_string(),
            }));
        }
        Ok(None)
    }

    async fn receipt_files_are_valid(
        &self,
        destination: &Path,
        receipt: &ManagedAgentReceipt,
    ) -> bool {
        let mut relative_paths = std::collections::HashSet::new();
        for file in &receipt.files {
            if !is_safe_relative_path(&file.relative_path)
                || !relative_paths.insert(file.relative_path.clone())
            {
                return false;
            }
            let path = destination.join(&file.relative_path);
            if !self
                .remote_host
                .file_sha256(&path)
                .await
                .is_ok_and(|digest| digest == file.sha256)
            {
                return false;
            }
        }
        !receipt.files.is_empty()
    }

    pub async fn install(
        &self,
        agent_id: &str,
        release: &AgentRelease,
    ) -> Result<ManagedAgentInstallation> {
        self.install_with_progress(agent_id, release, |_| {}).await
    }

    pub async fn install_with_progress(
        &self,
        agent_id: &str,
        release: &AgentRelease,
        mut report_progress: impl FnMut(ManagedAgentInstallPhase),
    ) -> Result<ManagedAgentInstallation> {
        validate_path_component(agent_id, "agent id")?;
        validate_path_component(release.version, "agent version")?;
        validate_path_component(release.executable_name, "agent executable name")?;

        if let Some(installation) = self
            .find_installed_with_progress(agent_id, release, &mut report_progress)
            .await?
        {
            return Ok(installation);
        }

        let artifact = self.artifacts.acquire(release).await?;
        if artifact.executable_sha256 != release.executable_sha256 {
            anyhow::bail!("verified artifact digest does not match the pinned release");
        }

        let ManagedAgentPaths {
            version_root,
            destination,
            destination_executable,
            target_name,
            ..
        } = self.installation_paths(agent_id, release).await?;

        let staging = version_root.join(format!(".{target_name}.staging-{}", uuid::Uuid::new_v4()));
        let rollback =
            version_root.join(format!(".{target_name}.rollback-{}", uuid::Uuid::new_v4()));
        let staged_executable = staging.join(release.executable_name);
        let staged_receipt = staging.join("receipt.json");
        let mut rollback_created = false;
        let artifact_files = artifact_files(&artifact).await?;
        let receipt = serde_json::to_vec(&managed_receipt(
            agent_id,
            release,
            &target_name,
            destination_executable.clone(),
            artifact_files
                .iter()
                .map(|file| ManagedAgentReceiptFile {
                    relative_path: file.relative_path.clone(),
                    sha256: file.sha256.clone(),
                })
                .collect(),
        ))?;

        let result = async {
            self.remote_host
                .create_private_directory(&staging)
                .await
                .context("failed to create remote agent staging directory")?;
            report_progress(ManagedAgentInstallPhase::Uploading);
            for file in &artifact_files {
                let destination = staging.join(&file.relative_path);
                if let Some(parent) = destination.parent() {
                    self.remote_host.create_private_directory(parent).await?;
                }
                self.remote_host
                    .upload_file(&file.source_path, &destination)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to upload verified agent file {}",
                            file.relative_path.display()
                        )
                    })?;
            }
            report_progress(ManagedAgentInstallPhase::VerifyingRemote);
            for file in &artifact_files {
                let destination = staging.join(&file.relative_path);
                let remote_digest = self
                    .remote_host
                    .file_sha256(&destination)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to verify uploaded agent file {}",
                            file.relative_path.display()
                        )
                    })?;
                if remote_digest != file.sha256 {
                    anyhow::bail!("uploaded agent file failed remote digest verification");
                }
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
            report_progress(ManagedAgentInstallPhase::Installing);
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

struct ArtifactFile {
    source_path: PathBuf,
    relative_path: PathBuf,
    sha256: String,
}

async fn artifact_files(artifact: &VerifiedAgentArtifact) -> Result<Vec<ArtifactFile>> {
    let Some(bundle_root) = artifact.bundle_root.as_ref() else {
        let relative_path = artifact
            .path
            .file_name()
            .context("verified agent executable has no file name")?;
        return Ok(vec![ArtifactFile {
            source_path: artifact.path.clone(),
            relative_path: PathBuf::from(relative_path),
            sha256: artifact.executable_sha256.clone(),
        }]);
    };
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(bundle_root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            anyhow::bail!("verified agent bundle contains a symbolic link");
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative_path = entry.path().strip_prefix(bundle_root)?;
        if relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            anyhow::bail!("verified agent bundle contains an unsafe path");
        }
        files.push(ArtifactFile {
            source_path: entry.path().to_path_buf(),
            relative_path: relative_path.to_path_buf(),
            sha256: file_sha256(entry.path()).await?,
        });
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let executable = files
        .iter()
        .find(|file| file.source_path == artifact.path)
        .context("verified agent bundle does not contain its executable")?;
    if executable.sha256 != artifact.executable_sha256 {
        anyhow::bail!("verified agent bundle executable digest changed before upload");
    }
    Ok(files)
}

async fn file_sha256(path: &Path) -> Result<String> {
    let mut file = smol::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn managed_receipt(
    agent_id: &str,
    release: &AgentRelease,
    target: &str,
    executable_path: PathBuf,
    files: Vec<ManagedAgentReceiptFile>,
) -> ManagedAgentReceipt {
    ManagedAgentReceipt {
        agent_id: agent_id.to_string(),
        version: release.version.to_string(),
        target: target.to_string(),
        executable_name: release.executable_name.to_string(),
        executable_path,
        executable_sha256: release.executable_sha256.to_string(),
        source_sha256: release.source_sha256.to_string(),
        files,
    }
}

fn receipt_matches_release(
    receipt: &ManagedAgentReceipt,
    agent_id: &str,
    release: &AgentRelease,
    target: &str,
    executable_path: &Path,
) -> bool {
    receipt.agent_id == agent_id
        && receipt.version == release.version
        && receipt.target == target
        && receipt.executable_name == release.executable_name
        && receipt.executable_path == executable_path
        && receipt.executable_sha256 == release.executable_sha256
        && receipt.source_sha256 == release.source_sha256
        && receipt.files.iter().any(|file| {
            file.relative_path == Path::new(release.executable_name)
                && file.sha256 == release.executable_sha256
        })
}

fn is_safe_relative_path(path: &Path) -> bool {
    path.components().next().is_some()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
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
    use http_client::{AsyncBody, FakeHttpClient, Response};
    use remote::{RemoteArch, RemoteLibc, RemoteOs, RemotePlatform};
    use std::{
        cell::{Cell, RefCell},
        collections::{HashMap, HashSet},
        rc::Rc,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
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

    #[test]
    fn separate_artifact_sources_share_one_cache_download() {
        smol::block_on(async {
            let request_count = Arc::new(AtomicUsize::new(0));
            let (download_started, download_started_receiver) = async_channel::bounded(1);
            let (release_download, release_download_receiver) = async_channel::bounded(1);
            let http_client = FakeHttpClient::create({
                let request_count = request_count.clone();
                move |_| {
                    request_count.fetch_add(1, Ordering::SeqCst);
                    let download_started = download_started.clone();
                    let release_download_receiver = release_download_receiver.clone();
                    async move {
                        download_started.send(()).await?;
                        release_download_receiver.recv().await?;
                        Ok(Response::builder()
                            .status(200)
                            .body(AsyncBody::from("agent-bytes"))
                            .expect("fixture response should build"))
                    }
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = Arc::new(AgentArtifactCache::new(
                directory.path().to_path_buf(),
                http_client,
            ));
            let first = CachedAgentArtifactSource::from_cache(
                cache.clone(),
                &["https://official.example/"],
            );
            let second =
                CachedAgentArtifactSource::from_cache(cache, &["https://official.example/"]);
            let digest: &'static str =
                Box::leak(format!("{:x}", sha2::Sha256::digest(b"agent-bytes")).into_boxed_str());
            let mut release = release();
            release.source_sha256 = digest;
            release.executable_sha256 = digest;
            let release_gate = async {
                download_started_receiver.recv().await?;
                release_download.send(()).await?;
                anyhow::Ok(())
            };

            let (first_result, second_result, released) = futures::join!(
                first.acquire(&release),
                second.acquire(&release),
                release_gate,
            );
            released.expect("fixture download should be released");

            assert_eq!(
                first_result.expect("first source should succeed").path,
                second_result.expect("second source should succeed").path
            );
            assert_eq!(request_count.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn cached_artifact_source_forwards_download_progress() {
        smol::block_on(async {
            let http_client = FakeHttpClient::create(|_| async {
                Ok(Response::builder()
                    .status(200)
                    .header(http_client::http::header::CONTENT_LENGTH, "11")
                    .body(AsyncBody::from("agent-bytes"))
                    .expect("fixture response should build"))
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = Arc::new(AgentArtifactCache::new(
                directory.path().to_path_buf(),
                http_client,
            ));
            let progress = Rc::new(RefCell::new(Vec::new()));
            let source = CachedAgentArtifactSource::from_cache_with_progress(
                cache,
                &["https://official.example/"],
                {
                    let progress = progress.clone();
                    move |update| progress.borrow_mut().push(update)
                },
            );
            let digest: &'static str =
                Box::leak(format!("{:x}", sha2::Sha256::digest(b"agent-bytes")).into_boxed_str());
            let mut release = release();
            release.source_sha256 = digest;
            release.executable_sha256 = digest;

            source.acquire(&release).await.unwrap();

            assert_eq!(
                progress.borrow().last().map(|update| update.complete),
                Some(true)
            );
        });
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
                bundle_root: None,
                executable_sha256: DIGEST.to_string(),
            })
        }
    }

    #[derive(Clone)]
    struct FakeBundleArtifacts {
        root: PathBuf,
    }

    #[async_trait(?Send)]
    impl AgentArtifactSource for FakeBundleArtifacts {
        async fn acquire(&self, release: &AgentRelease) -> Result<VerifiedAgentArtifact> {
            Ok(VerifiedAgentArtifact {
                path: self.root.join("agent"),
                bundle_root: Some(self.root.clone()),
                executable_sha256: release.executable_sha256.to_string(),
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

        async fn upload_file(&self, source: &Path, destination: &Path) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("upload:{}", destination.display()));
            let digest = std::fs::read(source)
                .map(|content| hex::encode(sha2::Sha256::digest(content)))
                .unwrap_or_else(|_| DIGEST.to_string());
            self.files
                .borrow_mut()
                .insert(destination.to_path_buf(), digest);
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

        async fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>> {
            Ok(self
                .files
                .borrow()
                .get(path)
                .map(|content| content.as_bytes().to_vec()))
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
    fn reuses_valid_remote_installation_before_acquiring_artifact() {
        smol::block_on(async {
            let release = release();
            let events = Rc::new(RefCell::new(Vec::new()));
            let phases = Rc::new(RefCell::new(Vec::new()));
            let target = target_name(release.target).unwrap();
            let destination = PathBuf::from("/remote/flint/agents/codex/1.2.3").join(&target);
            let executable = destination.join(release.executable_name);
            let receipt = serde_json::to_string(&managed_receipt(
                "codex",
                &release,
                &target,
                executable.clone(),
                vec![ManagedAgentReceiptFile {
                    relative_path: PathBuf::from(release.executable_name),
                    sha256: release.executable_sha256.to_string(),
                }],
            ))
            .unwrap();
            let files = Rc::new(RefCell::new(HashMap::from([
                (executable.clone(), DIGEST.to_string()),
                (destination.join("receipt.json"), receipt),
            ])));
            let provisioner = ManagedAgentProvisioner::new(
                FakeArtifacts {
                    events: events.clone(),
                },
                FakeRemoteHost {
                    events: events.clone(),
                    files,
                    paths: Rc::new(RefCell::new(HashSet::from([destination]))),
                    fail_staging_commit: Rc::new(Cell::new(false)),
                },
            );

            let installation = provisioner
                .install_with_progress("codex", &release, {
                    let phases = phases.clone();
                    move |phase| phases.borrow_mut().push(format!("{phase:?}"))
                })
                .await
                .unwrap();

            assert_eq!(installation.executable_path, executable);
            assert_eq!(
                *phases.borrow(),
                vec!["CheckingRemote".to_string(), "Reusing".to_string()]
            );
            assert!(
                !events.borrow().iter().any(|event| event == "acquire"),
                "remote reuse must not acquire the local artifact: {:?}",
                events.borrow()
            );
        });
    }

    #[test]
    fn does_not_reuse_bundle_when_a_manifest_file_is_missing() {
        smol::block_on(async {
            let release = release();
            let events = Rc::new(RefCell::new(Vec::new()));
            let target = target_name(release.target).unwrap();
            let destination = PathBuf::from("/remote/flint/agents/pi/1.2.3").join(&target);
            let executable = destination.join(release.executable_name);
            let receipt = serde_json::json!({
                "agent_id": "pi",
                "version": release.version,
                "target": target,
                "executable_name": release.executable_name,
                "executable_path": executable,
                "executable_sha256": release.executable_sha256,
                "source_sha256": release.source_sha256,
                "files": [
                    { "relative_path": "agent", "sha256": DIGEST },
                    { "relative_path": "package.json", "sha256": DIGEST },
                ],
            })
            .to_string();
            let files = Rc::new(RefCell::new(HashMap::from([
                (executable, DIGEST.to_string()),
                (destination.join("receipt.json"), receipt),
            ])));
            let provisioner = ManagedAgentProvisioner::new(
                FakeArtifacts {
                    events: events.clone(),
                },
                FakeRemoteHost {
                    events: events.clone(),
                    files,
                    paths: Rc::new(RefCell::new(HashSet::from([destination]))),
                    fail_staging_commit: Rc::new(Cell::new(false)),
                },
            );

            provisioner.install("pi", &release).await.unwrap();

            assert!(events.borrow().iter().any(|event| event == "acquire"));
        });
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
    fn bundle_install_uploads_adjacent_files_before_version_check() {
        smol::block_on(async {
            let bundle = tempfile::tempdir().expect("bundle fixture directory");
            std::fs::write(bundle.path().join("agent"), b"agent-bytes")
                .expect("write executable fixture");
            std::fs::write(bundle.path().join("package.json"), b"{}")
                .expect("write package fixture");
            let events = Rc::new(RefCell::new(Vec::new()));
            let provisioner = ManagedAgentProvisioner::new(
                FakeBundleArtifacts {
                    root: bundle.path().to_path_buf(),
                },
                FakeRemoteHost {
                    events: events.clone(),
                    files: Rc::new(RefCell::new(HashMap::new())),
                    paths: Rc::new(RefCell::new(HashSet::new())),
                    fail_staging_commit: Rc::new(Cell::new(false)),
                },
            );
            let mut release = release();
            release.executable_sha256 =
                Box::leak(hex::encode(sha2::Sha256::digest(b"agent-bytes")).into_boxed_str());

            provisioner
                .install("codex", &release)
                .await
                .expect("bundle installation should succeed");

            let events = events.borrow();
            assert!(events.iter().any(|event| event.ends_with("/agent")));
            assert!(events.iter().any(|event| event.ends_with("/package.json")));
            let last_upload = events
                .iter()
                .rposition(|event| event.starts_with("upload:"))
                .expect("bundle files should upload");
            let version = events
                .iter()
                .position(|event| event.starts_with("version:"))
                .expect("version check should run");
            assert!(last_upload < version);
        });
    }

    #[test]
    fn bundle_install_rejects_an_executable_changed_after_verification() {
        smol::block_on(async {
            let bundle = tempfile::tempdir().expect("bundle fixture directory");
            std::fs::write(bundle.path().join("agent"), b"changed-agent")
                .expect("write executable fixture");
            let provisioner = ManagedAgentProvisioner::new(
                FakeBundleArtifacts {
                    root: bundle.path().to_path_buf(),
                },
                FakeRemoteHost {
                    events: Rc::new(RefCell::new(Vec::new())),
                    files: Rc::new(RefCell::new(HashMap::new())),
                    paths: Rc::new(RefCell::new(HashSet::new())),
                    fail_staging_commit: Rc::new(Cell::new(false)),
                },
            );

            let error = provisioner
                .install("codex", &release())
                .await
                .expect_err("changed executable should be rejected");

            assert!(error.to_string().contains("executable digest"));
        });
    }

    #[test]
    fn reports_remote_installation_phases_in_order() {
        smol::block_on(async {
            let events = Rc::new(RefCell::new(Vec::new()));
            let phases = Rc::new(RefCell::new(Vec::new()));
            let provisioner = ManagedAgentProvisioner::new(
                FakeArtifacts {
                    events: events.clone(),
                },
                FakeRemoteHost {
                    events,
                    files: Rc::new(RefCell::new(HashMap::new())),
                    paths: Rc::new(RefCell::new(HashSet::new())),
                    fail_staging_commit: Rc::new(Cell::new(false)),
                },
            );

            provisioner
                .install_with_progress("codex", &release(), {
                    let phases = phases.clone();
                    move |phase| phases.borrow_mut().push(phase)
                })
                .await
                .unwrap();

            assert_eq!(
                *phases.borrow(),
                vec![
                    ManagedAgentInstallPhase::CheckingRemote,
                    ManagedAgentInstallPhase::Uploading,
                    ManagedAgentInstallPhase::VerifyingRemote,
                    ManagedAgentInstallPhase::Installing,
                ]
            );
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
