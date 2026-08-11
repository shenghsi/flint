//! `flint-agent-control`: the binary an agent thread's own CLI process (Codex,
//! Claude Code, etc.) invokes to ask Flint to re-tie itself to a different
//! worktree, or spawn a sibling thread. Talks to `agent_threads::control`'s
//! platform transport over `agent_control_protocol`'s wire types: a Unix
//! socket on Unix and a session-scoped named pipe on Windows.
//!
//! Sends bare, unauthenticated-looking requests on purpose: the server
//! establishes caller identity itself, from the kernel-reported PID of
//! whatever process actually connected to the socket (see
//! `agent_threads::control`'s peer-process resolution), not from
//! anything this binary presents. There is nothing here to mint, deliver,
//! or leak. The endpoint is computed identically by the server and client;
//! the platform-specific override is only for testing.

use std::path::PathBuf;

use agent_control_protocol::{
    ControlRequest, ControlResponse, ControlSuccess, CreateThreadRequest, CreateThreadWorktree,
    RetieThreadRequest,
};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "flint-agent-control",
    about = "Control Flint's agent threads panel from an agent's own CLI process"
)]
struct Cli {
    /// Unix socket to connect to. Defaults to the same path Flint's control
    /// server computes and binds -- only useful to override for testing.
    #[cfg(unix)]
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// Windows named pipe to connect to. Defaults to the session-scoped name
    /// Flint's control server computes; only useful to override for testing.
    #[cfg(windows)]
    #[arg(long, global = true)]
    pipe: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Re-tie this thread to a different (existing) worktree.
    RetieThread {
        #[arg(long)]
        worktree: PathBuf,
        /// Print the raw JSON response instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Launch a new sibling thread, in this worktree or a brand-new one.
    CreateThread {
        #[arg(long)]
        worktree: WorktreeArg,
        /// Branch/directory name hint for `--worktree new`; ignored for
        /// `--worktree current`.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        prompt: String,
        /// Print the raw JSON response instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum WorktreeArg {
    Current,
    New,
}

impl From<WorktreeArg> for CreateThreadWorktree {
    fn from(value: WorktreeArg) -> Self {
        match value {
            WorktreeArg::Current => CreateThreadWorktree::Current,
            WorktreeArg::New => CreateThreadWorktree::New,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    #[cfg(unix)]
    let socket_override = cli.socket;
    #[cfg(windows)]
    let pipe_override = cli.pipe;
    let (request, wants_json) = match cli.command {
        Command::RetieThread { worktree, json } => (
            ControlRequest::RetieThread(RetieThreadRequest { worktree }),
            json,
        ),
        Command::CreateThread {
            worktree,
            name,
            agent,
            prompt,
            json,
        } => (
            ControlRequest::CreateThread(CreateThreadRequest {
                worktree: worktree.into(),
                name,
                agent,
                prompt,
            }),
            json,
        ),
    };

    #[cfg(unix)]
    let result = run(request, socket_override);
    #[cfg(windows)]
    let result = run(request, pipe_override);
    #[cfg(not(any(unix, windows)))]
    let result = run(request);

    match result {
        Ok(response) => std::process::exit(print_response(&response, wants_json)),
        Err(error) => {
            eprintln!("flint-agent-control: {error:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(unix)]
fn run(
    request: ControlRequest,
    socket_override: Option<PathBuf>,
) -> anyhow::Result<ControlResponse> {
    unix::run(request, socket_override)
}

#[cfg(windows)]
fn run(request: ControlRequest, pipe_override: Option<String>) -> anyhow::Result<ControlResponse> {
    windows_client::run(request, pipe_override)
}

#[cfg(not(any(unix, windows)))]
fn run(_request: ControlRequest) -> anyhow::Result<ControlResponse> {
    anyhow::bail!("flint-agent-control is not supported on this platform")
}

#[cfg(windows)]
mod windows_client {
    use std::time::Duration;

    use agent_control_protocol::{
        ControlRequest, ControlResponse, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    };
    use anyhow::{Context as _, bail};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_IO_PENDING, ERROR_MORE_DATA, GENERIC_READ, HANDLE, WAIT_TIMEOUT,
        WIN32_ERROR,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, OPEN_EXISTING,
        ReadFile, WriteFile,
    };
    use windows::Win32::System::IO::{
        CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
    };
    use windows::Win32::System::Pipes::{
        PIPE_READMODE_MESSAGE, SetNamedPipeHandleState, WaitNamedPipeW,
    };
    use windows::Win32::System::Threading::CreateEventW;
    use windows::core::{Error as WindowsError, HSTRING, PCWSTR};

    const RETRY_BACKOFFS: &[Duration] = &[
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_millis(1000),
    ];
    const PIPE_AVAILABLE_TIMEOUT: Duration = Duration::from_secs(3);
    const PIPE_IO_TIMEOUT: Duration = Duration::from_secs(10);
    const READ_CHUNK_BYTES: usize = 16 * 1024;

    struct PipeHandle(HANDLE);

    impl Drop for PipeHandle {
        fn drop(&mut self) {
            // SAFETY: this type exclusively owns the valid handle returned by CreateFileW.
            if let Err(error) = unsafe { CloseHandle(self.0) } {
                eprintln!("flint-agent-control: failed to close named pipe: {error}");
            }
        }
    }

    pub(crate) fn run(
        request: ControlRequest,
        pipe_override: Option<String>,
    ) -> anyhow::Result<ControlResponse> {
        let pipe_name = match pipe_override {
            Some(pipe_name) => pipe_name,
            None => agent_control_protocol::pipe_name()
                .context("failed to derive Flint's agent control pipe name")?,
        };

        let mut attempt = 0;
        loop {
            let response = send_once(&pipe_name, &request)?;
            if !matches!(response, ControlResponse::NotReady) || attempt >= RETRY_BACKOFFS.len() {
                return Ok(response);
            }
            std::thread::sleep(RETRY_BACKOFFS[attempt]);
            attempt += 1;
        }
    }

    pub(super) fn send_once(
        pipe_name: &str,
        request: &ControlRequest,
    ) -> anyhow::Result<ControlResponse> {
        let wide_name = HSTRING::from(pipe_name);
        // SAFETY: the HSTRING supplies a valid, terminated pipe name for this call.
        if !unsafe {
            WaitNamedPipeW(
                PCWSTR(wide_name.as_ptr()),
                duration_millis(PIPE_AVAILABLE_TIMEOUT),
            )
        }
        .as_bool()
        {
            bail!(
                "Flint's agent control pipe at {pipe_name} was unavailable or stayed busy for {} ms: {}",
                PIPE_AVAILABLE_TIMEOUT.as_millis(),
                WindowsError::from_win32()
            );
        }

        // SAFETY: all pointers are either valid values or None; the returned handle is owned below.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide_name.as_ptr()),
                GENERIC_READ.0 | FILE_WRITE_DATA.0 | FILE_WRITE_ATTRIBUTES.0,
                Default::default(),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .with_context(|| {
            format!("failed to connect to Flint's agent control pipe at {pipe_name}")
        })?;
        let handle = PipeHandle(handle);
        let mode = PIPE_READMODE_MESSAGE;
        // SAFETY: handle is a connected named-pipe handle and mode remains valid for the call.
        unsafe { SetNamedPipeHandleState(handle.0, Some(&mode), None, None) }
            .context("failed to select named-pipe message mode")?;

        let payload = serde_json::to_vec(request).context("failed to encode request")?;
        if payload.len() > MAX_REQUEST_BYTES {
            bail!("request exceeds the {MAX_REQUEST_BYTES}-byte protocol limit");
        }
        overlapped_write(handle.0, &payload, PIPE_IO_TIMEOUT).context("failed to send request")?;
        let response = overlapped_read_message(handle.0, MAX_RESPONSE_BYTES, PIPE_IO_TIMEOUT)
            .context("failed to read response")?;
        serde_json::from_slice(&response).context("failed to decode response")
    }

    fn overlapped_write(handle: HANDLE, bytes: &[u8], timeout: Duration) -> anyhow::Result<()> {
        let mut transferred = 0;
        let event = create_overlapped_event()?;
        let mut overlapped = OVERLAPPED::default();
        overlapped.hEvent = event.0;
        // SAFETY: bytes and overlapped remain alive until completion is observed below.
        let result = unsafe { WriteFile(handle, Some(bytes), None, Some(&mut overlapped)) };
        finish_overlapped(handle, &mut overlapped, result, timeout, &mut transferred)?;
        if transferred as usize != bytes.len() {
            bail!(
                "named-pipe write completed after {transferred} of {} bytes",
                bytes.len()
            );
        }
        Ok(())
    }

    fn overlapped_read_message(
        handle: HANDLE,
        maximum_bytes: usize,
        timeout: Duration,
    ) -> anyhow::Result<Vec<u8>> {
        let mut message = Vec::new();
        loop {
            let remaining = maximum_bytes.saturating_sub(message.len());
            if remaining == 0 {
                bail!("response exceeds the {maximum_bytes}-byte protocol limit");
            }
            let mut chunk = vec![0; remaining.min(READ_CHUNK_BYTES)];
            let mut transferred = 0;
            let event = create_overlapped_event()?;
            let mut overlapped = OVERLAPPED::default();
            overlapped.hEvent = event.0;
            // SAFETY: chunk and overlapped remain alive until completion is observed below.
            let result = unsafe { ReadFile(handle, Some(&mut chunk), None, Some(&mut overlapped)) };
            match finish_overlapped(handle, &mut overlapped, result, timeout, &mut transferred) {
                Ok(()) => {
                    chunk.truncate(transferred as usize);
                    message.extend_from_slice(&chunk);
                    return Ok(message);
                }
                Err(error)
                    if windows_error_code(&error)
                        == Some(WIN32_ERROR(ERROR_MORE_DATA.0).into()) =>
                {
                    chunk.truncate(transferred as usize);
                    message.extend_from_slice(&chunk);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn finish_overlapped(
        handle: HANDLE,
        overlapped: &mut OVERLAPPED,
        initial_result: windows::core::Result<()>,
        timeout: Duration,
        transferred: &mut u32,
    ) -> anyhow::Result<()> {
        if initial_result.is_ok() {
            // SAFETY: the operation completed and all pointers remain valid.
            return unsafe { GetOverlappedResult(handle, overlapped, transferred, false) }
                .map_err(anyhow::Error::from);
        }

        if let Err(error) = &initial_result
            && error.code() != WIN32_ERROR(ERROR_IO_PENDING.0).into()
            && error.code() != WIN32_ERROR(ERROR_MORE_DATA.0).into()
        {
            return Err(error.clone().into());
        }

        // GetOverlappedResultEx reports immediate errors as well as pending completion.
        // SAFETY: the OVERLAPPED and its buffers remain alive through completion/cancellation.
        let completion = unsafe {
            GetOverlappedResultEx(
                handle,
                overlapped,
                transferred,
                duration_millis(timeout),
                false,
            )
        };
        if let Err(error) = completion {
            if error.code() == WIN32_ERROR(WAIT_TIMEOUT.0).into() {
                // SAFETY: this cancels exactly the operation represented by overlapped.
                if let Err(cancel_error) = unsafe { CancelIoEx(handle, Some(overlapped)) } {
                    // ERROR_NOT_FOUND is a normal completion race. In every
                    // case we still observe terminal completion below before
                    // releasing the OVERLAPPED or its buffer.
                    eprintln!(
                        "flint-agent-control: named-pipe cancellation reported {cancel_error}"
                    );
                }
                // SAFETY: waiting here ensures buffers can be released only after terminal completion.
                let terminal =
                    unsafe { GetOverlappedResult(handle, overlapped, transferred, true) };
                if let Err(terminal_error) = terminal {
                    eprintln!(
                        "flint-agent-control: cancelled named-pipe operation completed with {terminal_error}"
                    );
                }
                bail!("named-pipe I/O timed out after {} ms", timeout.as_millis());
            }
            return Err(error.into());
        }
        Ok(())
    }

    fn duration_millis(duration: Duration) -> u32 {
        duration.as_millis().min(u32::MAX as u128) as u32
    }

    fn create_overlapped_event() -> anyhow::Result<PipeHandle> {
        // SAFETY: default security, manual reset, initially nonsignaled, unnamed event.
        let event = unsafe { CreateEventW(None, true, false, None) }
            .context("failed to create named-pipe overlapped event")?;
        Ok(PipeHandle(event))
    }

    fn windows_error_code(error: &anyhow::Error) -> Option<windows::core::HRESULT> {
        error.downcast_ref::<WindowsError>().map(WindowsError::code)
    }
}

fn print_response(response: &ControlResponse, wants_json: bool) -> i32 {
    if wants_json {
        match serde_json::to_string(response) {
            Ok(json) => println!("{json}"),
            Err(error) => eprintln!("flint-agent-control: failed to encode response: {error}"),
        }
        return exit_code_for(response);
    }
    match response {
        ControlResponse::Ok(ControlSuccess::Retied { worktree }) => {
            println!("Retied to {}", worktree.display());
        }
        ControlResponse::Ok(ControlSuccess::ThreadCreated { worktree }) => {
            println!("Created thread in {}", worktree.display());
        }
        ControlResponse::NotReady => {
            eprintln!(
                "flint-agent-control: this terminal doesn't appear to be a Flint agent thread -- \
                 if you're not using Flint here, this is expected and can be ignored."
            );
        }
        ControlResponse::Error { message } => {
            eprintln!("flint-agent-control: {message}");
        }
    }
    exit_code_for(response)
}

fn exit_code_for(response: &ControlResponse) -> i32 {
    match response {
        ControlResponse::Ok(_) => 0,
        ControlResponse::NotReady | ControlResponse::Error { .. } => 1,
    }
}

#[cfg(unix)]
mod unix {
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::time::Duration;

    use agent_control_protocol::{ControlRequest, ControlResponse};
    use anyhow::Context as _;

    /// Bounded backoff for a `NotReady` response, which means Flint hasn't
    /// (yet, or ever will) matched the connecting process's PID ancestry to
    /// a registered thread. Keeping the wait client-side avoids parking
    /// requests inside the server.
    const RETRY_BACKOFFS: &[Duration] = &[
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_millis(1000),
    ];

    pub(crate) fn run(
        request: ControlRequest,
        socket_override: Option<PathBuf>,
    ) -> anyhow::Result<ControlResponse> {
        let socket_path = socket_override.unwrap_or_else(agent_control_protocol::socket_path);

        let mut attempt = 0;
        loop {
            let response = send_once(&socket_path, &request)?;
            if !matches!(response, ControlResponse::NotReady) || attempt >= RETRY_BACKOFFS.len() {
                return Ok(response);
            }
            std::thread::sleep(RETRY_BACKOFFS[attempt]);
            attempt += 1;
        }
    }

    fn send_once(
        socket_path: &std::path::Path,
        request: &ControlRequest,
    ) -> anyhow::Result<ControlResponse> {
        let mut stream = UnixStream::connect(socket_path).with_context(|| {
            format!(
                "failed to connect to Flint's agent control socket at {}",
                socket_path.display()
            )
        })?;
        let payload = serde_json::to_vec(request).context("failed to encode request")?;
        stream
            .write_all(&payload)
            .context("failed to send request")?;
        stream
            .shutdown(Shutdown::Write)
            .context("failed to finish sending request")?;

        let mut response_bytes = Vec::new();
        stream
            .read_to_end(&mut response_bytes)
            .context("failed to read response")?;
        serde_json::from_slice(&response_bytes).context("failed to decode response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_pipe_override_is_parsed_as_a_pipe_name() {
        let cli = Cli::try_parse_from([
            "flint-agent-control",
            "--pipe",
            r"\\.\pipe\flint-test",
            "retie-thread",
            "--worktree",
            r"C:\repo",
        ])
        .expect("parse Windows pipe override");
        assert_eq!(cli.pipe.as_deref(), Some(r"\\.\pipe\flint-test"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_client_reads_a_response_larger_than_one_chunk() {
        use windows::Win32::Foundation::{
            CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, WIN32_ERROR,
        };
        use windows::Win32::Storage::FileSystem::{
            FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
        };
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
            PIPE_TYPE_MESSAGE, PIPE_WAIT,
        };
        use windows::core::{HSTRING, PCWSTR};

        let pipe_name = format!(
            r"\\.\pipe\flint-agent-control-cli-test-{}",
            std::process::id()
        );
        let wide_name = HSTRING::from(&pipe_name);
        // SAFETY: the name remains valid for the call and default security is acceptable in a test.
        let server = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide_name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                1,
                64 * 1024,
                64 * 1024,
                0,
                None,
            )
        };
        assert_ne!(server, INVALID_HANDLE_VALUE, "create test pipe");
        let server_value = server.0 as usize;
        let server_thread = std::thread::spawn(move || {
            let server = HANDLE(server_value as *mut std::ffi::c_void);
            // SAFETY: server is a valid synchronous pipe handle owned by this thread.
            if let Err(error) = unsafe { ConnectNamedPipe(server, None) }
                && error.code() != WIN32_ERROR(ERROR_PIPE_CONNECTED.0).into()
            {
                panic!("accept CLI connection: {error}");
            }
            let mut request = vec![0; 64 * 1024];
            let mut request_bytes = 0;
            // SAFETY: request and count storage remain valid for the call.
            unsafe { ReadFile(server, Some(&mut request), Some(&mut request_bytes), None) }
                .expect("read CLI request");
            request.truncate(request_bytes as usize);
            serde_json::from_slice::<ControlRequest>(&request).expect("decode CLI request");

            let response = ControlResponse::Error {
                message: "x".repeat(32 * 1024),
            };
            let response = serde_json::to_vec(&response).expect("encode response");
            let mut response_bytes = 0;
            // SAFETY: response and count storage remain valid for the call.
            unsafe { WriteFile(server, Some(&response), Some(&mut response_bytes), None) }
                .expect("write CLI response");
            assert_eq!(response_bytes as usize, response.len());
            // SAFETY: server is connected; this returns once the client consumes the response.
            unsafe { FlushFileBuffers(server) }.expect("flush CLI response");
            // SAFETY: this thread owns the connected server handle.
            unsafe { DisconnectNamedPipe(server) }.expect("disconnect test pipe");
            // SAFETY: this thread owns the server handle and closes it once.
            unsafe { CloseHandle(server) }.expect("close test pipe");
        });

        let request = ControlRequest::RetieThread(RetieThreadRequest {
            worktree: PathBuf::from(r"C:\repo"),
        });
        let response = windows_client::send_once(&pipe_name, &request).expect("round trip request");
        match response {
            ControlResponse::Error { message } => assert_eq!(message.len(), 32 * 1024),
            other => panic!("unexpected response: {other:?}"),
        }
        server_thread.join().expect("server thread panicked");
    }

    #[test]
    fn unsuccessful_protocol_responses_have_nonzero_exit_codes() {
        assert_eq!(exit_code_for(&ControlResponse::NotReady), 1);
        assert_eq!(
            exit_code_for(&ControlResponse::Error {
                message: "failed".into()
            }),
            1
        );
    }
}
