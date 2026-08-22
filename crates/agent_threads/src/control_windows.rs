use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use agent_control_protocol::{
    ControlRequest, ControlResponse, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, decode_frame,
    frame_payload,
};
use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{App, Task};
use util::ResultExt as _;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_INSUFFICIENT_BUFFER, ERROR_IO_PENDING, ERROR_MORE_DATA,
    ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
    WAIT_TIMEOUT, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    SE_KERNEL_OBJECT, SetSecurityInfo,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_GROUPS,
    TOKEN_QUERY, TokenGroups,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    PIPE_ACCESS_DUPLEX, ReadFile, WRITE_DAC, WriteFile,
};
use windows::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use windows::Win32::System::SystemServices::SE_GROUP_LOGON_ID;
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken};
use windows::core::{Error as WindowsError, HSTRING, PCWSTR, PWSTR};

use crate::control::{self, error_response};
use crate::store::AgentThreadStore;

const SERVING_INSTANCE_COUNT: usize = 4;
const PIPE_BUFFER_BYTES: u32 = 16 * 1024;
const IO_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(60);
const RESPONSE_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

struct DispatchRequest {
    peer_pid: u32,
    request: ControlRequest,
    response_sender: mpsc::SyncSender<ControlResponse>,
}

pub(crate) struct ControlServerHandle {
    shutdown: Arc<AtomicBool>,
    _dispatcher: Task<()>,
    threads: Arc<Mutex<Option<Vec<JoinHandle<()>>>>>,
    marker_path: std::path::PathBuf,
    marker_written: bool,
}

impl ControlServerHandle {
    fn remove_owned_marker(&mut self) {
        if self.marker_written {
            std::fs::remove_file(&self.marker_path).log_err();
            self.marker_written = false;
        }
    }

    #[cfg(test)]
    fn stop_and_join(mut self) {
        self.remove_owned_marker();
        self.shutdown.store(true, Ordering::Release);
        join_threads(&self.threads);
    }
}

impl Drop for ControlServerHandle {
    fn drop(&mut self) {
        // Remove discovery while every persistent serving handle is still
        // open, so a replacement owner cannot publish a marker that this
        // instance then removes.
        self.remove_owned_marker();
        self.shutdown.store(true, Ordering::Release);
        let threads = self.threads.clone();
        match std::thread::Builder::new()
            .name("agent-control-reaper".into())
            .spawn(move || join_threads(&threads))
        {
            Ok(_reaper) => {}
            Err(error) => {
                log::error!("agent_threads: failed to start control-server reaper: {error}")
            }
        }
    }
}

pub(crate) fn init(cx: &mut App) {
    let scope = match agent_control_protocol::WindowsControlScope::current() {
        Ok(scope) => scope,
        Err(error) => {
            log::error!("agent_threads: could not derive Windows control scope: {error:#}");
            return;
        }
    };
    if let Some(handle) = start(scope, None, cx) {
        AgentThreadStore::global(cx).update(cx, |store, _cx| store.hold_control_server(handle));
    }
}

fn start(
    scope: agent_control_protocol::WindowsControlScope,
    executable_override: Option<std::path::PathBuf>,
    cx: &mut App,
) -> Option<ControlServerHandle> {
    let pipe_name = scope.pipe_name().to_owned();
    let marker_path = scope.executable_location_path().to_path_buf();
    match executable_override.clone() {
        Some(executable) => control::write_executable_location_for(&marker_path, executable),
        None => control::write_executable_location(&marker_path),
    };
    let logon_sid = match current_logon_sid_string() {
        Ok(logon_sid) => logon_sid,
        Err(error) => {
            log::error!("agent_threads: could not obtain the current logon SID: {error:#}");
            return None;
        }
    };

    let first_instance = match create_pipe(
        &pipe_name,
        &logon_sid,
        true,
        true,
        SERVING_INSTANCE_COUNT,
    ) {
        Ok(first_instance) => first_instance,
        Err(error) => {
            log::info!(
                "agent_threads: another Flint instance may own agent control pipe {pipe_name}; disabling this server: {error:#}"
            );
            return None;
        }
    };

    let mut serving_pipes = Vec::with_capacity(SERVING_INSTANCE_COUNT);
    serving_pipes.push(first_instance);
    for _ in 1..SERVING_INSTANCE_COUNT {
        match create_pipe(&pipe_name, &logon_sid, false, true, SERVING_INSTANCE_COUNT) {
            Ok(pipe) => serving_pipes.push(pipe),
            Err(error) => {
                log::error!("agent_threads: failed to create named-pipe pool: {error:#}");
                return None;
            }
        }
    }
    let Some(first_instance) = serving_pipes.first() else {
        log::error!("agent_threads: named-pipe pool was unexpectedly empty");
        return None;
    };
    if let Err(error) = restrict_pipe_dacl(first_instance, &logon_sid) {
        log::error!("agent_threads: failed to restrict named-pipe DACL: {error:#}");
        return None;
    }

    let (dispatch_sender, dispatch_receiver) =
        async_channel::bounded::<DispatchRequest>(SERVING_INSTANCE_COUNT * 2);
    let store = AgentThreadStore::global(cx);
    let dispatcher = cx.spawn(async move |cx| {
        while let Ok(dispatch_request) = dispatch_receiver.recv().await {
            let store = store.clone();
            cx.spawn(async move |cx| {
                let response = control::dispatch(
                    dispatch_request.peer_pid,
                    &dispatch_request.request,
                    &store,
                    cx,
                )
                .await;
                if dispatch_request.response_sender.send(response).is_err() {
                    log::debug!(
                        "agent_threads: named-pipe client stopped waiting for its response"
                    );
                }
            })
            .detach();
        }
    });

    let shutdown = Arc::new(AtomicBool::new(false));
    let mut worker_threads = Vec::with_capacity(SERVING_INSTANCE_COUNT);
    for (slot, pipe) in serving_pipes.into_iter().enumerate() {
        let dispatch_sender = dispatch_sender.clone();
        let shutdown_for_worker = shutdown.clone();
        match std::thread::Builder::new()
            .name(format!("agent-control-pipe-{slot}"))
            .spawn(move || worker_loop(pipe, dispatch_sender, shutdown_for_worker))
        {
            Ok(worker) => worker_threads.push(worker),
            Err(error) => {
                shutdown.store(true, Ordering::Release);
                log::error!("agent_threads: failed to start named-pipe worker: {error}");
                join_owned_threads(worker_threads);
                return None;
            }
        }
    }

    drop(dispatch_sender);
    Some(ControlServerHandle {
        shutdown,
        _dispatcher: dispatcher,
        threads: Arc::new(Mutex::new(Some(worker_threads))),
        marker_path,
        marker_written: false,
    })
}

fn join_threads(threads: &Mutex<Option<Vec<JoinHandle<()>>>>) {
    let workers = match threads.lock() {
        Ok(mut threads) => threads.take(),
        Err(error) => {
            log::error!("agent_threads: control-server thread lock was poisoned: {error}");
            None
        }
    };
    if let Some(workers) = workers {
        join_owned_threads(workers);
    }
}

fn join_owned_threads(workers: Vec<JoinHandle<()>>) {
    for worker in workers {
        if let Err(error) = worker.join() {
            log::error!("agent_threads: named-pipe worker panicked: {error:?}");
        }
    }
}

fn worker_loop(
    pipe: Pipe,
    dispatch_sender: async_channel::Sender<DispatchRequest>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Acquire) {
        let result = serve_one(&pipe, &dispatch_sender, &shutdown);
        // SAFETY: pipe is a valid server instance. Disconnect is also the
        // recovery path after malformed input or a failed response write.
        if let Err(error) = unsafe { DisconnectNamedPipe(pipe.0) }
            && !shutdown.load(Ordering::Acquire)
        {
            log::debug!("agent_threads: named-pipe disconnect reported {error}");
        }
        if let Err(error) = result
            && !shutdown.load(Ordering::Acquire)
        {
            log::warn!("agent_threads: named-pipe request failed: {error:#}");
        }
    }
}

fn serve_one(
    pipe: &Pipe,
    dispatch_sender: &async_channel::Sender<DispatchRequest>,
    shutdown: &AtomicBool,
) -> Result<()> {
    connect(pipe, shutdown)?;

    let response = match read_message(&pipe, MAX_REQUEST_BYTES + 4, shutdown) {
        Ok(request_frame) => match decode_frame(&request_frame, MAX_REQUEST_BYTES) {
            Ok(request_bytes) => match serde_json::from_slice::<ControlRequest>(request_bytes) {
                Ok(request) => {
                    let mut peer_pid = 0;
                    // SAFETY: pipe is connected and peer_pid is valid writable storage.
                    match unsafe { GetNamedPipeClientProcessId(pipe.0, &mut peer_pid) } {
                        Ok(()) => dispatch_request(dispatch_sender, peer_pid, request, shutdown),
                        Err(error) => error_response(format_args!(
                            "could not determine caller identity: {error}"
                        )),
                    }
                }
                Err(error) => error_response(format_args!("malformed request: {error}")),
            },
            Err(error) => error_response(format_args!("invalid request frame: {error}")),
        },
        Err(error) => error_response(format_args!("could not read request: {error:#}")),
    };
    let response_bytes = serde_json::to_vec(&response).context("failed to encode response")?;
    let response_frame =
        frame_payload(&response_bytes, MAX_RESPONSE_BYTES).context("failed to frame response")?;
    write_all(pipe, &response_frame, shutdown)?;
    wait_for_client_close(pipe, shutdown)?;
    Ok(())
}

fn dispatch_request(
    sender: &async_channel::Sender<DispatchRequest>,
    peer_pid: u32,
    request: ControlRequest,
    shutdown: &AtomicBool,
) -> ControlResponse {
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    let mut dispatch_request = DispatchRequest {
        peer_pid,
        request,
        response_sender,
    };
    loop {
        match sender.try_send(dispatch_request) {
            Ok(()) => break,
            Err(async_channel::TrySendError::Full(returned)) => {
                if shutdown.load(Ordering::Acquire) {
                    return error_response("control server is shutting down");
                }
                dispatch_request = returned;
                std::thread::park_timeout(IO_POLL_INTERVAL);
            }
            Err(async_channel::TrySendError::Closed(_)) => {
                return error_response("control dispatcher stopped");
            }
        }
    }
    let started = std::time::Instant::now();
    loop {
        if shutdown.load(Ordering::Acquire) {
            return error_response("control server is shutting down");
        }
        let remaining = DISPATCH_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return error_response("control request timed out");
        }
        match response_receiver.recv_timeout(remaining.min(IO_POLL_INTERVAL)) {
            Ok(response) => return response,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return error_response("control dispatcher stopped");
            }
        }
    }
}

struct Pipe(HANDLE);

// SAFETY: a Win32 kernel handle may be transferred between threads. Pipe keeps
// exclusive ownership and closes it exactly once.
unsafe impl Send for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: Pipe exclusively owns the handle.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            log::error!("agent_threads: failed to close named pipe: {error}");
        }
    }
}

fn create_pipe(
    name: &str,
    logon_sid: &str,
    first: bool,
    allow_create_instance: bool,
    maximum_instances: usize,
) -> Result<Pipe> {
    let descriptor = SecurityDescriptor::new(logon_sid, allow_create_instance)?;
    let security_attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    let name = HSTRING::from(name);
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode =
            FILE_FLAGS_AND_ATTRIBUTES(open_mode.0 | FILE_FLAG_FIRST_PIPE_INSTANCE.0 | WRITE_DAC.0);
    }
    let pipe_mode =
        PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: name and security descriptor are valid for the duration of the call.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            open_mode,
            pipe_mode,
            maximum_instances as u32,
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

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn new(logon_sid: &str, allow_create_instance: bool) -> Result<Self> {
        // FILE_READ_DATA | FILE_WRITE_DATA | SYNCHRONIZE. FILE_CREATE_PIPE_INSTANCE
        // is present only while Flint creates the fixed pool before publishing it.
        let access_mask = if allow_create_instance {
            "GA"
        } else {
            // FILE_GENERIC_READ | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES,
            // without FILE_APPEND_DATA / FILE_CREATE_PIPE_INSTANCE.
            "0x0012018B"
        };
        let sddl = HSTRING::from(format!(
            "D:P(A;;{access_mask};;;SY)(A;;{access_mask};;;{logon_sid})"
        ));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: descriptor receives a LocalAlloc-owned security descriptor.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .context("failed to construct named-pipe security descriptor")?;
        Ok(Self(descriptor))
    }
}

fn restrict_pipe_dacl(pipe: &Pipe, logon_sid: &str) -> Result<()> {
    let descriptor = SecurityDescriptor::new(logon_sid, false)?;
    let mut dacl_present = false.into();
    let mut dacl_defaulted = false.into();
    let mut dacl = std::ptr::null_mut();
    // SAFETY: outputs are writable and descriptor remains alive through SetSecurityInfo.
    unsafe {
        GetSecurityDescriptorDacl(
            descriptor.0,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        )
    }
    .context("failed to read restricted named-pipe DACL")?;
    if !dacl_present.as_bool() || dacl.is_null() {
        bail!("restricted named-pipe security descriptor has no DACL");
    }
    // SAFETY: pipe is a live kernel handle and dacl remains valid for the call.
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
    .context("failed to apply restricted named-pipe DACL")
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by LocalAlloc through the conversion API.
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

fn current_logon_sid_string() -> Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: token points to writable storage and is owned below on success.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .context("OpenProcessToken failed")?;
    let token = Pipe(token);
    let mut required = 0;
    // SAFETY: the null buffer query populates required.
    let size_result = unsafe { GetTokenInformation(token.0, TokenGroups, None, 0, &mut required) };
    if let Err(error) = size_result
        && error.code() != WIN32_ERROR(ERROR_INSUFFICIENT_BUFFER.0).into()
    {
        return Err(error).context("failed to size token groups");
    }
    // Token information contains pointers and SID structures, so its backing
    // allocation must be pointer-aligned rather than a Vec<u8> allocation.
    let words = (required as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0usize; words];
    // SAFETY: buffer has exactly the size requested by the preceding call.
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
    // SAFETY: GetTokenInformation initialized TOKEN_GROUPS and GroupCount entries.
    let groups = unsafe {
        std::slice::from_raw_parts((*groups).Groups.as_ptr(), (*groups).GroupCount as usize)
    };
    let logon_group = groups
        .iter()
        .find(|group| group.Attributes & SE_GROUP_LOGON_ID as u32 == SE_GROUP_LOGON_ID as u32)
        .ok_or_else(|| anyhow!("process token has no logon SID"))?;
    let mut sid_string = PWSTR::null();
    // SAFETY: the SID points into the live token buffer and sid_string receives LocalAlloc memory.
    unsafe { ConvertSidToStringSidW(logon_group.Sid, &mut sid_string) }
        .context("failed to format logon SID")?;
    // SAFETY: ConvertSidToStringSidW returns a terminated UTF-16 string.
    let result = unsafe { sid_string.to_string() }.context("logon SID is not valid UTF-16");
    // SAFETY: sid_string was allocated with LocalAlloc by ConvertSidToStringSidW.
    unsafe { LocalFree(Some(HLOCAL(sid_string.0.cast()))) };
    result
}

fn connect(pipe: &Pipe, shutdown: &AtomicBool) -> Result<()> {
    let event = create_overlapped_event()?;
    let mut overlapped = OVERLAPPED::default();
    overlapped.hEvent = event.0;
    // SAFETY: pipe and overlapped remain valid until terminal completion.
    let initial = unsafe { ConnectNamedPipe(pipe.0, Some(&mut overlapped)) };
    if initial.is_ok() {
        return Ok(());
    }
    if let Err(error) = &initial
        && error.code() == WIN32_ERROR(ERROR_PIPE_CONNECTED.0).into()
    {
        return Ok(());
    }
    finish_io(pipe.0, &mut overlapped, initial, shutdown).map(|_| ())
}

fn read_message(pipe: &Pipe, maximum_bytes: usize, shutdown: &AtomicBool) -> Result<Vec<u8>> {
    let mut message = Vec::new();
    loop {
        let remaining = maximum_bytes.saturating_sub(message.len());
        if remaining == 0 {
            bail!("request exceeds the {maximum_bytes}-byte protocol limit");
        }
        let mut chunk = vec![0; remaining.min(PIPE_BUFFER_BYTES as usize)];
        let event = create_overlapped_event()?;
        let mut overlapped = OVERLAPPED::default();
        overlapped.hEvent = event.0;
        // SAFETY: chunk and overlapped live through finish_io.
        let initial = unsafe { ReadFile(pipe.0, Some(&mut chunk), None, Some(&mut overlapped)) };
        match finish_io(pipe.0, &mut overlapped, initial, shutdown) {
            Ok(transferred) => {
                chunk.truncate(transferred as usize);
                message.extend_from_slice(&chunk);
                return Ok(message);
            }
            Err(error)
                if windows_error_code(&error) == Some(WIN32_ERROR(ERROR_MORE_DATA.0).into()) =>
            {
                let mut transferred = 0;
                // SAFETY: the completed operation's byte count is queried without waiting.
                if let Err(count_error) =
                    unsafe { GetOverlappedResult(pipe.0, &overlapped, &mut transferred, false) }
                    && count_error.code() != WIN32_ERROR(ERROR_MORE_DATA.0).into()
                {
                    return Err(count_error).context("failed to obtain partial message size");
                }
                chunk.truncate(transferred as usize);
                message.extend_from_slice(&chunk);
            }
            Err(error) => return Err(error),
        }
    }
}

fn write_all(pipe: &Pipe, bytes: &[u8], shutdown: &AtomicBool) -> Result<()> {
    let event = create_overlapped_event()?;
    let mut overlapped = OVERLAPPED::default();
    overlapped.hEvent = event.0;
    // SAFETY: bytes and overlapped live through finish_io.
    let initial = unsafe { WriteFile(pipe.0, Some(bytes), None, Some(&mut overlapped)) };
    let transferred = finish_io(pipe.0, &mut overlapped, initial, shutdown)?;
    if transferred as usize != bytes.len() {
        bail!(
            "named-pipe write completed after {transferred} of {} bytes",
            bytes.len()
        );
    }
    Ok(())
}

fn wait_for_client_close(pipe: &Pipe, shutdown: &AtomicBool) -> Result<()> {
    let started = std::time::Instant::now();
    'client: loop {
        if shutdown.load(Ordering::Acquire) || started.elapsed() >= RESPONSE_DRAIN_TIMEOUT {
            return Ok(());
        }
        let event = create_overlapped_event()?;
        let mut overlapped = OVERLAPPED::default();
        overlapped.hEvent = event.0;
        let mut unexpected_byte = [0u8; 1];
        // SAFETY: buffer and overlapped remain alive until the operation terminates below.
        let initial = unsafe {
            ReadFile(
                pipe.0,
                Some(&mut unexpected_byte),
                None,
                Some(&mut overlapped),
            )
        };
        if initial.is_ok() {
            // A protocol client sends exactly one request message. Discard any
            // extra input without treating it as proof that the response was
            // consumed, then continue waiting for the client to close.
            continue;
        }
        if let Err(error) = &initial {
            if is_closed_pipe_error(error.code()) {
                return Ok(());
            }
            if error.code() == WIN32_ERROR(ERROR_MORE_DATA.0).into() {
                continue;
            }
            if error.code() != WIN32_ERROR(ERROR_IO_PENDING.0).into() {
                return Err(error.clone())
                    .context("failed while waiting for named-pipe client close");
            }
        }

        let mut transferred = 0;
        loop {
            // SAFETY: operation storage remains live and the event belongs to this OVERLAPPED.
            match unsafe {
                GetOverlappedResultEx(
                    pipe.0,
                    &overlapped,
                    &mut transferred,
                    IO_POLL_INTERVAL.as_millis() as u32,
                    false,
                )
            } {
                Ok(()) => continue 'client,
                Err(error) if is_closed_pipe_error(error.code()) => return Ok(()),
                Err(error) if error.code() == WIN32_ERROR(WAIT_TIMEOUT.0).into() => {
                    if shutdown.load(Ordering::Acquire)
                        || started.elapsed() >= RESPONSE_DRAIN_TIMEOUT
                    {
                        // SAFETY: cancel exactly this pending read and observe terminal completion.
                        if let Err(cancel_error) = unsafe { CancelIoEx(pipe.0, Some(&overlapped)) }
                        {
                            log::debug!(
                                "agent_threads: response-drain cancellation reported {cancel_error}"
                            );
                        }
                        // SAFETY: waiting prevents operation storage from being released prematurely.
                        if let Err(terminal_error) = unsafe {
                            GetOverlappedResult(pipe.0, &overlapped, &mut transferred, true)
                        } {
                            log::debug!(
                                "agent_threads: response-drain read ended with {terminal_error}"
                            );
                        }
                        return Ok(());
                    }
                }
                Err(error) => {
                    return Err(error).context("failed while waiting for named-pipe client close");
                }
            }
        }
    }
}

fn is_closed_pipe_error(code: windows::core::HRESULT) -> bool {
    code == WIN32_ERROR(ERROR_BROKEN_PIPE.0).into() || code == WIN32_ERROR(ERROR_NO_DATA.0).into()
}

fn create_overlapped_event() -> Result<Pipe> {
    // SAFETY: default security, manual reset, initially nonsignaled, unnamed event.
    let event = unsafe { CreateEventW(None, true, false, None) }
        .context("failed to create named-pipe overlapped event")?;
    Ok(Pipe(event))
}

fn finish_io(
    handle: HANDLE,
    overlapped: &mut OVERLAPPED,
    initial: windows::core::Result<()>,
    shutdown: &AtomicBool,
) -> Result<u32> {
    let mut transferred = 0;
    if let Err(error) = &initial
        && error.code() != WIN32_ERROR(ERROR_IO_PENDING.0).into()
    {
        return Err(error.clone().into());
    }
    loop {
        // SAFETY: all operation storage stays alive while polling.
        match unsafe {
            GetOverlappedResultEx(
                handle,
                overlapped,
                &mut transferred,
                IO_POLL_INTERVAL.as_millis() as u32,
                false,
            )
        } {
            Ok(()) => return Ok(transferred),
            Err(error) if error.code() == WIN32_ERROR(WAIT_TIMEOUT.0).into() => {
                if shutdown.load(Ordering::Acquire) {
                    // SAFETY: cancel exactly this pending operation.
                    if let Err(cancel_error) = unsafe { CancelIoEx(handle, Some(overlapped)) } {
                        log::debug!(
                            "agent_threads: named-pipe cancellation reported {cancel_error}"
                        );
                    }
                    // SAFETY: wait for terminal completion before buffers are released.
                    if let Err(terminal_error) =
                        unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, true) }
                    {
                        log::debug!(
                            "agent_threads: cancelled named-pipe I/O ended with {terminal_error}"
                        );
                    }
                    bail!("control server is shutting down");
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn windows_error_code(error: &anyhow::Error) -> Option<windows::core::HRESULT> {
    error.downcast_ref::<WindowsError>().map(WindowsError::code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use settings::SettingsStore;

    struct TestServer {
        name: String,
        shutdown: Arc<AtomicBool>,
        workers: Vec<JoinHandle<()>>,
        dispatcher: JoinHandle<()>,
    }

    impl TestServer {
        fn stop(self) {
            self.shutdown.store(true, Ordering::Release);
            join_owned_threads(self.workers);
            self.dispatcher
                .join()
                .expect("test dispatcher thread panicked");
        }
    }

    fn unique_pipe_name(label: &str) -> String {
        format!(
            r"\\.\pipe\flint-agent-control-test-{}-{label}",
            uuid::Uuid::new_v4()
        )
    }

    fn start_test_server(
        label: &str,
        instance_count: usize,
        handler: impl Fn(ControlRequest) -> ControlResponse + Send + Sync + 'static,
    ) -> TestServer {
        assert!(instance_count > 0);
        let name = unique_pipe_name(label);
        let sid = current_logon_sid_string().expect("read current logon SID");
        let mut pipes = Vec::with_capacity(instance_count);
        pipes.push(
            create_pipe(&name, &sid, true, true, instance_count)
                .expect("create first test serving instance"),
        );
        for _ in 1..instance_count {
            pipes.push(
                create_pipe(&name, &sid, false, true, instance_count)
                    .expect("create test serving instance"),
            );
        }
        restrict_pipe_dacl(&pipes[0], &sid).expect("restrict test pipe DACL");

        let (dispatch_sender, dispatch_receiver) =
            async_channel::bounded::<DispatchRequest>(instance_count * 2);
        let handler = Arc::new(handler);
        let dispatcher = std::thread::spawn(move || {
            while let Ok(request) = dispatch_receiver.recv_blocking() {
                let handler = handler.clone();
                std::thread::spawn(move || {
                    let response = handler(request.request);
                    request
                        .response_sender
                        .send(response)
                        .expect("test worker stopped waiting for its response");
                });
            }
        });

        let shutdown = Arc::new(AtomicBool::new(false));
        let workers = pipes
            .into_iter()
            .map(|pipe| {
                let dispatch_sender = dispatch_sender.clone();
                let shutdown = shutdown.clone();
                std::thread::spawn(move || worker_loop(pipe, dispatch_sender, shutdown))
            })
            .collect();
        drop(dispatch_sender);

        TestServer {
            name,
            shutdown,
            workers,
            dispatcher,
        }
    }

    fn open_test_client(name: &str, timeout: Duration) -> Result<Pipe> {
        use windows::Win32::Foundation::GENERIC_READ;
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, OPEN_EXISTING,
        };
        use windows::Win32::System::Pipes::{SetNamedPipeHandleState, WaitNamedPipeW};

        let name = HSTRING::from(name);
        // SAFETY: name is a valid terminated pipe name for this call.
        if !unsafe {
            WaitNamedPipeW(
                PCWSTR(name.as_ptr()),
                timeout.as_millis().min(u32::MAX as u128) as u32,
            )
        }
        .as_bool()
        {
            return Err(WindowsError::from_win32()).context("test pipe stayed busy");
        }
        // SAFETY: all pointers are valid and the returned handle is owned by Pipe.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_READ.0 | FILE_WRITE_DATA.0 | FILE_WRITE_ATTRIBUTES.0,
                Default::default(),
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            )
        }
        .context("connect test pipe")?;
        let client = Pipe(handle);
        let mode = PIPE_READMODE_MESSAGE;
        // SAFETY: client is a connected named-pipe handle.
        unsafe { SetNamedPipeHandleState(client.0, Some(&mode), None, None) }
            .context("set test client message mode")?;
        Ok(client)
    }

    fn transact_raw(name: &str, request: &[u8]) -> Result<ControlResponse> {
        let client = open_test_client(name, Duration::from_secs(2))?;
        let mut written = 0;
        // SAFETY: client is synchronous and all input/output storage lives through the call.
        unsafe { WriteFile(client.0, Some(request), Some(&mut written), None) }
            .context("write test request")?;
        if written as usize != request.len() {
            bail!(
                "test request write completed after {written} of {} bytes",
                request.len()
            );
        }

        let mut response = Vec::new();
        loop {
            let mut chunk = vec![0; PIPE_BUFFER_BYTES as usize];
            let mut read = 0;
            // SAFETY: client is synchronous and chunk/read remain valid for the call.
            match unsafe { ReadFile(client.0, Some(&mut chunk), Some(&mut read), None) } {
                Ok(()) => {
                    chunk.truncate(read as usize);
                    response.extend_from_slice(&chunk);
                    break;
                }
                Err(error) if error.code() == WIN32_ERROR(ERROR_MORE_DATA.0).into() => {
                    chunk.truncate(read as usize);
                    response.extend_from_slice(&chunk);
                    if response.len() > MAX_RESPONSE_BYTES {
                        bail!("test response exceeded protocol limit");
                    }
                }
                Err(error) => return Err(error).context("read test response"),
            }
        }
        drop(client);
        serde_json::from_slice(&response).context("decode test response")
    }

    fn transact_request(name: &str, request: &ControlRequest) -> Result<ControlResponse> {
        let request = serde_json::to_vec(request).context("encode test request")?;
        transact_raw(name, &request)
    }

    fn prompt_request(prompt: impl Into<String>) -> ControlRequest {
        ControlRequest::CreateThread(agent_control_protocol::CreateThreadRequest {
            worktree: agent_control_protocol::CreateThreadWorktree::Current,
            name: None,
            agent: "codex".to_string(),
            prompt: prompt.into(),
        })
    }

    fn error_message(response: ControlResponse) -> String {
        match response {
            ControlResponse::Error { message } => message,
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    fn init_gpui_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            cx.set_global(db::AppDatabase::test_new());
            localization::init(localization::UiLanguage::English, cx)
                .expect("test localization must load");
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            terminal_view::init(cx);
            crate::init(cx);
        });
    }

    #[test]
    fn process_token_contains_a_logon_sid() {
        let sid = current_logon_sid_string().expect("read current logon SID");
        assert!(sid.starts_with("S-1-5-5-"), "unexpected logon SID: {sid}");
    }

    #[test]
    fn owner_can_create_first_and_additional_serving_instances() {
        let name = unique_pipe_name("instances");
        let sid = current_logon_sid_string().expect("read current logon SID");
        let first = create_pipe(&name, &sid, true, true, 3).expect("create first serving instance");
        let _first =
            create_pipe(&name, &sid, false, true, 3).expect("create first serving instance");
        let second =
            create_pipe(&name, &sid, false, true, 3).expect("create second serving instance");
        restrict_pipe_dacl(&first, &sid).expect("restrict shared pipe DACL");
        drop(second);
        assert!(
            create_pipe(&name, &sid, false, true, 3).is_err(),
            "the published DACL must deny FILE_CREATE_PIPE_INSTANCE"
        );
    }

    #[test]
    fn restricted_pipe_accepts_protocol_access_and_reports_client_pid() {
        use windows::Win32::Foundation::GENERIC_READ;
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_OVERLAPPED, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
            OPEN_EXISTING,
        };

        let name = unique_pipe_name("peer-pid");
        let sid = current_logon_sid_string().expect("read current logon SID");
        let serving = create_pipe(&name, &sid, true, true, 1).expect("create serving instance");
        restrict_pipe_dacl(&serving, &sid).expect("restrict shared pipe DACL");

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_server = shutdown.clone();
        let server = std::thread::spawn(move || -> Result<u32> {
            connect(&serving, &shutdown_for_server)?;
            let mut peer_pid = 0;
            // SAFETY: serving is connected and peer_pid is writable.
            unsafe { GetNamedPipeClientProcessId(serving.0, &mut peer_pid) }
                .context("read named-pipe client PID")?;
            Ok(peer_pid)
        });
        let client_name = HSTRING::from(name);
        // SAFETY: the pipe name is valid and the returned handle is owned by Pipe.
        let client_result = unsafe {
            CreateFileW(
                PCWSTR(client_name.as_ptr()),
                GENERIC_READ.0 | FILE_WRITE_DATA.0 | FILE_WRITE_ATTRIBUTES.0,
                Default::default(),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };
        if let Err(client_error) = client_result {
            shutdown.store(true, Ordering::Release);
            let server_result = server.join().expect("server thread panicked");
            panic!(
                "protocol client access failed: {client_error}; server result: {server_result:?}"
            );
        }
        let client = Pipe(client_result.expect("checked above"));
        let peer_pid = server
            .join()
            .expect("server thread panicked")
            .expect("server connection failed");
        assert_eq!(peer_pid, std::process::id());
        drop(client);
    }

    #[test]
    fn process_creation_time_is_available_for_current_process() {
        assert!(control::windows_process_creation_time(std::process::id()).is_some());
    }

    #[test]
    fn message_framing_rejects_bad_input_and_recycles_the_instance() {
        let server = start_test_server("framing", 1, |request| match request {
            ControlRequest::CreateThread(request) => error_response(format!(
                "accepted prompt with {} bytes",
                request.prompt.len()
            )),
            ControlRequest::RetieThread(_) => error_response("accepted retie"),
        });

        let malformed = transact_raw(&server.name, b"{not-json").expect("malformed round trip");
        assert!(error_message(malformed).contains("malformed request"));

        let large_prompt = "x".repeat(PIPE_BUFFER_BYTES as usize * 3);
        let large = transact_request(&server.name, &prompt_request(large_prompt.clone()))
            .expect("large request round trip");
        assert_eq!(
            error_message(large),
            format!("accepted prompt with {} bytes", large_prompt.len())
        );

        let oversized = vec![b'x'; MAX_REQUEST_BYTES + 1];
        let oversized = transact_raw(&server.name, &oversized).expect("oversized round trip");
        assert!(error_message(oversized).contains("protocol limit"));

        let recovered = transact_request(&server.name, &prompt_request("after-error"))
            .expect("post-error round trip");
        assert_eq!(error_message(recovered), "accepted prompt with 11 bytes");

        let sid = current_logon_sid_string().expect("read current logon SID");
        assert!(
            create_pipe(&server.name, &sid, true, true, 1).is_err(),
            "the persistent first serving handle must retain endpoint ownership after recycling"
        );
        server.stop();
    }

    #[test]
    fn concurrent_pool_services_a_fast_request_while_another_dispatch_is_slow() {
        let (slow_started_sender, slow_started_receiver) = mpsc::sync_channel(1);
        let server = start_test_server("concurrent", 2, move |request| {
            let ControlRequest::CreateThread(request) = request else {
                return error_response("unexpected request");
            };
            if request.prompt == "slow" {
                slow_started_sender
                    .send(())
                    .expect("signal slow dispatch start");
                std::thread::sleep(Duration::from_millis(400));
            }
            error_response(request.prompt)
        });

        let slow_name = server.name.clone();
        let slow_client = std::thread::spawn(move || {
            transact_request(&slow_name, &prompt_request("slow")).expect("slow request round trip")
        });
        slow_started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("slow request reached dispatch");

        let started = std::time::Instant::now();
        let fast = transact_request(&server.name, &prompt_request("fast"))
            .expect("fast request round trip");
        assert_eq!(error_message(fast), "fast");
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "fast request was serialized behind the slow dispatch"
        );
        assert_eq!(
            error_message(slow_client.join().expect("slow client thread panicked")),
            "slow"
        );
        server.stop();
    }

    #[test]
    fn pool_exhaustion_is_bounded_and_shutdown_cancels_a_pending_read() {
        use windows::Win32::System::Pipes::WaitNamedPipeW;

        let server = start_test_server("shutdown", 1, |_| error_response("unexpected dispatch"));
        let client = open_test_client(&server.name, Duration::from_secs(2))
            .expect("occupy the only serving instance");
        let name = HSTRING::from(&server.name);
        let wait_started = std::time::Instant::now();
        // SAFETY: name is a valid terminated pipe name for this call.
        let available = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 100) };
        assert!(
            !available.as_bool(),
            "the occupied one-slot pool looked available"
        );
        assert!(wait_started.elapsed() < Duration::from_secs(1));

        let shutdown_started = std::time::Instant::now();
        server.stop();
        assert!(
            shutdown_started.elapsed() < Duration::from_secs(2),
            "shutdown did not cancel the pending server read"
        );
        drop(client);
    }

    #[gpui::test]
    async fn startup_publishes_its_session_marker_and_leaves_it_after_stop(
        cx: &mut TestAppContext,
    ) {
        // Native pipe workers wake the GPUI dispatcher from OS threads.
        cx.executor().allow_parking();
        init_gpui_test(cx);
        let temp = tempfile::tempdir().expect("create startup test directory");
        let helper = temp.path().join("flint-agent-control.exe");
        std::fs::write(&helper, b"test helper").expect("write helper fixture");
        let session_base = uuid::Uuid::new_v4().as_u128() as u32;
        let first_scope = agent_control_protocol::WindowsControlScope::for_session(
            temp.path().to_path_buf(),
            session_base,
        );
        let second_scope = agent_control_protocol::WindowsControlScope::for_session(
            temp.path().to_path_buf(),
            session_base.wrapping_add(1),
        );
        let first_marker = first_scope.executable_location_path().to_path_buf();
        let second_marker = second_scope.executable_location_path().to_path_buf();
        let second_name = second_scope.pipe_name().to_owned();

        let first = cx.update(|cx| {
            start(first_scope, Some(helper.clone()), cx).expect("start first session server")
        });
        let second = cx.update(|cx| {
            start(second_scope, Some(helper.clone()), cx).expect("start second session server")
        });
        for marker in [&first_marker, &second_marker] {
            let marker_json = std::fs::read(marker).expect("read startup marker");
            let location: agent_control_protocol::AgentControlLocation =
                serde_json::from_slice(&marker_json).expect("decode startup marker");
            assert_eq!(location.executable, helper);
        }

        first.stop_and_join();
        assert!(
            first_marker.exists(),
            "the marker is rewritten on the next launch, not removed on stop"
        );
        assert!(second_marker.exists());

        let (response_sender, response_receiver) = mpsc::sync_channel(1);
        let client = std::thread::spawn(move || {
            let response = transact_request(&second_name, &prompt_request("startup"));
            response_sender
                .send(response)
                .expect("send startup response");
        });
        let response = loop {
            match response_receiver.try_recv() {
                Ok(response) => break response.expect("startup request round trip"),
                Err(mpsc::TryRecvError::Empty) => {
                    cx.run_until_parked();
                    cx.executor().timer(Duration::from_millis(10)).await;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("startup client stopped without a response")
                }
            }
        };
        assert!(matches!(response, ControlResponse::NotReady));
        client.join().expect("startup client thread panicked");

        second.stop_and_join();
        assert!(second_marker.exists());
    }
}
