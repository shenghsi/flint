//! Shared `flintctl` parser and output implementation. The local binary uses
//! Flint's local control endpoint. `flint-remote-server` supplies its remote
//! endpoint transport to the same implementation. Caller identity comes from
//! the operating system, not CLI data.
//!
//! Sends bare, unauthenticated-looking requests on purpose: the server
//! establishes caller identity itself from the kernel-reported PID of
//! the process that connected to its endpoint, not from
//! anything this binary presents. There is nothing here to mint, deliver,
//! or leak. The endpoint is computed identically by the server and client;
//! the platform-specific override is only for testing.

use std::path::PathBuf;

use agent_control_protocol::{
    ControlCommand, ControlRequest, ControlResponse, ControlResult, ControlSuccess,
    CreateThreadRequest, CreateThreadWorktree, RetieThreadRequest, TerminalControlId,
    TerminalListRequest, TerminalOutputMatcher, TerminalReadRequest, TerminalReadSource,
    TerminalRunRequest, TerminalSendKeyRequest, TerminalSendTextRequest, TerminalWaitOutputRequest,
};
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "flintctl", about = "Control a running Flint application")]
struct Cli {
    /// Unix socket to connect to. Defaults to the same path Flint's control
    /// server computes and binds -- only useful to override for testing.
    #[cfg(unix)]
    #[arg(long, global = true, hide = true)]
    socket: Option<PathBuf>,
    /// Windows named pipe to connect to. Defaults to the session-scoped name
    /// Flint's control server computes; only useful to override for testing.
    #[cfg(windows)]
    #[arg(long, global = true, hide = true)]
    pipe: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Status {
        #[arg(long)]
        json: bool,
    },
    Thread {
        #[command(subcommand)]
        command: ThreadCommand,
    },
    Terminal {
        #[command(subcommand)]
        command: TerminalCommand,
    },
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
}

#[derive(Subcommand)]
enum SkillCommand {
    Print,
    Status {
        #[arg(long, value_enum)]
        agent: SkillAgentArg,
    },
    Install {
        #[arg(long, value_enum)]
        agent: SkillAgentArg,
        #[arg(long)]
        replace: bool,
    },
    Update {
        #[arg(long, value_enum)]
        agent: SkillAgentArg,
    },
    Uninstall {
        #[arg(long, value_enum)]
        agent: SkillAgentArg,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ThreadCommand {
    Retie {
        #[arg(long)]
        worktree: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Create {
        #[arg(long)]
        worktree: WorktreeArg,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TerminalCommand {
    Current {
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    Read {
        terminal_id: String,
        #[arg(long, value_enum, default_value_t = ReadSourceArg::Recent)]
        source: ReadSourceArg,
        #[arg(long, default_value_t = agent_control_protocol::DEFAULT_READ_LINES)]
        lines: usize,
        /// Read only output appended after this cursor -- copy it verbatim
        /// from a prior read's `cursor` field (JSON output) or its printed
        /// "cursor" line (human output). Only valid with the default recent
        /// source.
        #[arg(long, value_parser = parse_read_cursor)]
        since: Option<agent_control_protocol::TerminalReadCursor>,
        #[arg(long)]
        json: bool,
    },
    SendText {
        terminal_id: String,
        text: String,
        #[arg(long)]
        json: bool,
    },
    SendKey {
        terminal_id: String,
        /// Key names such as enter, escape, ctrl-c, alt-left, arrows, and F1-F12.
        #[arg(required = true)]
        keys: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    Run {
        terminal_id: String,
        command: String,
        #[arg(long)]
        json: bool,
    },
    #[command(group(
        ArgGroup::new("matcher")
            .required(true)
            .multiple(false)
            .args(["match_text", "regex"])
    ))]
    WaitOutput {
        terminal_id: String,
        #[arg(long = "match")]
        match_text: Option<String>,
        #[arg(long)]
        regex: Option<String>,
        #[arg(long, value_enum, default_value_t = ReadSourceArg::Recent)]
        source: ReadSourceArg,
        #[arg(long, default_value_t = agent_control_protocol::DEFAULT_READ_LINES)]
        lines: usize,
        #[arg(long, default_value = "30s", value_parser = parse_duration_millis)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum WorktreeArg {
    Current,
    New,
}

#[derive(Clone, Copy, ValueEnum)]
enum SkillAgentArg {
    Codex,
    Claude,
}

impl From<SkillAgentArg> for agent_control_skill::AgentKind {
    fn from(value: SkillAgentArg) -> Self {
        match value {
            SkillAgentArg::Codex => Self::Codex,
            SkillAgentArg::Claude => Self::Claude,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ReadSourceArg {
    Visible,
    Recent,
    RecentUnwrapped,
    Detection,
}

impl From<ReadSourceArg> for TerminalReadSource {
    fn from(value: ReadSourceArg) -> Self {
        match value {
            ReadSourceArg::Visible => TerminalReadSource::Visible,
            ReadSourceArg::Recent => TerminalReadSource::Recent,
            ReadSourceArg::RecentUnwrapped => TerminalReadSource::RecentUnwrapped,
            ReadSourceArg::Detection => TerminalReadSource::Detection,
        }
    }
}

impl From<WorktreeArg> for CreateThreadWorktree {
    fn from(value: WorktreeArg) -> Self {
        match value {
            WorktreeArg::Current => CreateThreadWorktree::Current,
            WorktreeArg::New => CreateThreadWorktree::New,
        }
    }
}

pub fn main() {
    let cli = Cli::parse();
    if let Some(result) = cli.run_local_command() {
        match result {
            Ok(()) => return,
            Err(error) => {
                eprintln!("flintctl: {error:#}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(unix)]
    let socket_override = cli.socket.clone();
    #[cfg(windows)]
    let pipe_override = cli.pipe.clone();
    let (request, wants_json) = match cli.into_request() {
        Ok(request) => request,
        Err(error) => {
            eprintln!("flintctl: {error:#}");
            std::process::exit(2);
        }
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
            eprintln!("flintctl: {error:#}");
            std::process::exit(1);
        }
    }
}

pub fn main_with_transport(
    transport: impl FnOnce(ControlRequest) -> anyhow::Result<ControlResponse>,
) {
    let cli = Cli::parse();
    if let Some(result) = cli.run_local_command() {
        match result {
            Ok(()) => return,
            Err(error) => {
                eprintln!("flintctl: {error:#}");
                std::process::exit(1);
            }
        }
    }
    let (request, wants_json) = match cli.into_request() {
        Ok(request) => request,
        Err(error) => {
            eprintln!("flintctl: {error:#}");
            std::process::exit(2);
        }
    };

    match transport(request) {
        Ok(response) => std::process::exit(print_response(&response, wants_json)),
        Err(error) => {
            eprintln!("flintctl: {error:#}");
            std::process::exit(1);
        }
    }
}

impl Cli {
    fn run_local_command(&self) -> Option<anyhow::Result<()>> {
        let Command::Skill { command } = &self.command else {
            return None;
        };
        Some(run_skill_command(command))
    }

    fn into_request(self) -> anyhow::Result<(ControlRequest, bool)> {
        let (command, wants_json) = match self.command {
            Command::Status { json } => (ControlCommand::Status, json),
            Command::Thread { command } => match command {
                ThreadCommand::Retie { worktree, json } => (
                    ControlCommand::ThreadRetie(RetieThreadRequest { worktree }),
                    json,
                ),
                ThreadCommand::Create {
                    worktree,
                    name,
                    agent,
                    prompt,
                    json,
                } => (
                    ControlCommand::ThreadCreate(CreateThreadRequest {
                        worktree: worktree.into(),
                        name,
                        agent,
                        prompt,
                    }),
                    json,
                ),
            },
            Command::Terminal { command } => match command {
                TerminalCommand::Current { json } => (ControlCommand::TerminalCurrent, json),
                TerminalCommand::List { all, json } => (
                    ControlCommand::TerminalList(TerminalListRequest { all }),
                    json,
                ),
                TerminalCommand::Read {
                    terminal_id,
                    source,
                    lines,
                    since,
                    json,
                } => (
                    ControlCommand::TerminalRead(TerminalReadRequest {
                        terminal_id: TerminalControlId(terminal_id),
                        source: source.into(),
                        lines,
                        since,
                    }),
                    json,
                ),
                TerminalCommand::SendText {
                    terminal_id,
                    text,
                    json,
                } => (
                    ControlCommand::TerminalSendText(TerminalSendTextRequest {
                        terminal_id: TerminalControlId(terminal_id),
                        text,
                    }),
                    json,
                ),
                TerminalCommand::SendKey {
                    terminal_id,
                    keys,
                    json,
                } => (
                    ControlCommand::TerminalSendKey(TerminalSendKeyRequest {
                        terminal_id: TerminalControlId(terminal_id),
                        keys,
                    }),
                    json,
                ),
                TerminalCommand::Run {
                    terminal_id,
                    command,
                    json,
                } => (
                    ControlCommand::TerminalRun(TerminalRunRequest {
                        terminal_id: TerminalControlId(terminal_id),
                        command,
                    }),
                    json,
                ),
                TerminalCommand::WaitOutput {
                    terminal_id,
                    match_text,
                    regex,
                    source,
                    lines,
                    timeout,
                    json,
                } => {
                    let matcher = match (match_text, regex) {
                        (Some(text), None) => TerminalOutputMatcher::Literal(text),
                        (None, Some(pattern)) => TerminalOutputMatcher::Regex(pattern),
                        _ => anyhow::bail!("select exactly one of --match or --regex"),
                    };
                    (
                        ControlCommand::TerminalWaitOutput(TerminalWaitOutputRequest {
                            terminal_id: TerminalControlId(terminal_id),
                            source: source.into(),
                            lines,
                            matcher,
                            timeout_millis: timeout,
                        }),
                        json,
                    )
                }
            },
            Command::Skill { .. } => anyhow::bail!("skill commands do not use the control server"),
        };
        Ok((ControlRequest::current(command), wants_json))
    }
}

fn run_skill_command(command: &SkillCommand) -> anyhow::Result<()> {
    use agent_control_skill::{SkillEnvironment, SkillState};

    let environment = SkillEnvironment::current();
    match command {
        SkillCommand::Print => print!("{}", agent_control_skill::BUNDLED_SKILL),
        SkillCommand::Status { agent } => {
            let agent = (*agent).into();
            let state = agent_control_skill::status(agent, &environment)?;
            let state = match state {
                SkillState::NotInstalled => "not-installed",
                SkillState::Unowned => "unowned",
                SkillState::InstalledCurrent => "installed-current",
                SkillState::InstalledOutdated => "installed-outdated",
                SkillState::Modified => "modified",
                SkillState::Missing => "missing",
            };
            println!("{}: {state}", agent.label());
        }
        SkillCommand::Install { agent, replace } => {
            let agent = (*agent).into();
            agent_control_skill::install(agent, &environment, *replace)?;
            println!("Installed the Flint control skill for {}", agent.label());
        }
        SkillCommand::Update { agent } => {
            let agent = (*agent).into();
            match agent_control_skill::status(agent, &environment)? {
                SkillState::InstalledOutdated => {
                    agent_control_skill::install(agent, &environment, true)?;
                    println!("Updated the Flint control skill for {}", agent.label());
                }
                SkillState::InstalledCurrent => {
                    println!("The Flint control skill for {} is current", agent.label());
                }
                SkillState::Modified => anyhow::bail!(
                    "the installed {} skill was modified; use skill install --replace only after review",
                    agent.label()
                ),
                SkillState::NotInstalled | SkillState::Unowned | SkillState::Missing => {
                    anyhow::bail!(
                        "the Flint control skill for {} is not installed",
                        agent.label()
                    )
                }
            }
        }
        SkillCommand::Uninstall { agent, force } => {
            let agent = (*agent).into();
            agent_control_skill::uninstall(agent, &environment, *force)?;
            println!("Uninstalled the Flint control skill for {}", agent.label());
        }
    }
    Ok(())
}

fn parse_duration_millis(value: &str) -> Result<u64, String> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else {
        return Err("duration must end in ms, s, or m".to_string());
    };
    let number = number
        .parse::<u64>()
        .map_err(|_| "duration must start with a positive integer".to_string())?;
    number
        .checked_mul(multiplier)
        .filter(|duration| *duration > 0)
        .ok_or_else(|| "duration is out of range".to_string())
}

/// Cursors carry a raw snippet of terminal output (see
/// `agent_control_protocol::TerminalReadCursor`), which can contain
/// anything a shell argument can't safely hold -- quotes, `$`, newlines,
/// control bytes. Base64 keeps the CLI's textual `--since`/printed-cursor
/// form a single shell-safe token; `--json` output carries the cursor's
/// `anchor` field as a plain JSON string instead, with no encoding needed.
fn parse_read_cursor(value: &str) -> Result<agent_control_protocol::TerminalReadCursor, String> {
    use base64::Engine as _;
    let invalid = || "cursor must be the exact value printed by a prior read".to_string();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| invalid())?;
    let anchor = String::from_utf8(bytes).map_err(|_| invalid())?;
    Ok(agent_control_protocol::TerminalReadCursor { anchor })
}

fn encode_read_cursor(cursor: &agent_control_protocol::TerminalReadCursor) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(cursor.anchor.as_bytes())
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
    anyhow::bail!("flintctl is not supported on this platform")
}

#[cfg(windows)]
mod windows_client {
    use std::time::Duration;

    use agent_control_protocol::{
        ControlRequest, ControlResponse, ControlResult, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
        decode_frame, frame_payload,
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
                eprintln!("flintctl: failed to close named pipe: {error}");
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
            if !matches!(&response.result, ControlResult::NotReady) {
                return Ok(response);
            }
            if attempt >= RETRY_BACKOFFS.len() {
                return Ok(ControlResponse::error(
                    agent_control_protocol::ControlErrorCode::CallerNotRecognized,
                    "caller was not recognized before the retry deadline",
                ));
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
        let frame =
            frame_payload(&payload, MAX_REQUEST_BYTES).context("failed to frame request")?;
        overlapped_write(handle.0, &frame, PIPE_IO_TIMEOUT).context("failed to send request")?;
        let response_frame =
            overlapped_read_message(handle.0, MAX_RESPONSE_BYTES + 4, PIPE_IO_TIMEOUT)
                .context("failed to read response")?;
        let response = decode_frame(&response_frame, MAX_RESPONSE_BYTES)
            .context("failed to decode response frame")?;
        serde_json::from_slice(response).context("failed to decode response")
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
                    eprintln!("flintctl: named-pipe cancellation reported {cancel_error}");
                }
                // SAFETY: waiting here ensures buffers can be released only after terminal completion.
                let terminal =
                    unsafe { GetOverlappedResult(handle, overlapped, transferred, true) };
                if let Err(terminal_error) = terminal {
                    eprintln!(
                        "flintctl: cancelled named-pipe operation completed with {terminal_error}"
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
            Err(error) => eprintln!("flintctl: failed to encode response: {error}"),
        }
        return exit_code_for(response);
    }
    match &response.result {
        ControlResult::Ok(ControlSuccess::Retied { worktree }) => {
            println!("Retied to {}", worktree.display());
        }
        ControlResult::Ok(ControlSuccess::ThreadCreated { worktree }) => {
            println!("Created thread in {}", worktree.display());
        }
        ControlResult::Ok(ControlSuccess::Status(status)) => {
            println!(
                "Flint {} ({}, protocol {}.{})",
                status.flint_version,
                status.release_channel,
                status.protocol_version.major,
                status.protocol_version.minor
            );
        }
        ControlResult::Ok(ControlSuccess::TerminalCurrent(terminal)) => {
            print_terminal(terminal);
        }
        ControlResult::Ok(ControlSuccess::TerminalList(terminals)) => {
            for terminal in terminals {
                print_terminal(terminal);
            }
        }
        ControlResult::Ok(ControlSuccess::TerminalRead(snapshot))
        | ControlResult::Ok(ControlSuccess::TerminalWaitOutput(snapshot)) => {
            print!("{}", snapshot.text);
            eprintln!(
                "flintctl: cursor {} (pass as --since to read only what's new)",
                encode_read_cursor(&snapshot.cursor)
            );
        }
        ControlResult::Ok(ControlSuccess::TerminalInputAccepted) => {}
        ControlResult::NotReady => {
            eprintln!(
                "flintctl: this process does not appear to be in a controllable Flint terminal"
            );
        }
        ControlResult::Error(error) => {
            eprintln!(
                "flintctl: {}: {}",
                error_code_name(error.code),
                error.message
            );
        }
    }
    exit_code_for(response)
}

fn exit_code_for(response: &ControlResponse) -> i32 {
    match &response.result {
        ControlResult::Ok(_) => 0,
        ControlResult::NotReady | ControlResult::Error(_) => 1,
    }
}

fn print_terminal(terminal: &agent_control_protocol::TerminalMetadata) {
    let working_directory = terminal
        .working_directory
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "-".to_string());
    println!(
        "{}\t{}\t{}",
        terminal.id.0, terminal.title, working_directory
    );
}

fn error_code_name(code: agent_control_protocol::ControlErrorCode) -> &'static str {
    use agent_control_protocol::ControlErrorCode;
    match code {
        ControlErrorCode::CallerNotRecognized => "caller-not-recognized",
        ControlErrorCode::CallerNotAgentThread => "caller-not-agent-thread",
        ControlErrorCode::TerminalNotFound => "terminal-not-found",
        ControlErrorCode::TerminalOutsideWorkspace => "terminal-outside-workspace",
        ControlErrorCode::TerminalExited => "terminal-exited",
        ControlErrorCode::InvalidKey => "invalid-key",
        ControlErrorCode::InvalidPattern => "invalid-pattern",
        ControlErrorCode::InvalidRequest => "invalid-request",
        ControlErrorCode::CursorExpired => "cursor-expired",
        ControlErrorCode::Timeout => "timeout",
        ControlErrorCode::ResponseTooLarge => "response-too-large",
        ControlErrorCode::UnsupportedProtocol => "unsupported-protocol",
        ControlErrorCode::RemoteControlUnavailable => "remote-control-unavailable",
        ControlErrorCode::RemoteSessionStale => "remote-session-stale",
        ControlErrorCode::RemoteVersionMismatch => "remote-version-mismatch",
        ControlErrorCode::Internal => "internal",
    }
}

#[cfg(unix)]
mod unix {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::time::Duration;

    use agent_control_protocol::{
        ControlRequest, ControlResponse, ControlResult, FRAME_LENGTH_BYTES, MAX_REQUEST_BYTES,
        MAX_RESPONSE_BYTES, frame_payload,
    };
    use anyhow::{Context as _, bail};

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
            if !matches!(&response.result, ControlResult::NotReady) {
                return Ok(response);
            }
            if attempt >= RETRY_BACKOFFS.len() {
                return Ok(ControlResponse::error(
                    agent_control_protocol::ControlErrorCode::CallerNotRecognized,
                    "caller was not recognized before the retry deadline",
                ));
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
        let frame =
            frame_payload(&payload, MAX_REQUEST_BYTES).context("failed to frame request")?;
        stream.write_all(&frame).context("failed to send request")?;
        let mut length_bytes = [0; FRAME_LENGTH_BYTES];
        stream
            .read_exact(&mut length_bytes)
            .context("failed to read response length")?;
        let response_length = u32::from_be_bytes(length_bytes) as usize;
        if response_length > MAX_RESPONSE_BYTES {
            bail!("response exceeds the {MAX_RESPONSE_BYTES}-byte protocol limit");
        }
        let mut response_bytes = vec![0; response_length];
        stream
            .read_exact(&mut response_bytes)
            .context("failed to read response payload")?;
        serde_json::from_slice(&response_bytes).context("failed to decode response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_commands_parse_without_a_control_endpoint() {
        assert!(Cli::try_parse_from(["flintctl", "skill", "print"]).is_ok());
        assert!(Cli::try_parse_from(["flintctl", "skill", "status", "--agent", "codex"]).is_ok());
        assert!(Cli::try_parse_from(["flintctl", "skill", "install", "--agent", "claude"]).is_ok());
        assert!(Cli::try_parse_from(["flintctl", "skill", "update", "--agent", "codex"]).is_ok());
        assert!(
            Cli::try_parse_from(["flintctl", "skill", "uninstall", "--agent", "claude"]).is_ok()
        );
    }

    #[test]
    fn noun_first_thread_command_builds_current_request() {
        let cli = Cli::try_parse_from([
            "flintctl",
            "thread",
            "retie",
            "--worktree",
            "/repo/worktree",
        ])
        .expect("parse thread retie");

        let (request, wants_json) = cli.into_request().expect("build request");

        assert!(!wants_json);
        assert!(matches!(
            request.command,
            ControlCommand::ThreadRetie(RetieThreadRequest { .. })
        ));
    }

    #[test]
    fn old_flat_commands_are_rejected() {
        assert!(
            Cli::try_parse_from(["flintctl", "retie-thread", "--worktree", "/repo/worktree"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "flintctl",
                "create-thread",
                "--worktree",
                "current",
                "--agent",
                "codex",
                "--prompt",
                "test"
            ])
            .is_err()
        );
    }

    #[test]
    fn terminal_wait_requires_exactly_one_matcher() {
        assert!(
            Cli::try_parse_from([
                "flintctl",
                "terminal",
                "wait-output",
                "t1",
                "--match",
                "ready"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["flintctl", "terminal", "wait-output", "t1"]).is_err());
        assert!(
            Cli::try_parse_from([
                "flintctl",
                "terminal",
                "wait-output",
                "t1",
                "--match",
                "ready",
                "--regex",
                "ready.*"
            ])
            .is_err()
        );
    }

    #[test]
    fn read_cursor_round_trips_through_its_printed_encoding() {
        let cursor = agent_control_protocol::TerminalReadCursor {
            anchor: "line one\nline two".to_string(),
        };
        let encoded = encode_read_cursor(&cursor);
        // Base64 output is a single shell-safe token: no quotes, `$`, or
        // whitespace that would need escaping on a command line.
        assert!(
            encoded
                .chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || character == '+'
                    || character == '/'
                    || character == '=')
        );
        assert_eq!(parse_read_cursor(&encoded), Ok(cursor));
    }

    #[test]
    fn read_cursor_rejects_malformed_input() {
        assert!(parse_read_cursor("not valid base64!!").is_err());
    }

    #[test]
    fn terminal_read_since_flag_builds_a_request_with_the_decoded_cursor() {
        let cursor = agent_control_protocol::TerminalReadCursor {
            anchor: "previous tail".to_string(),
        };
        let encoded = encode_read_cursor(&cursor);
        let cli = Cli::try_parse_from(["flintctl", "terminal", "read", "t1", "--since", &encoded])
            .expect("parse terminal read with --since");

        let (request, _wants_json) = cli.into_request().expect("build request");
        match request.command {
            ControlCommand::TerminalRead(request) => assert_eq!(request.since, Some(cursor)),
            other => panic!("expected TerminalRead, got {other:?}"),
        }
    }

    #[test]
    fn terminal_read_without_since_defaults_to_none() {
        let cli = Cli::try_parse_from(["flintctl", "terminal", "read", "t1"])
            .expect("parse terminal read");

        let (request, _wants_json) = cli.into_request().expect("build request");
        match request.command {
            ControlCommand::TerminalRead(request) => assert_eq!(request.since, None),
            other => panic!("expected TerminalRead, got {other:?}"),
        }
    }

    #[test]
    fn terminal_read_source_detection_is_parsed() {
        let cli = Cli::try_parse_from([
            "flintctl",
            "terminal",
            "read",
            "t1",
            "--source",
            "detection",
        ])
        .expect("parse terminal read --source detection");

        let (request, _wants_json) = cli.into_request().expect("build request");
        match request.command {
            ControlCommand::TerminalRead(request) => {
                assert_eq!(request.source, TerminalReadSource::Detection)
            }
            other => panic!("expected TerminalRead, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipe_override_is_parsed_as_a_pipe_name() {
        let cli = Cli::try_parse_from([
            "flintctl",
            "--pipe",
            r"\\.\pipe\flint-test",
            "thread",
            "retie",
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
        assert_eq!(exit_code_for(&ControlResponse::not_ready()), 1);
        assert_eq!(
            exit_code_for(&ControlResponse::error(
                agent_control_protocol::ControlErrorCode::Internal,
                "failed",
            )),
            1
        );
    }
}
