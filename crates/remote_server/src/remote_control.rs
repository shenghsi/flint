use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use agent_control_protocol::{
    ControlCommand, ControlErrorCode, ControlRequest, ControlResponse, ControlResult,
    ControlSuccess, FRAME_LENGTH_BYTES, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, PROTOCOL_VERSION,
    RemoteControlEnvelope, RemoteTerminalRegistrationId, frame_payload,
};
#[cfg(not(windows))]
use anyhow::bail;
use anyhow::{Context as _, Result};
#[cfg(unix)]
use futures::AsyncWriteExt as _;
use futures::{FutureExt as _, select};
use gpui::AppContext as _;
use parking_lot::Mutex;
use release_channel::RELEASE_CHANNEL;
use rpc::{AnyProtoClient, proto};
use serde::{Deserialize, Serialize};

#[cfg(windows)]
#[path = "remote_control_windows.rs"]
mod windows_control;

const MAX_DISCOVERY_RECORDS: usize = 64;
const MAX_ANCESTRY_DEPTH: usize = 32;
const PENDING_REGISTRATION_LIFETIME: Duration = Duration::from_secs(10 * 60);
const MANAGED_BLOCK_BEGIN_PREFIX: &str = "<!-- Flint managed agent-thread instructions: begin v";
const MANAGED_BLOCK_END: &str = "<!-- Flint managed agent-thread instructions: end -->";
const REMOTE_MANAGED_BLOCK_VERSION: u32 = 4;

#[derive(Debug, Serialize, Deserialize)]
struct DiscoveryRecord {
    endpoint: PathBuf,
    server_process_id: u32,
    protocol_major: u16,
    protocol_minor: u16,
}

struct DiscoverySet {
    records: Vec<DiscoveryRecord>,
    version_mismatch: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "remote-control-transport", rename_all = "kebab-case")]
enum EndpointRequest {
    RegisterTerminal {
        remote_terminal_registration_id: RemoteTerminalRegistrationId,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct EndpointResponse {
    claimed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum EndpointReply {
    Control(ControlResponse),
    Claim(EndpointResponse),
}

struct RegisteredRemoteTerminal {
    root_process_id: u32,
    root_process_start_time: u64,
    working_directory: Option<PathBuf>,
    is_agent_thread: Option<bool>,
    local_registration_verified: bool,
}

struct RemoteTerminalRegistration {
    allocated_at: Instant,
    terminal: Option<RegisteredRemoteTerminal>,
}

type RemoteTerminalRegistrations =
    HashMap<RemoteTerminalRegistrationId, RemoteTerminalRegistration>;

struct RemoteControlState {
    registrations: Arc<Mutex<RemoteTerminalRegistrations>>,
}

fn register_terminal_allocation_handler(
    session: &AnyProtoClient,
    cx: &mut gpui::App,
) -> Arc<Mutex<RemoteTerminalRegistrations>> {
    let registrations = Arc::new(Mutex::new(HashMap::new()));
    let state = cx.new(|_| RemoteControlState {
        registrations: registrations.clone(),
    });
    session.add_request_handler(
        state.downgrade(),
        |state,
         _envelope: rpc::TypedEnvelope<proto::AllocateRemoteTerminalRegistration>,
         mut cx| async move {
            let registration_id = RemoteTerminalRegistrationId(uuid::Uuid::new_v4().to_string());
            state.update(&mut cx, |state, _cx| {
                let mut registrations = state.registrations.lock();
                prune_registrations(&mut registrations);
                registrations.insert(
                    registration_id.clone(),
                    RemoteTerminalRegistration {
                        allocated_at: Instant::now(),
                        terminal: None,
                    },
                );
            });
            Ok(proto::AllocateRemoteTerminalRegistrationResponse {
                registration_id: registration_id.0,
            })
        },
    );
    cx.on_app_quit(move |_cx| {
        let state = state.clone();
        async move {
            drop(state);
        }
    })
    .detach();
    registrations
}

#[derive(Serialize)]
struct ExecutableMarker {
    executable: PathBuf,
}

pub(crate) fn install_command() -> Result<()> {
    let server_executable = std::env::current_exe().context("failed to locate remote server")?;
    install_command_at(&server_executable, &command_directory(), &paths::home_dir())
}

fn install_command_at(
    server_executable: &Path,
    scoped_directory: &Path,
    home: &Path,
) -> Result<()> {
    let parent = server_executable
        .parent()
        .context("remote server executable has no parent directory")?;
    #[cfg(unix)]
    let command = parent.join("flintctl");
    #[cfg(windows)]
    let command = parent.join("flintctl.exe");
    std::fs::create_dir_all(scoped_directory)?;
    #[cfg(unix)]
    let scoped_command = scoped_directory.join("flintctl");
    #[cfg(windows)]
    let scoped_command = scoped_directory.join("flintctl.exe");

    install_command_file(server_executable, &scoped_command)?;

    let marker_path = scoped_directory.join("executable.json");
    std::fs::create_dir_all(
        marker_path
            .parent()
            .context("remote flintctl marker has no parent")?,
    )?;
    let temporary_marker = marker_path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(
        &temporary_marker,
        serde_json::to_vec_pretty(&ExecutableMarker {
            executable: scoped_command,
        })?,
    )?;
    replace_file(&temporary_marker, &marker_path)?;
    synchronize_remote_instructions(home, &marker_path)?;
    install_command_file(server_executable, &command)?;
    Ok(())
}

#[cfg(unix)]
fn install_command_file(server_executable: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    let temporary = target.with_extension(format!("{}.tmp", std::process::id()));
    if let Err(error) = std::fs::remove_file(&temporary)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(error).context("failed to remove stale flintctl link");
    }
    symlink(server_executable, &temporary).context("failed to create flintctl link")?;
    replace_file(&temporary, target).context("failed to replace flintctl link")
}

#[cfg(windows)]
fn install_command_file(server_executable: &Path, target: &Path) -> Result<()> {
    let temporary = target.with_extension(format!("{}.tmp.exe", std::process::id()));
    std::fs::copy(server_executable, &temporary)
        .context("failed to copy remote flintctl executable")?;
    replace_file(&temporary, target).context("failed to install remote flintctl")
}

fn synchronize_remote_instructions(home: &Path, marker_path: &Path) -> Result<()> {
    for (installed_directory, instructions_path) in remote_instruction_locations(home) {
        if !installed_directory.exists() {
            continue;
        }
        synchronize_remote_instructions_file(&instructions_path, marker_path)?;
    }
    Ok(())
}

fn remote_instruction_locations(home: &Path) -> [(PathBuf, PathBuf); 4] {
    [
        (home.join(".codex"), home.join(".codex/AGENTS.md")),
        (home.join(".claude"), home.join(".claude/CLAUDE.md")),
        (
            home.join(".config/opencode"),
            home.join(".config/opencode/AGENTS.md"),
        ),
        (home.join(".pi/agent"), home.join(".pi/agent/AGENTS.md")),
    ]
}

fn synchronize_remote_instructions_file(path: &Path, marker_path: &Path) -> Result<()> {
    let block = remote_instruction_block(&marker_path);
    synchronize_remote_instructions_content(path, block)
}

#[cfg(unix)]
fn remote_instruction_block(marker_path: &Path) -> String {
    remote_unix_instruction_block(marker_path)
}

#[cfg(any(unix, test))]
fn remote_unix_instruction_block(marker_path: &Path) -> String {
    let marker_path = marker_path.to_string_lossy();
    format!(
        "{MANAGED_BLOCK_BEGIN_PREFIX}{REMOTE_MANAGED_BLOCK_VERSION} -->\n\
         This block is managed by Flint. It applies only to Agent Threads that Flint launches.\n\n\
         ## Worktree creation\n\n\
         After you create a git worktree, read this executable marker:\n\n\
             {marker_path}\n\n\
         If the marker is absent, continue without retieing the thread. Otherwise use its `executable` value as `<executable>` and check whether this is a controllable Agent Thread:\n\n\
             \"<executable>\" terminal current --json\n\n\
         If the connection fails or the result reports that the caller is not recognized, continue without retieing the thread. If the result has `is_agent_thread: true`, run:\n\n\
             \"<executable>\" thread retie --worktree <path>\n\
         {MANAGED_BLOCK_END}\n"
    )
}

#[cfg(windows)]
fn remote_instruction_block(marker_path: &Path) -> String {
    remote_windows_instruction_block(marker_path)
}

#[cfg(any(windows, test))]
fn remote_windows_instruction_block(marker_path: &Path) -> String {
    let marker_path = marker_path.to_string_lossy().replace('\'', "''");
    format!(
        "{MANAGED_BLOCK_BEGIN_PREFIX}{REMOTE_MANAGED_BLOCK_VERSION} -->\n\
         This block is managed by Flint. It applies only to Agent Threads that Flint launches.\n\n\
         ## Worktree creation\n\n\
         After you create a git worktree, read this executable marker in PowerShell:\n\n\
             $control = (Get-Content -Raw '{marker_path}' | ConvertFrom-Json).executable\n\n\
         If `$control` does not contain an executable path, continue without retieing the thread. Otherwise check whether this is a controllable Agent Thread:\n\n\
             & $control terminal current --json\n\n\
         If the connection fails or the result reports that the caller is not recognized, continue without retieing the thread. If the result has `is_agent_thread: true`, run:\n\n\
             & $control thread retie --worktree \"<path>\"\n\
         {MANAGED_BLOCK_END}\n"
    )
}

fn synchronize_remote_instructions_content(path: &Path, block: String) -> Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("failed to read {path:?}")),
    };
    let content = if let Some(start) = existing.find(MANAGED_BLOCK_BEGIN_PREFIX) {
        let relative_end = existing[start..]
            .find(MANAGED_BLOCK_END)
            .context("Flint managed instruction block has no end marker")?;
        let mut end = start + relative_end + MANAGED_BLOCK_END.len();
        if existing.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        let mut content = existing;
        content.replace_range(start..end, &block);
        content
    } else {
        let mut content = existing;
        if !content.is_empty() && !content.ends_with("\n\n") {
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push('\n');
        }
        content.push_str(&block);
        content
    };
    let parent = path.parent().context("instruction path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, content)?;
    replace_file(&temporary, path)?;
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> Result<()> {
    std::fs::rename(source, target).with_context(|| format!("failed to replace {target:?}"))
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Storage::FileSystem::{
        MOVE_FILE_FLAGS, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let target_path = target.to_path_buf();
    let mut source = source.as_os_str().encode_wide().collect::<Vec<_>>();
    source.push(0);
    let mut target = target.as_os_str().encode_wide().collect::<Vec<_>>();
    target.push(0);
    // SAFETY: both vectors are terminated UTF-16 paths and stay live through the call.
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVE_FILE_FLAGS(MOVEFILE_REPLACE_EXISTING.0 | MOVEFILE_WRITE_THROUGH.0),
        )
    }
    .with_context(|| format!("failed to replace {target_path:?}"))
}

#[cfg(unix)]
pub(crate) fn start(session: AnyProtoClient, cx: &mut gpui::App) -> Result<()> {
    let registrations = register_terminal_allocation_handler(&session, cx);
    let directory = control_directory();
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create remote control directory {directory:?}"))?;
    let instance = uuid::Uuid::new_v4().simple().to_string();
    let endpoint = directory.join(format!("{instance}.sock"));
    let record_path = directory.join(format!("{instance}.json"));
    let listener = bind_unix_endpoint(&endpoint)?;
    write_discovery_record(&record_path, &endpoint)?;

    cx.on_app_quit({
        let endpoint = endpoint.clone();
        let record_path = record_path.clone();
        move |_cx| {
            let endpoint = endpoint.clone();
            let record_path = record_path.clone();
            async move {
                std::fs::remove_file(endpoint).ok();
                std::fs::remove_file(record_path).ok();
            }
        }
    })
    .detach();

    cx.spawn(async move |cx| {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let session = session.clone();
            let registrations = registrations.clone();
            cx.spawn(async move |_cx| {
                if let Err(error) = handle_connection(stream, session, registrations).await {
                    log::warn!("remote flintctl connection failed: {error:#}");
                }
            })
            .detach();
        }
        std::fs::remove_file(&endpoint).ok();
        std::fs::remove_file(&record_path).ok();
    })
    .detach();
    Ok(())
}

#[cfg(unix)]
fn bind_unix_endpoint(endpoint: &Path) -> Result<net::async_net::UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;

    let listener = net::async_net::UnixListener::bind(endpoint)
        .with_context(|| format!("failed to bind remote control endpoint {endpoint:?}"))?;
    std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to protect remote control endpoint {endpoint:?}"))?;
    Ok(listener)
}

#[cfg(windows)]
pub(crate) fn start(session: AnyProtoClient, cx: &mut gpui::App) -> Result<()> {
    windows_control::start(session, cx)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn start(_session: AnyProtoClient, _cx: &mut gpui::App) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn handle_connection(
    mut stream: net::async_net::UnixStream,
    session: AnyProtoClient,
    registrations: Arc<Mutex<RemoteTerminalRegistrations>>,
) -> Result<()> {
    let peer_process_id = peer_process_id(&stream)?;
    let request = read_frame(&mut stream, MAX_REQUEST_BYTES).await?;

    if let Ok(EndpointRequest::RegisterTerminal {
        remote_terminal_registration_id,
    }) = serde_json::from_slice(&request)
    {
        let claimed = {
            let mut registrations = registrations.lock();
            prune_registrations(&mut registrations);
            let root_process_id = parent_process_id(peer_process_id).unwrap_or(peer_process_id);
            let root_process_start_time = process_start_time(root_process_id);
            match (
                registrations.get_mut(&remote_terminal_registration_id),
                root_process_start_time,
            ) {
                (Some(registration), Some(root_process_start_time)) => {
                    registration.terminal = Some(RegisteredRemoteTerminal {
                        root_process_id,
                        root_process_start_time,
                        working_directory: process_working_directory(peer_process_id),
                        is_agent_thread: None,
                        local_registration_verified: false,
                    });
                    true
                }
                _ => false,
            }
        };
        return write_endpoint_response(&mut stream, &EndpointResponse { claimed }).await;
    }

    let control_request: ControlRequest =
        serde_json::from_slice(&request).context("failed to decode remote control request")?;
    prune_registrations(&mut registrations.lock());
    let registration_id = resolve_registration(peer_process_id, &registrations.lock());
    let registration_id = match registration_id {
        Some(registration_id) => Some(registration_id),
        None => resolve_agent_thread_fallback(peer_process_id, &session, &registrations).await?,
    };
    let Some(remote_terminal_registration_id) = registration_id else {
        return write_endpoint_response(&mut stream, &EndpointResponse { claimed: false }).await;
    };
    let envelope = RemoteControlEnvelope {
        remote_terminal_registration_id: remote_terminal_registration_id.clone(),
        control_request,
    };
    let envelope = serde_json::to_vec(&envelope).context("failed to encode remote envelope")?;
    if envelope.len() > MAX_REQUEST_BYTES {
        return write_control_response(
            &mut stream,
            &ControlResponse::error(
                ControlErrorCode::InvalidRequest,
                "remote control envelope exceeds the request byte limit",
            ),
        )
        .await;
    }
    let rpc_response = request_before_disconnect(
        session.request(proto::RemoteTerminalControl { envelope }),
        wait_for_disconnect(stream.clone()),
    )
    .await;
    let Some(rpc_response) = rpc_response else {
        return Ok(());
    };
    let mut response = match rpc_response {
        Ok(response) => {
            if response.response.len() > MAX_RESPONSE_BYTES {
                ControlResponse::error(
                    ControlErrorCode::ResponseTooLarge,
                    "remote control response exceeds the byte limit",
                )
            } else {
                serde_json::from_slice(&response.response)
                    .context("failed to decode remote control response")?
            }
        }
        Err(error) => ControlResponse::error(
            ControlErrorCode::RemoteControlUnavailable,
            format!("the matching remote session is unavailable: {error}"),
        ),
    };
    let local_registration_verified = registrations
        .lock()
        .get(&remote_terminal_registration_id)
        .and_then(|registration| registration.terminal.as_ref())
        .is_some_and(|registration| registration.local_registration_verified);
    if matches!(
        response.result,
        ControlResult::Error(ref error) if error.code == ControlErrorCode::RemoteSessionStale
    ) && !local_registration_verified
    {
        response = ControlResponse::not_ready();
    } else if matches!(response.result, ControlResult::Ok(_)) {
        if let Some(registration) = registrations
            .lock()
            .get_mut(&remote_terminal_registration_id)
        {
            if let Some(terminal) = registration.terminal.as_mut() {
                terminal.local_registration_verified = true;
            }
        }
    }
    write_control_response(&mut stream, &response).await
}

async fn request_before_disconnect<T>(
    request: impl Future<Output = T>,
    disconnect: impl Future<Output = ()>,
) -> Option<T> {
    let request = request.fuse();
    let disconnect = disconnect.fuse();
    futures::pin_mut!(request, disconnect);
    select! {
        response = request => Some(response),
        _ = disconnect => None,
    }
}

#[cfg(unix)]
async fn wait_for_disconnect(mut stream: net::async_net::UnixStream) {
    use futures::AsyncReadExt as _;
    let mut byte = [0; 1];
    loop {
        match stream.read(&mut byte).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn resolve_agent_thread_fallback(
    peer_process_id: u32,
    session: &AnyProtoClient,
    registrations: &Arc<Mutex<RemoteTerminalRegistrations>>,
) -> Result<Option<RemoteTerminalRegistrationId>> {
    let Some(peer_working_directory) = process_working_directory(peer_process_id) else {
        return Ok(None);
    };
    let candidates = working_directory_candidates(&peer_working_directory, &registrations.lock());
    let mut agent_thread_candidates = Vec::new();
    for registration_id in candidates {
        let known_agent_thread = registrations
            .lock()
            .get(&registration_id)
            .and_then(|registration| registration.terminal.as_ref())
            .and_then(|registration| registration.is_agent_thread);
        let is_agent_thread = match known_agent_thread {
            Some(is_agent_thread) => is_agent_thread,
            None => {
                let envelope = RemoteControlEnvelope {
                    remote_terminal_registration_id: registration_id.clone(),
                    control_request: ControlRequest::current(ControlCommand::TerminalCurrent),
                };
                let envelope = serde_json::to_vec(&envelope)?;
                let response = session
                    .request(proto::RemoteTerminalControl { envelope })
                    .await
                    .ok()
                    .filter(|response| response.response.len() <= MAX_RESPONSE_BYTES)
                    .and_then(|response| {
                        serde_json::from_slice::<ControlResponse>(&response.response).ok()
                    });
                let Some(ControlResponse {
                    result: ControlResult::Ok(ControlSuccess::TerminalCurrent(metadata)),
                    ..
                }) = response
                else {
                    continue;
                };
                let is_agent_thread = metadata.is_agent_thread;
                if let Some(registration) = registrations.lock().get_mut(&registration_id)
                    && let Some(terminal) = registration.terminal.as_mut()
                {
                    terminal.is_agent_thread = Some(is_agent_thread);
                }
                is_agent_thread
            }
        };
        if is_agent_thread {
            agent_thread_candidates.push(registration_id);
        }
    }
    Ok(unique_registration(agent_thread_candidates))
}

fn working_directory_candidates(
    peer_working_directory: &Path,
    registrations: &RemoteTerminalRegistrations,
) -> Vec<RemoteTerminalRegistrationId> {
    registrations
        .iter()
        .filter_map(|(registration_id, registration)| {
            let registration = registration.terminal.as_ref()?;
            let root = registration.working_directory.as_deref()?;
            (peer_working_directory == root || peer_working_directory.starts_with(root))
                .then(|| registration_id.clone())
        })
        .collect()
}

fn unique_registration(
    registrations: Vec<RemoteTerminalRegistrationId>,
) -> Option<RemoteTerminalRegistrationId> {
    match registrations.as_slice() {
        [registration_id] => Some(registration_id.clone()),
        _ => None,
    }
}

#[cfg(unix)]
pub(crate) fn run_client(request: ControlRequest) -> Result<ControlResponse> {
    run_unix_client(&request, &control_directory())
}

#[cfg(unix)]
fn run_unix_client(request: &ControlRequest, directory: &Path) -> Result<ControlResponse> {
    const RETRY_BACKOFFS: &[std::time::Duration] = &[
        std::time::Duration::from_millis(250),
        std::time::Duration::from_millis(500),
        std::time::Duration::from_millis(1_000),
    ];
    let mut selected_endpoint = None;
    for attempt in 0..=RETRY_BACKOFFS.len() {
        let discovery = discovery_records_in(directory)?;
        let records = discovery
            .records
            .into_iter()
            .filter(|record| {
                selected_endpoint
                    .as_ref()
                    .is_none_or(|endpoint| endpoint == &record.endpoint)
            })
            .collect::<Vec<_>>();
        let has_records = !records.is_empty();
        let mut retry = false;
        for record in records {
            let mut stream = match std::os::unix::net::UnixStream::connect(&record.endpoint) {
                Ok(stream) => stream,
                Err(error) if selected_endpoint.is_some() => {
                    return Ok(ControlResponse::error(
                        ControlErrorCode::RemoteControlUnavailable,
                        format!("the matching remote session is unavailable: {error}"),
                    ));
                }
                Err(_) => continue,
            };
            write_sync_frame(
                &mut stream,
                &serde_json::to_vec(&request)?,
                MAX_REQUEST_BYTES,
            )?;
            let response: EndpointReply = read_sync_json(&mut stream, MAX_RESPONSE_BYTES)?;
            let response = match response {
                EndpointReply::Control(response) => response,
                EndpointReply::Claim(response) if !response.claimed => continue,
                EndpointReply::Claim(_) => {
                    bail!("the remote control endpoint returned no control response")
                }
            };
            if matches!(response.result, ControlResult::NotReady) {
                selected_endpoint = Some(record.endpoint);
                retry = true;
                break;
            }
            return Ok(response);
        }
        if discovery.version_mismatch {
            return Ok(ControlResponse::error(
                ControlErrorCode::RemoteVersionMismatch,
                "the installed flintctl protocol does not match the available remote session",
            ));
        }
        if attempt == RETRY_BACKOFFS.len() {
            break;
        }
        if retry || has_records {
            std::thread::sleep(RETRY_BACKOFFS[attempt]);
        } else {
            break;
        }
    }
    Ok(ControlResponse::error(
        ControlErrorCode::CallerNotRecognized,
        "this process is not in a controllable Flint remote terminal",
    ))
}

#[cfg(windows)]
pub(crate) fn run_client(request: ControlRequest) -> Result<ControlResponse> {
    windows_control::run_client(request)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn run_client(_request: ControlRequest) -> Result<ControlResponse> {
    bail!("remote flintctl is not supported on this platform")
}

#[cfg(unix)]
pub(crate) fn register_current_terminal(
    remote_terminal_registration_id: RemoteTerminalRegistrationId,
) -> Result<()> {
    let request = EndpointRequest::RegisterTerminal {
        remote_terminal_registration_id,
    };
    let payload = serde_json::to_vec(&request)?;
    for delay in [0, 100, 250, 500, 1_000] {
        if delay > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay));
        }
        for record in discovery_records()?.records {
            let mut stream = match std::os::unix::net::UnixStream::connect(&record.endpoint) {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            write_sync_frame(&mut stream, &payload, MAX_REQUEST_BYTES)?;
            let response: EndpointResponse = read_sync_json(&mut stream, MAX_RESPONSE_BYTES)?;
            if response.claimed {
                return Ok(());
            }
        }
    }
    bail!("no matching Flint remote control endpoint is available")
}

#[cfg(windows)]
pub(crate) fn register_current_terminal(
    remote_terminal_registration_id: RemoteTerminalRegistrationId,
) -> Result<()> {
    windows_control::register_current_terminal(remote_terminal_registration_id)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn register_current_terminal(
    _remote_terminal_registration_id: RemoteTerminalRegistrationId,
) -> Result<()> {
    bail!("remote terminal registration is not supported on this platform")
}

fn control_directory() -> PathBuf {
    paths::data_dir()
        .join("ac")
        .join(RELEASE_CHANNEL.dev_name())
}

fn command_directory() -> PathBuf {
    paths::data_dir()
        .join("agent-control")
        .join(RELEASE_CHANNEL.dev_name())
        .join(crate::VERSION.as_str())
}

fn write_discovery_record(record_path: &Path, endpoint: &Path) -> Result<()> {
    let record = DiscoveryRecord {
        endpoint: endpoint.to_path_buf(),
        server_process_id: std::process::id(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
    };
    let temporary_path = record_path.with_extension("json.tmp");
    std::fs::write(&temporary_path, serde_json::to_vec(&record)?)?;
    std::fs::rename(temporary_path, record_path)?;
    Ok(())
}

fn discovery_records() -> Result<DiscoverySet> {
    discovery_records_in(&control_directory())
}

fn discovery_records_in(directory: &Path) -> Result<DiscoverySet> {
    let mut records = Vec::new();
    let mut version_mismatch = false;
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(DiscoverySet {
            records,
            version_mismatch,
        });
    };
    for entry in entries.take(MAX_DISCOVERY_RECORDS) {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<DiscoveryRecord>(&bytes) else {
            continue;
        };
        if !process_is_live(record.server_process_id) {
            continue;
        }
        if record.protocol_major != PROTOCOL_VERSION.major {
            version_mismatch = true;
            continue;
        }
        records.push(record);
    }
    records.sort_by(|left, right| left.endpoint.cmp(&right.endpoint));
    Ok(DiscoverySet {
        records,
        version_mismatch,
    })
}

fn resolve_registration(
    peer_process_id: u32,
    registrations: &RemoteTerminalRegistrations,
) -> Option<RemoteTerminalRegistrationId> {
    let tracked = registrations
        .iter()
        .filter_map(|(registration_id, registration)| {
            let registration = registration.terminal.as_ref()?;
            Some((
                registration.root_process_id,
                (registration_id, registration.root_process_start_time),
            ))
        })
        .collect::<HashMap<_, _>>();
    let refresh = sysinfo::ProcessRefreshKind::nothing();
    let mut system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(refresh),
    );
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, refresh);
    let mut current = sysinfo::Pid::from_u32(peer_process_id);
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if let Some((registration_id, start_time)) = tracked.get(&current.as_u32())
            && system
                .process(current)
                .is_some_and(|process| process.start_time() == *start_time)
        {
            return Some((*registration_id).clone());
        }
        let Some(parent) = system.process(current).and_then(sysinfo::Process::parent) else {
            break;
        };
        if parent == current {
            return None;
        }
        current = parent;
    }
    None
}

fn parent_process_id(process_id: u32) -> Option<u32> {
    let refresh = sysinfo::ProcessRefreshKind::nothing();
    let mut system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(refresh),
    );
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, refresh);
    system
        .process(sysinfo::Pid::from_u32(process_id))?
        .parent()
        .map(sysinfo::Pid::as_u32)
}

fn process_working_directory(process_id: u32) -> Option<PathBuf> {
    let refresh = sysinfo::ProcessRefreshKind::nothing().with_cwd(sysinfo::UpdateKind::Always);
    let mut system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(refresh),
    );
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, refresh);
    system
        .process(sysinfo::Pid::from_u32(process_id))?
        .cwd()
        .map(Path::to_path_buf)
}

fn process_start_time(process_id: u32) -> Option<u64> {
    let refresh = sysinfo::ProcessRefreshKind::nothing();
    let mut system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(refresh),
    );
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, refresh);
    system
        .process(sysinfo::Pid::from_u32(process_id))
        .map(sysinfo::Process::start_time)
}

fn process_is_live(process_id: u32) -> bool {
    let processes = [sysinfo::Pid::from_u32(process_id)];
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&processes), true);
    system.process(sysinfo::Pid::from_u32(process_id)).is_some()
}

fn prune_registrations(registrations: &mut RemoteTerminalRegistrations) {
    let now = Instant::now();
    registrations.retain(|_, registration| match registration.terminal.as_ref() {
        Some(terminal) => process_start_time(terminal.root_process_id)
            .is_some_and(|start_time| start_time == terminal.root_process_start_time),
        None => now.duration_since(registration.allocated_at) < PENDING_REGISTRATION_LIFETIME,
    });
}

#[cfg(target_os = "linux")]
fn peer_process_id(stream: &net::async_net::UnixStream) -> Result<u32> {
    use std::os::fd::AsRawFd as _;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials and length are writable for the duration of getsockopt.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to read peer credentials");
    }
    u32::try_from(credentials.pid).context("peer process id is invalid")
}

#[cfg(target_os = "macos")]
fn peer_process_id(stream: &net::async_net::UnixStream) -> Result<u32> {
    use std::os::fd::AsRawFd as _;
    let mut process_id: libc::pid_t = 0;
    let mut length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: process_id and length are writable for the duration of getsockopt.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut process_id as *mut libc::pid_t).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to read peer process id");
    }
    u32::try_from(process_id).context("peer process id is invalid")
}

#[cfg(unix)]
async fn read_frame(stream: &mut net::async_net::UnixStream, maximum: usize) -> Result<Vec<u8>> {
    use futures::AsyncReadExt as _;
    let mut length = [0; FRAME_LENGTH_BYTES];
    stream.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > maximum {
        bail!("frame exceeds the {maximum}-byte limit");
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

#[cfg(unix)]
async fn write_endpoint_response(
    stream: &mut net::async_net::UnixStream,
    response: &EndpointResponse,
) -> Result<()> {
    let payload = serde_json::to_vec(response)?;
    let frame = frame_payload(&payload, MAX_RESPONSE_BYTES)?;
    stream.write_all(&frame).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(unix)]
async fn write_control_response(
    stream: &mut net::async_net::UnixStream,
    response: &ControlResponse,
) -> Result<()> {
    let payload = serde_json::to_vec(response)?;
    let frame = frame_payload(&payload, MAX_RESPONSE_BYTES)?;
    stream.write_all(&frame).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(unix)]
fn write_sync_frame(
    stream: &mut std::os::unix::net::UnixStream,
    payload: &[u8],
    maximum: usize,
) -> Result<()> {
    use std::io::Write as _;
    stream.write_all(&frame_payload(payload, maximum)?)?;
    stream.flush()?;
    Ok(())
}

#[cfg(unix)]
fn read_sync_json<T: serde::de::DeserializeOwned>(
    stream: &mut std::os::unix::net::UnixStream,
    maximum: usize,
) -> Result<T> {
    use std::io::Read as _;
    let mut length = [0; FRAME_LENGTH_BYTES];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > maximum {
        bail!("frame exceeds the {maximum}-byte limit");
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).context("failed to decode endpoint response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn registration(terminal: RegisteredRemoteTerminal) -> RemoteTerminalRegistration {
        RemoteTerminalRegistration {
            allocated_at: Instant::now(),
            terminal: Some(terminal),
        }
    }

    #[test]
    fn client_disconnect_drops_the_in_flight_forward_request() {
        struct DropSignal(Arc<AtomicBool>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let signal = DropSignal(dropped.clone());
        let request = async move {
            let _signal = signal;
            futures::future::pending::<()>().await;
        };

        let result = smol::block_on(request_before_disconnect(request, async {}));

        assert!(result.is_none());
        assert!(dropped.load(Ordering::Acquire));
    }

    #[cfg(unix)]
    #[test]
    fn ancestry_resolution_selects_the_registered_terminal() {
        let mut child = smol::process::Command::new("sleep")
            .arg("10")
            .spawn()
            .expect("spawn child");
        let registration_id = RemoteTerminalRegistrationId("registration-1".to_string());
        let registrations = HashMap::from([(
            registration_id.clone(),
            registration(RegisteredRemoteTerminal {
                root_process_id: std::process::id(),
                root_process_start_time: process_start_time(std::process::id())
                    .expect("current process start time"),
                working_directory: None,
                is_agent_thread: Some(false),
                local_registration_verified: true,
            }),
        )]);

        let resolved = resolve_registration(child.id(), &registrations);
        child.kill().expect("kill child");
        smol::block_on(child.status()).expect("wait for child");

        assert_eq!(resolved, Some(registration_id));
    }

    #[cfg(unix)]
    #[test]
    fn ancestry_resolution_rejects_reused_process_id() {
        let registration_id = RemoteTerminalRegistrationId("registration-1".to_string());
        let registrations = HashMap::from([(
            registration_id,
            registration(RegisteredRemoteTerminal {
                root_process_id: std::process::id(),
                root_process_start_time: u64::MAX,
                working_directory: None,
                is_agent_thread: Some(false),
                local_registration_verified: true,
            }),
        )]);

        assert_eq!(
            resolve_registration(std::process::id(), &registrations),
            None
        );
    }

    #[test]
    fn registration_pruning_removes_expired_pending_and_stale_processes() {
        let mut registrations = HashMap::from([
            (
                RemoteTerminalRegistrationId("pending".to_string()),
                RemoteTerminalRegistration {
                    allocated_at: Instant::now()
                        .checked_sub(PENDING_REGISTRATION_LIFETIME + Duration::from_secs(1))
                        .expect("old instant"),
                    terminal: None,
                },
            ),
            (
                RemoteTerminalRegistrationId("stale".to_string()),
                registration(RegisteredRemoteTerminal {
                    root_process_id: std::process::id(),
                    root_process_start_time: u64::MAX,
                    working_directory: None,
                    is_agent_thread: Some(false),
                    local_registration_verified: true,
                }),
            ),
        ]);

        prune_registrations(&mut registrations);

        assert!(registrations.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn unix_endpoint_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("temporary directory");
        let endpoint = directory.path().join("control.sock");
        let _listener = bind_unix_endpoint(&endpoint).expect("bind endpoint");

        let mode = std::fs::metadata(endpoint)
            .expect("endpoint metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn unix_endpoint_path_fits_platform_limit() {
        const PORTABLE_UNIX_SOCKET_PATH_LIMIT: usize = 103;
        let instance = "0".repeat(32);
        let endpoint = control_directory().join(format!("{instance}.sock"));

        assert!(
            endpoint.as_os_str().as_encoded_bytes().len() <= PORTABLE_UNIX_SOCKET_PATH_LIMIT,
            "remote control endpoint is too long: {}",
            endpoint.display()
        );
    }

    #[test]
    fn working_directory_candidates_do_not_authorize_ordinary_terminals() {
        let ordinary = RemoteTerminalRegistrationId("ordinary".to_string());
        let registrations = HashMap::from([(
            ordinary,
            registration(RegisteredRemoteTerminal {
                root_process_id: 1,
                root_process_start_time: 0,
                working_directory: Some(PathBuf::from("/workspace")),
                is_agent_thread: Some(false),
                local_registration_verified: true,
            }),
        )]);

        let candidates = working_directory_candidates(Path::new("/workspace/src"), &registrations);
        let verified = candidates
            .into_iter()
            .filter(|registration_id| {
                registrations
                    .get(registration_id)
                    .and_then(|registration| registration.terminal.as_ref())
                    .and_then(|registration| registration.is_agent_thread)
                    == Some(true)
            })
            .collect();

        assert_eq!(unique_registration(verified), None);
    }

    #[test]
    fn agent_thread_working_directory_fallback_rejects_ambiguity() {
        let first = RemoteTerminalRegistrationId("first".to_string());
        let second = RemoteTerminalRegistrationId("second".to_string());

        assert_eq!(
            unique_registration(vec![first]),
            Some(RemoteTerminalRegistrationId("first".to_string()))
        );
        assert_eq!(
            unique_registration(vec![
                RemoteTerminalRegistrationId("first".to_string()),
                second
            ]),
            None
        );
    }

    #[test]
    fn discovery_keeps_live_compatible_instances_and_reports_major_mismatch() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let compatible = DiscoveryRecord {
            endpoint: PathBuf::from("compatible"),
            server_process_id: std::process::id(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
        };
        let incompatible = DiscoveryRecord {
            endpoint: PathBuf::from("incompatible"),
            server_process_id: std::process::id(),
            protocol_major: PROTOCOL_VERSION.major.saturating_add(1),
            protocol_minor: 0,
        };
        let stale = DiscoveryRecord {
            endpoint: PathBuf::from("stale"),
            server_process_id: u32::MAX,
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
        };
        for (name, record) in [
            ("compatible.json", compatible),
            ("incompatible.json", incompatible),
            ("stale.json", stale),
        ] {
            std::fs::write(
                directory.path().join(name),
                serde_json::to_vec(&record).expect("encode record"),
            )
            .expect("write record");
        }

        let discovery = discovery_records_in(directory.path()).expect("read discovery");

        assert_eq!(discovery.records.len(), 1);
        assert_eq!(discovery.records[0].endpoint, PathBuf::from("compatible"));
        assert!(discovery.version_mismatch);
    }

    #[cfg(unix)]
    #[test]
    fn client_pins_the_first_instance_that_claims_the_caller() {
        use std::io::{Read as _, Write as _};

        fn read_request(stream: &mut std::os::unix::net::UnixStream) {
            let mut length = [0; FRAME_LENGTH_BYTES];
            stream.read_exact(&mut length).expect("read request length");
            let mut payload = vec![0; u32::from_be_bytes(length) as usize];
            stream
                .read_exact(&mut payload)
                .expect("read request payload");
        }

        fn write_response(stream: &mut std::os::unix::net::UnixStream, response: &ControlResponse) {
            let payload = serde_json::to_vec(response).expect("encode response");
            let frame = frame_payload(&payload, MAX_RESPONSE_BYTES).expect("frame response");
            stream.write_all(&frame).expect("write response");
        }

        let directory = tempfile::tempdir().expect("temporary directory");
        let first_endpoint = directory.path().join("a.sock");
        let second_endpoint = directory.path().join("b.sock");
        let first_listener =
            std::os::unix::net::UnixListener::bind(&first_endpoint).expect("bind first endpoint");
        let second_listener =
            std::os::unix::net::UnixListener::bind(&second_endpoint).expect("bind second endpoint");
        write_discovery_record(&directory.path().join("a.json"), &first_endpoint)
            .expect("write first record");
        write_discovery_record(&directory.path().join("b.json"), &second_endpoint)
            .expect("write second record");

        let first = std::thread::spawn(move || {
            let (mut stream, _) = first_listener.accept().expect("accept first request");
            read_request(&mut stream);
            write_response(&mut stream, &ControlResponse::not_ready());
            let (mut stream, _) = first_listener.accept().expect("accept retry");
            read_request(&mut stream);
            write_response(
                &mut stream,
                &ControlResponse::error(ControlErrorCode::Timeout, "selected instance"),
            );
        });
        second_listener
            .set_nonblocking(true)
            .expect("set second endpoint nonblocking");
        let second = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match second_listener.accept() {
                    Ok((mut stream, _)) => {
                        read_request(&mut stream);
                        write_response(
                            &mut stream,
                            &ControlResponse::error(ControlErrorCode::Internal, "wrong instance"),
                        );
                        return true;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept second request: {error}"),
                }
            }
            false
        });

        let response = run_unix_client(
            &ControlRequest::current(ControlCommand::Status),
            directory.path(),
        )
        .expect("run remote client");

        first.join().expect("first endpoint thread");
        assert!(!second.join().expect("second endpoint thread"));
        assert!(matches!(
            response.result,
            ControlResult::Error(ref error) if error.code == ControlErrorCode::Timeout
        ));
    }

    #[test]
    fn remote_instruction_sync_replaces_the_existing_managed_block() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("AGENTS.md");
        std::fs::write(
            &path,
            "before\n\n<!-- Flint managed agent-thread instructions: begin v2 -->\nold\n<!-- Flint managed agent-thread instructions: end -->\nafter\n",
        )
        .expect("write instructions");

        synchronize_remote_instructions_file(&path, &directory.path().join("executable.json"))
            .expect("synchronize instructions");
        let content = std::fs::read_to_string(path).expect("read instructions");

        assert!(content.contains("before"));
        assert!(content.contains("after"));
        assert!(content.contains("begin v4"));
        assert!(content.contains("thread retie --worktree"));
        assert!(!content.contains("\nold\n"));
    }

    #[test]
    fn remote_instruction_locations_cover_all_supported_managed_agents() {
        let home = Path::new("/remote-home");
        let locations = remote_instruction_locations(home)
            .into_iter()
            .map(|(_, instructions)| instructions)
            .collect::<Vec<_>>();

        assert_eq!(
            locations,
            vec![
                home.join(".codex/AGENTS.md"),
                home.join(".claude/CLAUDE.md"),
                home.join(".config/opencode/AGENTS.md"),
                home.join(".pi/agent/AGENTS.md"),
            ]
        );
    }

    #[test]
    fn remote_instruction_blocks_use_the_versioned_marker_on_each_platform() {
        let marker = Path::new("/remote/agent-control/stable/1.2.3/executable.json");
        let unix = remote_unix_instruction_block(marker);
        let windows = remote_windows_instruction_block(marker);

        assert!(unix.contains(&marker.to_string_lossy().to_string()));
        assert!(unix.contains("\"<executable>\" terminal current --json"));
        assert!(unix.contains("\"<executable>\" thread retie --worktree <path>"));
        assert!(windows.contains("Get-Content -Raw"));
        assert!(windows.contains("& $control terminal current --json"));
        assert!(windows.contains("& $control thread retie --worktree \"<path>\""));
    }

    #[cfg(unix)]
    #[test]
    fn remote_command_install_writes_links_marker_and_all_agent_instructions() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let server_directory = directory.path().join("server");
        let control_directory = directory.path().join("control/stable/1.2.3");
        let home = directory.path().join("home");
        std::fs::create_dir_all(&server_directory).expect("create server directory");
        let server = server_directory.join("flint-remote-server-stable-1.2.3");
        std::fs::write(&server, "server").expect("write server executable");
        for (installed_directory, _) in remote_instruction_locations(&home) {
            std::fs::create_dir_all(installed_directory).expect("create agent directory");
        }

        install_command_at(&server, &control_directory, &home).expect("install remote command");

        assert_eq!(
            std::fs::canonicalize(server_directory.join("flintctl"))
                .expect("resolve sibling command"),
            std::fs::canonicalize(&server).expect("resolve server")
        );
        let scoped_command = control_directory.join("flintctl");
        assert_eq!(
            std::fs::canonicalize(&scoped_command).expect("resolve scoped command"),
            std::fs::canonicalize(&server).expect("resolve server")
        );
        let marker: serde_json::Value = serde_json::from_slice(
            &std::fs::read(control_directory.join("executable.json")).expect("read marker"),
        )
        .expect("decode marker");
        assert_eq!(
            marker["executable"],
            scoped_command.to_string_lossy().as_ref()
        );
        for (_, instructions_path) in remote_instruction_locations(&home) {
            let instructions =
                std::fs::read_to_string(instructions_path).expect("read agent instructions");
            assert!(instructions.contains("thread retie --worktree"));
            assert!(
                instructions.contains(
                    &control_directory
                        .join("executable.json")
                        .to_string_lossy()
                        .to_string()
                )
            );
        }
    }
}
