use crate::agent_release::{AgentArtifactFormat, AgentRelease, source_is_official};
use anyhow::{Context as _, Result};
use async_compression::futures::bufread::GzipDecoder;
use futures::{AsyncReadExt as _, AsyncWriteExt as _};
use http_client::{AsyncBody, HttpClient};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentArtifactDownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub complete: bool,
}

pub struct AgentArtifactCache {
    root: PathBuf,
    http_client: Arc<dyn HttpClient>,
    acquisitions: futures::lock::Mutex<HashMap<String, Arc<futures::lock::Mutex<()>>>>,
}

#[derive(Default)]
struct DownloadProgressThrottle {
    last_reported_bytes: Option<u64>,
    last_reported_percentage: Option<u64>,
}

impl DownloadProgressThrottle {
    fn should_report(&mut self, downloaded_bytes: u64, total_bytes: Option<u64>) -> bool {
        const UNKNOWN_TOTAL_REPORT_INTERVAL: u64 = 1024 * 1024;

        let should_report = if let Some(total_bytes) = total_bytes {
            let percentage = if total_bytes == 0 {
                100
            } else {
                (((downloaded_bytes as u128 * 100) / total_bytes as u128).min(100)) as u64
            };
            self.last_reported_percentage != Some(percentage)
        } else {
            self.last_reported_bytes.is_none_or(|last_reported_bytes| {
                downloaded_bytes.saturating_sub(last_reported_bytes)
                    >= UNKNOWN_TOTAL_REPORT_INTERVAL
            })
        };

        if should_report {
            self.last_reported_bytes = Some(downloaded_bytes);
            self.last_reported_percentage = total_bytes.map(|total_bytes| {
                if total_bytes == 0 {
                    100
                } else {
                    (((downloaded_bytes as u128 * 100) / total_bytes as u128).min(100)) as u64
                }
            });
        }
        should_report
    }
}

impl AgentArtifactCache {
    pub fn for_app(http_client: Arc<dyn HttpClient>) -> Self {
        Self::new(paths::agent_artifact_cache_dir().to_path_buf(), http_client)
    }

    pub fn new(root: PathBuf, http_client: Arc<dyn HttpClient>) -> Self {
        Self {
            root,
            http_client,
            acquisitions: futures::lock::Mutex::new(HashMap::new()),
        }
    }

    pub async fn acquire(
        &self,
        release: &AgentRelease,
        official_source_prefixes: &[&str],
    ) -> Result<PathBuf> {
        self.acquire_with_progress(release, official_source_prefixes, |_| {})
            .await
    }

    pub async fn acquire_with_progress(
        &self,
        release: &AgentRelease,
        official_source_prefixes: &[&str],
        mut report_progress: impl FnMut(AgentArtifactDownloadProgress),
    ) -> Result<PathBuf> {
        if !source_is_official(release.source_url, official_source_prefixes) {
            anyhow::bail!("artifact URL is outside the official HTTPS source policy");
        }
        let acquisition = {
            let mut acquisitions = self.acquisitions.lock().await;
            acquisitions
                .entry(release.source_sha256.to_string())
                .or_insert_with(|| Arc::new(futures::lock::Mutex::new(())))
                .clone()
        };
        let _acquisition = acquisition.lock().await;
        self.acquire_exclusive(release, official_source_prefixes, &mut report_progress)
            .await
    }

    pub async fn release_is_cached(&self, release: &AgentRelease) -> Result<bool> {
        if matches!(
            release.artifact,
            AgentArtifactFormat::TarGzBundle { .. } | AgentArtifactFormat::ZipBundle { .. }
        ) {
            let source = self.root.join("sources").join(release.source_sha256);
            return file_has_digest(&source, release.source_sha256).await;
        }
        let executable = self.executable_destination(release)?;
        if file_has_digest(&executable, release.executable_sha256).await? {
            return Ok(true);
        }
        let source = self.root.join("sources").join(release.source_sha256);
        file_has_digest(&source, release.source_sha256).await
    }

    async fn acquire_exclusive(
        &self,
        release: &AgentRelease,
        official_source_prefixes: &[&str],
        report_progress: &mut impl FnMut(AgentArtifactDownloadProgress),
    ) -> Result<PathBuf> {
        let destination = self.executable_destination(release)?;
        let is_bundle = matches!(
            release.artifact,
            AgentArtifactFormat::TarGzBundle { .. } | AgentArtifactFormat::ZipBundle { .. }
        );
        if !is_bundle && file_has_digest(&destination, release.executable_sha256).await? {
            return Ok(destination);
        }
        if let AgentArtifactFormat::TarGzBundle { root_path, .. }
        | AgentArtifactFormat::ZipBundle { root_path, .. } = release.artifact
        {
            let bundle_root = self.bundle_destination(release, root_path)?;
            remove_directory_if_exists(&bundle_root).await?;
        } else {
            remove_file_if_exists(&destination).await?;
        }

        let source = self.root.join("sources").join(release.source_sha256);
        if !file_has_digest(&source, release.source_sha256).await? {
            self.download(release, official_source_prefixes, &source, report_progress)
                .await?;
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
            let bundle_root = self.bundle_destination(release, root_path)?;
            let bundle_parent = bundle_root
                .parent()
                .context("bundle cache destination has no parent")?;
            smol::fs::create_dir_all(bundle_parent).await?;
            let partial_bundle = bundle_parent.join(format!(
                ".{}.{}.partial",
                release.executable_name,
                uuid::Uuid::new_v4()
            ));
            let result = match release.artifact {
                AgentArtifactFormat::TarGzBundle { .. } => {
                    extract_verified_tar_bundle(
                        &source,
                        &partial_bundle,
                        root_path,
                        executable_path,
                        release.executable_sha256,
                    )
                    .await
                }
                AgentArtifactFormat::ZipBundle { .. } => {
                    extract_verified_zip_bundle(
                        &source,
                        &partial_bundle,
                        root_path,
                        executable_path,
                        release.executable_sha256,
                    )
                    .await
                }
                _ => Err(anyhow::anyhow!(
                    "non-bundle artifact reached bundle extraction"
                )),
            };
            if let Err(error) = result {
                remove_directory_if_exists(&partial_bundle).await?;
                return Err(error);
            }
            smol::fs::rename(&partial_bundle, &bundle_root)
                .await
                .context("failed to commit cached agent bundle")?;
            return Ok(destination);
        }
        let parent = destination
            .parent()
            .context("artifact cache destination has no parent")?;
        smol::fs::create_dir_all(parent).await?;
        let partial = parent.join(format!(
            ".{}.{}.partial",
            release.executable_name,
            uuid::Uuid::new_v4()
        ));
        let normalization_result = match release.artifact {
            AgentArtifactFormat::Raw => {
                if release.source_sha256 != release.executable_sha256 {
                    Err(anyhow::anyhow!(
                        "raw artifact source and executable digests differ"
                    ))
                } else {
                    smol::fs::copy(&source, &partial)
                        .await
                        .context("failed to normalize cached executable")
                        .map(|_| ())
                }
            }
            AgentArtifactFormat::TarGz { executable_path } => {
                extract_verified_tar_executable(&source, &partial, executable_path).await
            }
            AgentArtifactFormat::TarGzBundle { .. } | AgentArtifactFormat::ZipBundle { .. } => Err(
                anyhow::anyhow!("bundle artifact reached executable normalization"),
            ),
        };
        if let Err(error) = normalization_result {
            remove_partial(&partial).await;
            return Err(error);
        }
        if !file_has_digest(&partial, release.executable_sha256).await? {
            remove_partial(&partial).await;
            anyhow::bail!("normalized executable failed installed-byte verification");
        }
        smol::fs::rename(&partial, &destination)
            .await
            .context("failed to commit cached executable")?;
        Ok(destination)
    }

    fn executable_destination(&self, release: &AgentRelease) -> Result<PathBuf> {
        match release.artifact {
            AgentArtifactFormat::TarGzBundle {
                root_path,
                executable_path,
            }
            | AgentArtifactFormat::ZipBundle {
                root_path,
                executable_path,
            } => {
                let relative_executable = strip_bundle_root(executable_path, root_path)?;
                Ok(self
                    .bundle_destination(release, root_path)?
                    .join(relative_executable))
            }
            AgentArtifactFormat::Raw | AgentArtifactFormat::TarGz { .. } => Ok(self
                .root
                .join("executables")
                .join(release.executable_sha256)
                .join(release.executable_name)),
        }
    }

    fn bundle_destination(&self, release: &AgentRelease, root_path: &str) -> Result<PathBuf> {
        if !root_path.is_empty() {
            validate_archive_path(Path::new(root_path))?;
        }
        Ok(self
            .root
            .join("bundles")
            .join(release.source_sha256)
            .join(if root_path.is_empty() {
                "root"
            } else {
                root_path
            }))
    }

    async fn download(
        &self,
        release: &AgentRelease,
        official_source_prefixes: &[&str],
        destination: &PathBuf,
        report_progress: &mut impl FnMut(AgentArtifactDownloadProgress),
    ) -> Result<()> {
        let response = self
            .get_with_official_redirects(release.source_url, official_source_prefixes)
            .await
            .with_context(|| format!("failed to download {}", release.executable_name))?;
        if !response.status().is_success() {
            anyhow::bail!(
                "artifact download returned HTTP {} for {}",
                response.status(),
                release.executable_name
            );
        }
        let total_bytes = response
            .headers()
            .get(http_client::http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if total_bytes.is_some_and(|length| length > MAX_ARTIFACT_BYTES) {
            anyhow::bail!("artifact exceeds the maximum download size");
        }

        let parent = destination
            .parent()
            .context("artifact cache source has no parent")?;
        smol::fs::create_dir_all(parent).await?;
        let partial = parent.join(format!(
            ".{}.{}.partial",
            release.source_sha256,
            uuid::Uuid::new_v4()
        ));
        let result = write_verified_response(
            response.into_body(),
            &partial,
            release.source_sha256,
            total_bytes,
            report_progress,
        )
        .await;
        if let Err(error) = result {
            remove_partial(&partial).await;
            return Err(error);
        }
        if destination.exists() {
            remove_partial(&partial).await;
        } else {
            smol::fs::rename(&partial, destination)
                .await
                .context("failed to commit downloaded artifact")?;
        }
        Ok(())
    }

    async fn get_with_official_redirects(
        &self,
        source_url: &str,
        official_source_prefixes: &[&str],
    ) -> Result<http_client::Response<AsyncBody>> {
        const MAX_REDIRECTS: usize = 5;

        let mut current = url::Url::parse(source_url)?;
        for redirect_count in 0..=MAX_REDIRECTS {
            let response = self
                .http_client
                .get(current.as_str(), AsyncBody::empty(), false)
                .await?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if redirect_count == MAX_REDIRECTS {
                anyhow::bail!("artifact download exceeded the redirect limit");
            }
            let location = response
                .headers()
                .get(http_client::http::header::LOCATION)
                .context("artifact redirect has no Location header")?
                .to_str()
                .context("artifact redirect Location is not valid text")?;
            let next = current.join(location)?;
            if !source_is_official(next.as_str(), official_source_prefixes) {
                anyhow::bail!("artifact redirect is outside the official HTTPS source policy");
            }
            current = next;
        }
        anyhow::bail!("artifact redirect loop ended unexpectedly")
    }
}

async fn extract_verified_tar_executable(
    source: &PathBuf,
    destination: &PathBuf,
    executable_path: &str,
) -> Result<()> {
    use futures::TryStreamExt as _;

    let source = smol::fs::File::open(source).await?;
    let decompressed = GzipDecoder::new(futures::io::BufReader::new(source));
    let mut entries = async_tar::Archive::new(decompressed).entries()?;
    let mut found = false;
    while let Some(entry) = entries.try_next().await? {
        let path = entry.path()?.into_owned();
        if path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            anyhow::bail!("agent archive contains an unsafe path");
        }
        if path.to_string_lossy() == executable_path {
            if found {
                anyhow::bail!("agent archive contains duplicate executable entries");
            }
            if !entry.header().entry_type().is_file() {
                anyhow::bail!("agent archive executable is not a regular file");
            }
            let mut output = smol::fs::File::create(destination).await?;
            let mut bounded_entry = entry.take(MAX_ARTIFACT_BYTES + 1);
            let copied = futures::io::copy(&mut bounded_entry, &mut output).await?;
            if copied > MAX_ARTIFACT_BYTES {
                anyhow::bail!("normalized executable exceeds the maximum size");
            }
            output.flush().await?;
            output.sync_all().await?;
            found = true;
        }
    }
    if !found {
        anyhow::bail!("agent archive does not contain the pinned executable path");
    }
    Ok(())
}

async fn extract_verified_tar_bundle(
    source: &PathBuf,
    destination: &Path,
    root_path: &str,
    executable_path: &str,
    executable_sha256: &str,
) -> Result<()> {
    use futures::TryStreamExt as _;

    validate_archive_path(Path::new(root_path))?;
    validate_archive_path(Path::new(executable_path))?;
    smol::fs::create_dir_all(destination).await?;
    let source = smol::fs::File::open(source).await?;
    let decompressed = GzipDecoder::new(futures::io::BufReader::new(source));
    let mut entries = async_tar::Archive::new(decompressed).entries()?;
    let mut seen = HashSet::new();
    let mut extracted_bytes = 0_u64;
    while let Some(entry) = entries.try_next().await? {
        let archive_path = entry.path()?;
        let path = PathBuf::from(
            archive_path
                .to_str()
                .context("agent bundle contains a non-UTF-8 path")?,
        );
        validate_archive_path(&path)?;
        let relative_path = path
            .strip_prefix(root_path)
            .context("agent bundle contains an entry outside its root")?;
        if relative_path.as_os_str().is_empty() {
            continue;
        }
        if !seen.insert(relative_path.to_path_buf()) {
            anyhow::bail!("agent bundle contains duplicate entries");
        }
        let output_path = destination.join(relative_path);
        if entry.header().entry_type().is_dir() {
            smol::fs::create_dir_all(&output_path).await?;
            continue;
        }
        if !entry.header().entry_type().is_file() {
            anyhow::bail!("agent bundle contains a non-regular file");
        }
        let parent = output_path
            .parent()
            .context("agent bundle entry has no parent")?;
        smol::fs::create_dir_all(parent).await?;
        let mut output = smol::fs::File::create(&output_path).await?;
        let remaining = MAX_ARTIFACT_BYTES
            .checked_sub(extracted_bytes)
            .context("agent bundle exceeds the maximum size")?;
        let mut bounded_entry = entry.take(remaining + 1);
        let copied = futures::io::copy(&mut bounded_entry, &mut output).await?;
        extracted_bytes = extracted_bytes
            .checked_add(copied)
            .context("agent bundle size overflowed")?;
        if extracted_bytes > MAX_ARTIFACT_BYTES {
            anyhow::bail!("agent bundle exceeds the maximum size");
        }
        output.flush().await?;
        output.sync_all().await?;
    }
    let relative_executable = Path::new(executable_path)
        .strip_prefix(root_path)
        .context("bundle executable is outside its root")?;
    if !file_has_digest(&destination.join(relative_executable), executable_sha256).await? {
        anyhow::bail!("normalized executable failed installed-byte verification");
    }
    Ok(())
}

async fn extract_verified_zip_bundle(
    source: &PathBuf,
    destination: &Path,
    root_path: &str,
    executable_path: &str,
    executable_sha256: &str,
) -> Result<()> {
    use async_zip::base::read::seek::ZipFileReader;

    validate_archive_path(Path::new(executable_path))?;
    if !root_path.is_empty() {
        validate_archive_path(Path::new(root_path))?;
    }
    let source_file = smol::fs::File::open(source).await?;
    let reader = ZipFileReader::new(futures::io::BufReader::new(source_file)).await?;
    let mut seen = HashSet::new();
    let mut total_bytes = 0_u64;
    for entry in reader.file().entries() {
        let filename = entry.filename().as_str()?;
        let path = Path::new(filename);
        validate_archive_path(path)?;
        let relative_path = if root_path.is_empty() {
            path
        } else {
            path.strip_prefix(root_path)
                .context("agent bundle contains an entry outside its root")?
        };
        if relative_path.as_os_str().is_empty() {
            continue;
        }
        if !seen.insert(relative_path.to_path_buf()) {
            anyhow::bail!("agent bundle contains duplicate entries");
        }
        total_bytes = total_bytes
            .checked_add(entry.uncompressed_size())
            .context("agent bundle size overflowed")?;
        if total_bytes > MAX_ARTIFACT_BYTES {
            anyhow::bail!("agent bundle exceeds the maximum size");
        }
        if entry
            .unix_permissions()
            .is_some_and(|permissions| permissions & 0o170000 == 0o120000)
        {
            anyhow::bail!("agent bundle contains a symbolic link");
        }
    }
    smol::fs::create_dir_all(destination).await?;
    let source_file = smol::fs::File::open(source).await?;
    util::archive::extract_zip(destination, source_file).await?;
    let relative_executable = strip_bundle_root(executable_path, root_path)?;
    if !file_has_digest(&destination.join(relative_executable), executable_sha256).await? {
        anyhow::bail!("normalized executable failed installed-byte verification");
    }
    Ok(())
}

fn strip_bundle_root<'a>(path: &'a str, root_path: &str) -> Result<&'a Path> {
    let path = Path::new(path);
    if root_path.is_empty() {
        return Ok(path);
    }
    path.strip_prefix(root_path)
        .context("bundle executable is outside its root")
}

fn validate_archive_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        anyhow::bail!("agent archive contains an unsafe path");
    }
    Ok(())
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match smol::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove corrupt cache entry {}", path.display())),
    }
}

async fn remove_directory_if_exists(path: &Path) -> Result<()> {
    match smol::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove corrupt cache entry {}", path.display())),
    }
}

async fn write_verified_response(
    mut body: AsyncBody,
    partial: &PathBuf,
    expected_digest: &str,
    total_bytes: Option<u64>,
    report_progress: &mut impl FnMut(AgentArtifactDownloadProgress),
) -> Result<()> {
    let mut file = smol::fs::File::create(partial).await?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0; 64 * 1024];
    let mut progress_throttle = DownloadProgressThrottle::default();
    loop {
        let bytes_read = body.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        total = total
            .checked_add(bytes_read as u64)
            .context("artifact download size overflowed")?;
        if total > MAX_ARTIFACT_BYTES {
            anyhow::bail!("artifact exceeds the maximum download size");
        }
        file.write_all(&buffer[..bytes_read]).await?;
        hasher.update(&buffer[..bytes_read]);
        if progress_throttle.should_report(total, total_bytes) {
            report_progress(AgentArtifactDownloadProgress {
                downloaded_bytes: total,
                total_bytes,
                complete: false,
            });
        }
    }
    if let Some(expected_bytes) = total_bytes
        && total != expected_bytes
    {
        anyhow::bail!(
            "artifact Content-Length was {expected_bytes} bytes but the response contained {total} bytes"
        );
    }
    report_progress(AgentArtifactDownloadProgress {
        downloaded_bytes: total,
        total_bytes,
        complete: true,
    });
    file.flush().await?;
    file.sync_all().await?;
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_digest {
        anyhow::bail!("downloaded artifact failed SHA-256 verification");
    }
    Ok(())
}

async fn file_has_digest(path: &PathBuf, expected_digest: &str) -> Result<bool> {
    let mut file = match smol::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to open {}", path.display()));
        }
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(format!("{:x}", hasher.finalize()) == expected_digest)
}

async fn remove_partial(path: &PathBuf) {
    if let Err(error) = smol::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!(
            "failed to remove partial artifact {}: {error}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_release::{
        AgentArtifactFormat, AgentRelease, AgentSourceVerification, AgentVersionMatcher,
    };
    use http_client::{AsyncBody, FakeHttpClient, Response};
    use remote::{RemoteArch, RemoteLibc, RemoteOs, RemotePlatform};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    const DIGEST: &str = "2c18a5823d3012a1dd7bee6409d4d05b98dfa47733ac4c22e8161445523c10f0";

    async fn tar_gz(path: &str, content: &[u8]) -> Vec<u8> {
        use async_compression::futures::write::GzipEncoder;
        use futures::io::Cursor;

        let mut archive = async_tar::Builder::new(Cursor::new(Vec::new()));
        let mut header = async_tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, path, Cursor::new(content))
            .await
            .expect("tar fixture entry should be appended");
        archive.finish().await.expect("tar fixture should finish");
        let tar_bytes = archive
            .into_inner()
            .await
            .expect("tar fixture should return its buffer")
            .into_inner();
        let mut encoder = GzipEncoder::new(Cursor::new(Vec::new()));
        encoder
            .write_all(&tar_bytes)
            .await
            .expect("tar fixture should compress");
        encoder.close().await.expect("gzip fixture should finish");
        encoder.into_inner().into_inner()
    }

    async fn tar_gz_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use async_compression::futures::write::GzipEncoder;
        use futures::io::Cursor;

        let mut archive = async_tar::Builder::new(Cursor::new(Vec::new()));
        for (path, content) in entries {
            let mut header = async_tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, path, Cursor::new(*content))
                .await
                .expect("tar fixture entry should be appended");
        }
        archive.finish().await.expect("tar fixture should finish");
        let tar_bytes = archive
            .into_inner()
            .await
            .expect("tar fixture should return its buffer")
            .into_inner();
        let mut encoder = GzipEncoder::new(Cursor::new(Vec::new()));
        encoder
            .write_all(&tar_bytes)
            .await
            .expect("tar fixture should compress");
        encoder.close().await.expect("gzip fixture should finish");
        encoder.into_inner().into_inner()
    }

    async fn zip_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use async_zip::{Compression, ZipEntryBuilder, base::write::ZipFileWriter};
        use futures::io::Cursor;

        let mut buffer = Cursor::new(Vec::new());
        let mut writer = ZipFileWriter::new(&mut buffer);
        for (path, content) in entries {
            let entry = ZipEntryBuilder::new((*path).into(), Compression::Stored);
            writer
                .write_entry_whole(entry, content)
                .await
                .expect("zip fixture entry should be written");
        }
        writer.close().await.expect("zip fixture should finish");
        buffer.into_inner()
    }

    fn fixture_release(source_url: &'static str) -> AgentRelease {
        AgentRelease {
            version: "1.2.3",
            target: RemotePlatform {
                os: RemoteOs::Linux,
                arch: RemoteArch::X86_64,
                libc: Some(RemoteLibc::Glibc),
            },
            source_url,
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
    fn verified_cache_downloads_once_and_reuses_the_content_addressed_entry() {
        smol::block_on(async {
            let request_count = Arc::new(AtomicUsize::new(0));
            let http_client = FakeHttpClient::create({
                let request_count = request_count.clone();
                move |_| {
                    request_count.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok(Response::builder()
                            .status(200)
                            .body(AsyncBody::from("agent-bytes"))
                            .expect("fixture response should build"))
                    }
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let release = fixture_release("https://downloads.example.test/agent");

            let first = cache
                .acquire(&release, &["https://downloads.example.test/"])
                .await
                .expect("first acquisition should succeed");
            let second = cache
                .acquire(&release, &["https://downloads.example.test/"])
                .await
                .expect("cached acquisition should succeed");

            assert_eq!(first, second);
            assert_eq!(request_count.load(Ordering::SeqCst), 1);
            assert_eq!(
                smol::fs::read(first)
                    .await
                    .expect("cached artifact should be readable"),
                b"agent-bytes"
            );
        });
    }

    #[test]
    fn tar_bundle_keeps_files_adjacent_to_the_executable() {
        smol::block_on(async {
            let archive = tar_gz_entries(&[
                ("pi/pi", b"agent-bytes"),
                ("pi/package.json", br#"{"version":"1.2.3"}"#),
            ])
            .await;
            let source_digest = format!("{:x}", sha2::Sha256::digest(&archive));
            let executable_digest = format!("{:x}", sha2::Sha256::digest(b"agent-bytes"));
            let http_client = FakeHttpClient::create(move |_| {
                let archive = archive.clone();
                async move {
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(archive))
                        .expect("fixture response should build"))
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let mut release = fixture_release("https://downloads.example.test/pi.tar.gz");
            release.source_sha256 = Box::leak(source_digest.into_boxed_str());
            release.executable_sha256 = Box::leak(executable_digest.into_boxed_str());
            release.executable_name = "pi";
            release.artifact = AgentArtifactFormat::TarGzBundle {
                root_path: "pi",
                executable_path: "pi/pi",
            };

            let executable = cache
                .acquire(&release, &["https://downloads.example.test/"])
                .await
                .expect("bundle acquisition should succeed");

            assert_eq!(
                smol::fs::read(
                    executable
                        .parent()
                        .expect("bundle root")
                        .join("package.json")
                )
                .await
                .expect("package metadata should be cached"),
                br#"{"version":"1.2.3"}"#
            );
        });
    }

    #[test]
    fn bundle_acquisition_restores_missing_adjacent_files_from_the_verified_source() {
        smol::block_on(async {
            let archive = tar_gz_entries(&[
                ("pi/pi", b"agent-bytes"),
                ("pi/package.json", br#"{"version":"1.2.3"}"#),
            ])
            .await;
            let source_digest = format!("{:x}", sha2::Sha256::digest(&archive));
            let executable_digest = format!("{:x}", sha2::Sha256::digest(b"agent-bytes"));
            let http_client = FakeHttpClient::create(move |_| {
                let archive = archive.clone();
                async move {
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(archive))
                        .expect("fixture response should build"))
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let mut release = fixture_release("https://downloads.example.test/pi.tar.gz");
            release.source_sha256 = Box::leak(source_digest.into_boxed_str());
            release.executable_sha256 = Box::leak(executable_digest.into_boxed_str());
            release.executable_name = "pi";
            release.artifact = AgentArtifactFormat::TarGzBundle {
                root_path: "pi",
                executable_path: "pi/pi",
            };

            let executable = cache
                .acquire(&release, &["https://downloads.example.test/"])
                .await
                .expect("first bundle acquisition should succeed");
            let package = executable
                .parent()
                .expect("bundle root")
                .join("package.json");
            smol::fs::remove_file(&package)
                .await
                .expect("fixture package should be removed");

            cache
                .acquire(&release, &["https://downloads.example.test/"])
                .await
                .expect("second bundle acquisition should restore the bundle");

            assert!(package.exists());
        });
    }

    #[test]
    fn zip_bundle_keeps_files_adjacent_to_the_executable() {
        smol::block_on(async {
            let archive = zip_entries(&[
                ("pi.exe", b"agent-bytes"),
                ("package.json", br#"{"version":"1.2.3"}"#),
            ])
            .await;
            let source_digest = format!("{:x}", sha2::Sha256::digest(&archive));
            let executable_digest = format!("{:x}", sha2::Sha256::digest(b"agent-bytes"));
            let http_client = FakeHttpClient::create(move |_| {
                let archive = archive.clone();
                async move {
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(archive))
                        .expect("fixture response should build"))
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let mut release = fixture_release("https://downloads.example.test/pi.zip");
            release.source_sha256 = Box::leak(source_digest.into_boxed_str());
            release.executable_sha256 = Box::leak(executable_digest.into_boxed_str());
            release.executable_name = "pi.exe";
            release.artifact = AgentArtifactFormat::ZipBundle {
                root_path: "",
                executable_path: "pi.exe",
            };

            let executable = cache
                .acquire(&release, &["https://downloads.example.test/"])
                .await
                .expect("bundle acquisition should succeed");

            assert_eq!(
                smol::fs::read(
                    executable
                        .parent()
                        .expect("bundle root")
                        .join("package.json")
                )
                .await
                .expect("package metadata should be cached"),
                br#"{"version":"1.2.3"}"#
            );
        });
    }

    #[test]
    fn cache_query_does_not_start_an_http_request() {
        smol::block_on(async {
            let request_count = Arc::new(AtomicUsize::new(0));
            let http_client = FakeHttpClient::create({
                let request_count = request_count.clone();
                move |_| {
                    request_count.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok(Response::builder()
                            .status(200)
                            .body(AsyncBody::from("agent-bytes"))
                            .expect("fixture response should build"))
                    }
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let release = fixture_release("https://downloads.example.test/agent");

            assert!(!cache.release_is_cached(&release).await.unwrap());
            assert_eq!(request_count.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn cache_query_accepts_a_verified_executable_when_the_source_was_removed() {
        smol::block_on(async {
            let request_count = Arc::new(AtomicUsize::new(0));
            let http_client = FakeHttpClient::create({
                let request_count = request_count.clone();
                move |_| {
                    request_count.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok(Response::builder()
                            .status(200)
                            .body(AsyncBody::from("agent-bytes"))
                            .expect("fixture response should build"))
                    }
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let release = fixture_release("https://downloads.example.test/agent");
            cache
                .acquire(&release, &["https://downloads.example.test/"])
                .await
                .expect("artifact should be cached");
            smol::fs::remove_file(directory.path().join("sources").join(release.source_sha256))
                .await
                .expect("source fixture should be removed");

            assert!(cache.release_is_cached(&release).await.unwrap());
            assert_eq!(request_count.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn reports_known_download_length_and_exact_final_byte_count() {
        smol::block_on(async {
            let http_client = FakeHttpClient::create(|_| async {
                Ok(Response::builder()
                    .status(200)
                    .header(http_client::http::header::CONTENT_LENGTH, "11")
                    .body(AsyncBody::from("agent-bytes"))
                    .expect("fixture response should build"))
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let release = fixture_release("https://downloads.example.test/agent");
            let progress = std::cell::RefCell::new(Vec::new());

            cache
                .acquire_with_progress(&release, &["https://downloads.example.test/"], |update| {
                    progress.borrow_mut().push(update)
                })
                .await
                .expect("download should succeed");

            assert_eq!(
                progress.into_inner().last(),
                Some(&AgentArtifactDownloadProgress {
                    downloaded_bytes: 11,
                    total_bytes: Some(11),
                    complete: true,
                })
            );
        });
    }

    #[test]
    fn rejects_a_response_shorter_than_its_declared_length() {
        smol::block_on(async {
            let http_client = FakeHttpClient::create(|_| async {
                Ok(Response::builder()
                    .status(200)
                    .header(http_client::http::header::CONTENT_LENGTH, "12")
                    .body(AsyncBody::from("agent-bytes"))
                    .expect("fixture response should build"))
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let release = fixture_release("https://downloads.example.test/agent");

            let error = cache
                .acquire(&release, &["https://downloads.example.test/"])
                .await
                .expect_err("short response should be rejected");

            assert!(error.to_string().contains("Content-Length"));
            assert!(!cache.release_is_cached(&release).await.unwrap());
        });
    }

    #[test]
    fn reports_transferred_bytes_without_inventing_an_unknown_total() {
        smol::block_on(async {
            let http_client = FakeHttpClient::create(|_| async {
                Ok(Response::builder()
                    .status(200)
                    .body(AsyncBody::from("agent-bytes"))
                    .expect("fixture response should build"))
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let release = fixture_release("https://downloads.example.test/agent");
            let progress = std::cell::RefCell::new(Vec::new());

            cache
                .acquire_with_progress(&release, &["https://downloads.example.test/"], |update| {
                    progress.borrow_mut().push(update)
                })
                .await
                .expect("download should succeed");

            assert_eq!(
                progress.into_inner().last(),
                Some(&AgentArtifactDownloadProgress {
                    downloaded_bytes: 11,
                    total_bytes: None,
                    complete: true,
                })
            );
        });
    }

    #[test]
    fn throttles_known_progress_to_percentage_changes() {
        let mut throttle = DownloadProgressThrottle::default();

        assert!(throttle.should_report(1, Some(1_000)));
        assert!(!throttle.should_report(9, Some(1_000)));
        assert!(throttle.should_report(10, Some(1_000)));
        assert!(!throttle.should_report(19, Some(1_000)));
        assert!(throttle.should_report(20, Some(1_000)));
    }

    #[test]
    fn throttles_unknown_progress_by_transferred_bytes() {
        let mut throttle = DownloadProgressThrottle::default();

        assert!(throttle.should_report(64 * 1024, None));
        assert!(!throttle.should_report(512 * 1024, None));
        assert!(throttle.should_report(1_088 * 1024, None));
    }

    #[test]
    fn normalizes_only_the_pinned_tar_executable() {
        smol::block_on(async {
            let archive = tar_gz("codex-target", b"agent-bytes").await;
            let source_digest: &'static str =
                Box::leak(format!("{:x}", Sha256::digest(&archive)).into_boxed_str());
            let http_client = FakeHttpClient::create(move |_| {
                let archive = archive.clone();
                async move {
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(archive))
                        .expect("fixture response should build"))
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let mut release = fixture_release("https://downloads.example.test/codex.tar.gz");
            release.source_sha256 = source_digest;
            release.artifact = AgentArtifactFormat::TarGz {
                executable_path: "codex-target",
            };

            let executable = cache
                .acquire(&release, &["https://downloads.example.test/"])
                .await
                .expect("pinned archive executable should normalize");

            assert_eq!(
                smol::fs::read(executable)
                    .await
                    .expect("normalized executable should be readable"),
                b"agent-bytes"
            );
        });
    }

    #[test]
    fn follows_a_redirect_only_when_both_hops_are_official() {
        smol::block_on(async {
            let http_client = FakeHttpClient::create(|request| async move {
                if request.uri().host() == Some("downloads.example.test") {
                    return Ok(Response::builder()
                        .status(302)
                        .header(
                            http_client::http::header::LOCATION,
                            "https://cdn.example.test/agent",
                        )
                        .body(AsyncBody::empty())
                        .expect("redirect response should build"));
                }
                Ok(Response::builder()
                    .status(200)
                    .body(AsyncBody::from("agent-bytes"))
                    .expect("artifact response should build"))
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let release = fixture_release("https://downloads.example.test/agent");

            let path = cache
                .acquire(
                    &release,
                    &[
                        "https://downloads.example.test/",
                        "https://cdn.example.test/",
                    ],
                )
                .await
                .expect("approved redirect should succeed");

            assert_eq!(
                smol::fs::read(path)
                    .await
                    .expect("redirected artifact should be cached"),
                b"agent-bytes"
            );
        });
    }

    #[test]
    fn concurrent_acquisitions_share_one_download() {
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
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let release = fixture_release("https://downloads.example.test/agent");
            let prefixes = ["https://downloads.example.test/"];

            let release_gate = async {
                download_started_receiver.recv().await?;
                release_download.send(()).await?;
                anyhow::Ok(())
            };
            let (first, second, released) = futures::join!(
                cache.acquire(&release, &prefixes),
                cache.acquire(&release, &prefixes),
                release_gate,
            );
            released.expect("test download should be released");

            assert_eq!(
                first.expect("first acquisition should succeed"),
                second.expect("second acquisition should succeed")
            );
            assert_eq!(request_count.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn corrupted_executable_entry_is_rebuilt_from_the_verified_source() {
        smol::block_on(async {
            let request_count = Arc::new(AtomicUsize::new(0));
            let http_client = FakeHttpClient::create({
                let request_count = request_count.clone();
                move |_| {
                    request_count.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok(Response::builder()
                            .status(200)
                            .body(AsyncBody::from("agent-bytes"))
                            .expect("fixture response should build"))
                    }
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory should be created");
            let cache = AgentArtifactCache::new(directory.path().to_path_buf(), http_client);
            let release = fixture_release("https://downloads.example.test/agent");
            let prefixes = ["https://downloads.example.test/"];
            let path = cache
                .acquire(&release, &prefixes)
                .await
                .expect("first acquisition should succeed");
            smol::fs::write(&path, b"corrupt")
                .await
                .expect("cache fixture should be corrupted");

            let rebuilt = cache
                .acquire(&release, &prefixes)
                .await
                .expect("corrupt executable should be rebuilt");

            assert_eq!(
                smol::fs::read(rebuilt)
                    .await
                    .expect("rebuilt artifact should be readable"),
                b"agent-bytes"
            );
            assert_eq!(request_count.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn production_cache_uses_the_flint_agent_artifact_root() {
        let http_client = FakeHttpClient::with_200_response();

        let cache = AgentArtifactCache::for_app(http_client);

        assert_eq!(cache.root, paths::agent_artifact_cache_dir().to_path_buf());
    }
}
