use std::{
    ffi::c_void,
    mem::size_of,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use gpui::AppContext as _;
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_BROKEN_PIPE, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_DATA,
            ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, HANDLE, HLOCAL,
            INVALID_HANDLE_VALUE, LocalFree, WIN32_ERROR,
        },
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1, SE_KERNEL_OBJECT, SetSecurityInfo,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
            TOKEN_GROUPS, TOKEN_QUERY, TokenGroups,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE,
            FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
            WRITE_DAC, WriteFile,
        },
        System::{
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                GetNamedPipeClientProcessId, NAMED_PIPE_MODE, PIPE_READMODE_MESSAGE,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT, PeekNamedPipe,
                SetNamedPipeHandleState, WaitNamedPipeW,
            },
            SystemServices::SE_GROUP_LOGON_ID,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
    core::{Error as WindowsError, HSTRING, PCWSTR, PWSTR},
};

use super::*;

const PIPE_BUFFER_BYTES: u32 = 16 * 1024;
const PIPE_INSTANCE_COUNT: usize = 4;
const RETRY_BACKOFFS: &[Duration] = &[
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_millis(1_000),
];

pub(super) fn start(session: AnyProtoClient, cx: &mut gpui::App) -> Result<()> {
    let directory = control_directory();
    std::fs::create_dir_all(&directory)?;
    let instance = uuid::Uuid::new_v4().simple().to_string();
    let pipe_name = format!(r"\\.\pipe\flint-remote-control-{instance}");
    let record_path = directory.join(format!("{instance}.json"));
    let logon_sid = current_logon_sid_string()?;
    let registrations = Arc::new(Mutex::new(HashMap::new()));
    let state = cx.new(|_| RemoteControlState {
        registrations: registrations.clone(),
    });
    session.add_request_handler(
        state.downgrade(),
        |_state,
         _envelope: rpc::TypedEnvelope<proto::AllocateRemoteTerminalRegistration>,
         mut cx| async move {
            let registration_id = RemoteTerminalRegistrationId(uuid::Uuid::new_v4().to_string());
            _state.update(&mut cx, |state, _cx| {
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

    let first_pipe = create_pipe(&pipe_name, &logon_sid, true)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut pipes = vec![first_pipe];
    for _ in 1..PIPE_INSTANCE_COUNT {
        pipes.push(create_pipe(&pipe_name, &logon_sid, false)?);
    }
    restrict_pipe_dacl(
        pipes
            .first()
            .context("remote flintctl named-pipe pool is empty")?,
        &logon_sid,
    )?;
    for pipe in pipes {
        let session = session.clone();
        let registrations = registrations.clone();
        let shutdown = shutdown.clone();
        std::thread::Builder::new()
            .name("remote-flintctl-pipe".to_string())
            .spawn(move || worker_loop(pipe, session, registrations, shutdown))
            .context("failed to start remote flintctl named-pipe worker")?;
    }
    write_discovery_record(&record_path, Path::new(&pipe_name))?;
    cx.on_app_quit(move |_cx| {
        shutdown.store(true, Ordering::Release);
        std::fs::remove_file(&record_path).ok();
        async {}
    })
    .detach();
    cx.spawn(async move |_cx| {
        let _state = state;
        futures::future::pending::<()>().await;
    })
    .detach();
    Ok(())
}

fn worker_loop(
    pipe: Pipe,
    session: AnyProtoClient,
    registrations: Arc<Mutex<RemoteTerminalRegistrations>>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Acquire) {
        if let Err(error) = serve_one(&pipe, &session, &registrations)
            && !shutdown.load(Ordering::Acquire)
        {
            log::warn!("remote flintctl named-pipe request failed: {error:#}");
        }
        // SAFETY: pipe owns a valid named-pipe server handle.
        unsafe { DisconnectNamedPipe(pipe.0) }.ok();
        if shutdown.load(Ordering::Acquire) {
            break;
        }
    }
}

fn serve_one(
    pipe: &Pipe,
    session: &AnyProtoClient,
    registrations: &Arc<Mutex<RemoteTerminalRegistrations>>,
) -> Result<()> {
    // SAFETY: pipe owns a listening named-pipe server handle.
    if let Err(error) = unsafe { ConnectNamedPipe(pipe.0, None) }
        && error.code() != WIN32_ERROR(ERROR_PIPE_CONNECTED.0).into()
    {
        return Err(error).context("failed to accept remote flintctl named-pipe client");
    }
    let mut peer_process_id = 0;
    // SAFETY: the pipe is connected and the output points to valid storage.
    unsafe { GetNamedPipeClientProcessId(pipe.0, &mut peer_process_id) }
        .context("failed to read remote flintctl peer process id")?;
    let request = read_message(pipe, MAX_REQUEST_BYTES + FRAME_LENGTH_BYTES)?;
    let request = agent_control_protocol::decode_frame(&request, MAX_REQUEST_BYTES)?;
    let response = if let Ok(EndpointRequest::RegisterTerminal {
        remote_terminal_registration_id,
    }) = serde_json::from_slice(request)
    {
        EndpointReply::Claim(register_terminal(
            peer_process_id,
            remote_terminal_registration_id,
            registrations,
        ))
    } else {
        let control_request: ControlRequest = serde_json::from_slice(request)?;
        smol::block_on(dispatch_request(
            pipe.0,
            peer_process_id,
            control_request,
            session,
            registrations,
        ))?
    };
    let payload = serde_json::to_vec(&response)?;
    let frame = frame_payload(&payload, MAX_RESPONSE_BYTES)?;
    write_all(pipe, &frame)
}

fn register_terminal(
    peer_process_id: u32,
    registration_id: RemoteTerminalRegistrationId,
    registrations: &Arc<Mutex<RemoteTerminalRegistrations>>,
) -> EndpointResponse {
    let mut registrations = registrations.lock();
    prune_registrations(&mut registrations);
    let Some(registration) = registrations.get_mut(&registration_id) else {
        return EndpointResponse { claimed: false };
    };
    let root_process_id = parent_process_id(peer_process_id).unwrap_or(peer_process_id);
    let Some(root_process_start_time) = process_start_time(root_process_id) else {
        return EndpointResponse { claimed: false };
    };
    registration.terminal = Some(RegisteredRemoteTerminal {
        root_process_id,
        root_process_start_time,
        working_directory: process_working_directory(peer_process_id),
        is_agent_thread: None,
        local_registration_verified: false,
    });
    EndpointResponse { claimed: true }
}

async fn dispatch_request(
    pipe: HANDLE,
    peer_process_id: u32,
    control_request: ControlRequest,
    session: &AnyProtoClient,
    registrations: &Arc<Mutex<RemoteTerminalRegistrations>>,
) -> Result<EndpointReply> {
    prune_registrations(&mut registrations.lock());
    let registration_id = resolve_registration(peer_process_id, &registrations.lock());
    let registration_id = match registration_id {
        Some(registration_id) => Some(registration_id),
        None => resolve_agent_thread_fallback(peer_process_id, session, registrations).await?,
    };
    let Some(registration_id) = registration_id else {
        return Ok(EndpointReply::Claim(EndpointResponse { claimed: false }));
    };
    let envelope = RemoteControlEnvelope {
        remote_terminal_registration_id: registration_id.clone(),
        control_request,
    };
    let envelope = serde_json::to_vec(&envelope)?;
    if envelope.len() > MAX_REQUEST_BYTES {
        return Ok(EndpointReply::Control(ControlResponse::error(
            ControlErrorCode::InvalidRequest,
            "remote control envelope exceeds the request byte limit",
        )));
    }
    let rpc_response = request_before_disconnect(
        session.request(proto::RemoteTerminalControl { envelope }),
        wait_for_disconnect(pipe),
    )
    .await;
    let Some(rpc_response) = rpc_response else {
        return Ok(EndpointReply::Claim(EndpointResponse { claimed: true }));
    };
    let mut response = match rpc_response {
        Ok(response) if response.response.len() <= MAX_RESPONSE_BYTES => {
            serde_json::from_slice::<ControlResponse>(&response.response)?
        }
        Ok(_) => ControlResponse::error(
            ControlErrorCode::ResponseTooLarge,
            "remote control response exceeds the byte limit",
        ),
        Err(error) => ControlResponse::error(
            ControlErrorCode::RemoteControlUnavailable,
            format!("the matching remote session is unavailable: {error}"),
        ),
    };
    let verified = registrations
        .lock()
        .get(&registration_id)
        .and_then(|registration| registration.terminal.as_ref())
        .is_some_and(|registration| registration.local_registration_verified);
    if matches!(
        response.result,
        ControlResult::Error(ref error) if error.code == ControlErrorCode::RemoteSessionStale
    ) && !verified
    {
        response = ControlResponse::not_ready();
    } else if matches!(response.result, ControlResult::Ok(_)) {
        if let Some(registration) = registrations.lock().get_mut(&registration_id)
            && let Some(terminal) = registration.terminal.as_mut()
        {
            terminal.local_registration_verified = true;
        }
    }
    Ok(EndpointReply::Control(response))
}

async fn wait_for_disconnect(pipe: HANDLE) {
    loop {
        // The unix path awaits a socket read, which resolves on its own when the
        // peer disconnects. A named pipe has no equivalent, so this polls. The
        // worker thread driving it runs under `smol::block_on` with no GPUI
        // executor in reach, and no test drives it.
        #[allow(
            clippy::disallowed_methods,
            reason = "no gpui executor on the named-pipe worker thread"
        )]
        smol::Timer::after(Duration::from_millis(100)).await;
        // SAFETY: the worker owns this live handle for the duration of dispatch.
        let result = unsafe { PeekNamedPipe(pipe, None, 0, None, None, None) };
        if let Err(error) = result
            && (error.code() == WIN32_ERROR(ERROR_BROKEN_PIPE.0).into()
                || error.code() == WIN32_ERROR(ERROR_NO_DATA.0).into())
        {
            return;
        }
    }
}

pub(super) fn run_client(request: ControlRequest) -> Result<ControlResponse> {
    let mut selected_endpoint = None;
    for attempt in 0..=RETRY_BACKOFFS.len() {
        let discovery = discovery_records()?;
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
            let Some(pipe_name) = record.endpoint.to_str() else {
                continue;
            };
            let mut pipe = match open_pipe(pipe_name) {
                Ok(Some(pipe)) => pipe,
                Ok(None) | Err(_) if selected_endpoint.is_some() => {
                    return Ok(ControlResponse::error(
                        ControlErrorCode::RemoteControlUnavailable,
                        "the matching remote session is unavailable",
                    ));
                }
                Ok(None) | Err(_) => continue,
            };
            write_all(
                &pipe,
                &frame_payload(&serde_json::to_vec(&request)?, MAX_REQUEST_BYTES)?,
            )?;
            let response = match read_endpoint_reply(&mut pipe)? {
                EndpointReply::Control(response) => response,
                EndpointReply::Claim(response) if !response.claimed => continue,
                EndpointReply::Claim(_) => {
                    bail!("remote endpoint returned no control response")
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

pub(super) fn register_current_terminal(
    registration_id: RemoteTerminalRegistrationId,
) -> Result<()> {
    let payload = serde_json::to_vec(&EndpointRequest::RegisterTerminal {
        remote_terminal_registration_id: registration_id,
    })?;
    for delay in [0, 100, 250, 500, 1_000] {
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        for record in discovery_records()?.records {
            let Some(pipe_name) = record.endpoint.to_str() else {
                continue;
            };
            let pipe = match open_pipe(pipe_name) {
                Ok(Some(pipe)) => pipe,
                Ok(None) | Err(_) => continue,
            };
            write_all(&pipe, &frame_payload(&payload, MAX_REQUEST_BYTES)?)?;
            let mut pipe = pipe;
            let response = match read_endpoint_reply(&mut pipe)? {
                EndpointReply::Claim(response) => response,
                EndpointReply::Control(_) => {
                    bail!("registration endpoint returned control data")
                }
            };
            if response.claimed {
                return Ok(());
            }
        }
    }
    bail!("no matching Flint remote control endpoint is available")
}

fn open_pipe(name: &str) -> Result<Option<Pipe>> {
    let name = HSTRING::from(name);
    // SAFETY: name is a terminated UTF-16 string and no raw pointers escape.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_READ.0 | FILE_WRITE_DATA.0 | FILE_WRITE_ATTRIBUTES.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
    };
    let handle = match handle {
        Ok(handle) => handle,
        Err(error) if error.code() == WIN32_ERROR(ERROR_PIPE_BUSY.0).into() => {
            // SAFETY: name remains valid for the duration of the wait.
            if let Err(error) = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 1_000) }.ok() {
                log::debug!("failed waiting for busy remote flintctl named pipe: {error}");
            }
            return Ok(None);
        }
        Err(error) => return Err(error).context("failed to open remote flintctl named pipe"),
    };
    let pipe = Pipe(handle);
    let mode = NAMED_PIPE_MODE(PIPE_READMODE_MESSAGE.0);
    // SAFETY: pipe is a connected named-pipe client handle.
    unsafe { SetNamedPipeHandleState(pipe.0, Some(&mode), None, None) }
        .context("failed to set remote flintctl pipe mode")?;
    Ok(Some(pipe))
}

fn read_endpoint_reply(pipe: &mut Pipe) -> Result<EndpointReply> {
    let frame = read_message(pipe, MAX_RESPONSE_BYTES + FRAME_LENGTH_BYTES)?;
    let payload = agent_control_protocol::decode_frame(&frame, MAX_RESPONSE_BYTES)?;
    serde_json::from_slice(payload).context("failed to decode remote endpoint response")
}

fn create_pipe(name: &str, logon_sid: &str, first: bool) -> Result<Pipe> {
    let descriptor = SecurityDescriptor::new(logon_sid)?;
    let security_attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    let name = HSTRING::from(name);
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first {
        open_mode =
            FILE_FLAGS_AND_ATTRIBUTES(open_mode.0 | FILE_FLAG_FIRST_PIPE_INSTANCE.0 | WRITE_DAC.0);
    }
    let mode = PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: inputs remain valid for the duration of CreateNamedPipeW.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            open_mode,
            mode,
            PIPE_INSTANCE_COUNT as u32,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(&security_attributes),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(WindowsError::from_win32()).context("CreateNamedPipeW failed");
    }
    Ok(Pipe(handle))
}

fn read_message(pipe: &Pipe, maximum: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0; maximum];
    let mut transferred = 0;
    // SAFETY: buffer is valid and pipe owns a connected handle.
    unsafe { ReadFile(pipe.0, Some(&mut bytes), Some(&mut transferred), None) }
        .context("failed to read remote flintctl named pipe")?;
    bytes.truncate(transferred as usize);
    Ok(bytes)
}

fn write_all(pipe: &Pipe, bytes: &[u8]) -> Result<()> {
    let mut transferred = 0;
    // SAFETY: bytes remain valid and pipe owns a connected handle.
    unsafe { WriteFile(pipe.0, Some(bytes), Some(&mut transferred), None) }
        .context("failed to write remote flintctl named pipe")?;
    if transferred as usize != bytes.len() {
        bail!(
            "named-pipe write stopped after {transferred} of {} bytes",
            bytes.len()
        );
    }
    Ok(())
}

struct Pipe(HANDLE);

// SAFETY: Win32 handles can move between threads, and Pipe closes its handle once.
unsafe impl Send for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: Pipe owns this handle.
        unsafe { CloseHandle(self.0) }.ok();
    }
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn new_with_access(logon_sid: &str, access: &str) -> Result<Self> {
        let sddl = HSTRING::from(format!("D:P(A;;{access};;;SY)(A;;{access};;;{logon_sid})"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: descriptor receives LocalAlloc-owned storage.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .context("failed to construct remote flintctl pipe security descriptor")?;
        Ok(Self(descriptor))
    }

    fn new(logon_sid: &str) -> Result<Self> {
        Self::new_with_access(logon_sid, "GA")
    }
}

fn restrict_pipe_dacl(pipe: &Pipe, logon_sid: &str) -> Result<()> {
    let descriptor = SecurityDescriptor::new_with_access(logon_sid, "0x0012018B")?;
    let mut dacl_present = false.into();
    let mut dacl_defaulted = false.into();
    let mut dacl = std::ptr::null_mut();
    // SAFETY: outputs are writable and descriptor remains alive through the call.
    unsafe {
        GetSecurityDescriptorDacl(
            descriptor.0,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        )
    }
    .context("failed to read remote flintctl named-pipe DACL")?;
    if !dacl_present.as_bool() || dacl.is_null() {
        bail!("remote flintctl named-pipe security descriptor has no DACL");
    }
    // SAFETY: pipe is live and dacl remains valid through SetSecurityInfo.
    unsafe {
        SetSecurityInfo(
            pipe.0,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(dacl),
            None,
        )
    }
    .ok()
    .context("failed to restrict remote flintctl named-pipe DACL")
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: the conversion API allocated this descriptor with LocalAlloc.
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

fn current_logon_sid_string() -> Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: token points to writable storage.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .context("OpenProcessToken failed")?;
    let token = Pipe(token);
    let mut required = 0;
    // SAFETY: this null-buffer call only reports the required size.
    let size_result = unsafe { GetTokenInformation(token.0, TokenGroups, None, 0, &mut required) };
    if let Err(error) = size_result
        && error.code() != WIN32_ERROR(ERROR_INSUFFICIENT_BUFFER.0).into()
    {
        return Err(error).context("failed to size token groups");
    }
    let mut buffer = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
    // SAFETY: buffer has the exact requested byte capacity and suitable alignment.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenGroups,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            required,
            &mut required,
        )
    }
    .context("failed to read token groups")?;
    let groups = buffer.as_ptr().cast::<TOKEN_GROUPS>();
    // SAFETY: GetTokenInformation initialized GroupCount entries.
    let groups = unsafe {
        std::slice::from_raw_parts((*groups).Groups.as_ptr(), (*groups).GroupCount as usize)
    };
    let logon_group = groups
        .iter()
        .find(|group| group.Attributes & SE_GROUP_LOGON_ID as u32 == SE_GROUP_LOGON_ID as u32)
        .ok_or_else(|| anyhow!("process token has no logon SID"))?;
    let mut sid = PWSTR::null();
    // SAFETY: SID points into the live token buffer and sid receives LocalAlloc storage.
    unsafe { ConvertSidToStringSidW(logon_group.Sid, &mut sid) }
        .context("failed to format logon SID")?;
    // SAFETY: the conversion API returned a terminated UTF-16 string.
    let value = unsafe { sid.to_string() }.context("logon SID is not valid UTF-16");
    // SAFETY: ConvertSidToStringSidW allocated sid with LocalAlloc.
    unsafe { LocalFree(Some(HLOCAL(sid.0.cast()))) };
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_pipe_accepts_current_logon_and_reports_peer_process() {
        let name = format!(
            r"\\.\pipe\flint-remote-control-test-{}",
            uuid::Uuid::new_v4().simple()
        );
        let logon_sid = current_logon_sid_string().expect("current logon SID");
        let pipe = create_pipe(&name, &logon_sid, true).expect("create named pipe");
        restrict_pipe_dacl(&pipe, &logon_sid).expect("restrict named pipe");
        let request = ControlRequest::current(ControlCommand::Status);
        let client = std::thread::spawn({
            move || {
                let pipe = loop {
                    if let Some(pipe) = open_pipe(&name).expect("open named pipe") {
                        break pipe;
                    }
                };
                let frame = frame_payload(
                    &serde_json::to_vec(&request).expect("encode request"),
                    MAX_REQUEST_BYTES,
                )
                .expect("frame request");
                write_all(&pipe, &frame).expect("write request");
            }
        });

        // SAFETY: pipe owns a listening named-pipe server handle.
        if let Err(error) = unsafe { ConnectNamedPipe(pipe.0, None) }
            && error.code() != WIN32_ERROR(ERROR_PIPE_CONNECTED.0).into()
        {
            panic!("connect named pipe: {error}");
        }
        let mut peer_process_id = 0;
        // SAFETY: pipe is connected and the output points to valid storage.
        unsafe { GetNamedPipeClientProcessId(pipe.0, &mut peer_process_id) }
            .expect("get peer process id");
        assert_eq!(peer_process_id, std::process::id());
        let frame =
            read_message(&pipe, MAX_REQUEST_BYTES + FRAME_LENGTH_BYTES).expect("read request");
        let payload = agent_control_protocol::decode_frame(&frame, MAX_REQUEST_BYTES)
            .expect("decode request");
        let decoded: ControlRequest = serde_json::from_slice(payload).expect("parse request");
        assert!(matches!(decoded.command, ControlCommand::Status));
        client.join().expect("client thread");
    }
}
