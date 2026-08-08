//! `flint-agent-control`: the binary an agent thread's own CLI process (Codex,
//! Claude Code, etc.) invokes to ask Flint to re-tie itself to a different
//! worktree, or spawn a sibling thread. Talks to `agent_threads::control`'s
//! Unix socket server over `agent_control_protocol`'s wire types. Unix-only
//! for this pass -- see the `run` stub below for why the crate still builds
//! (but does nothing) on Windows, where it participates in CI as a workspace
//! member without ever being bundled.
//!
//! Discovers its socket and token from the marker file Flint writes into
//! the thread's own working directory (`agent_control_protocol::MARKER_FILE_PATH`)
//! rather than environment variables: some coding-agent CLIs run their own
//! shell commands through a subprocess exec that silently strips custom env
//! vars, which a plain file read isn't subject to. `--socket`/`--token`
//! remain as explicit overrides for testing or an agent that already knows
//! them another way.

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
    /// Unix socket to connect to. Defaults to the `socket` field of the
    /// marker file in the current directory.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// Per-thread token to authenticate with. Defaults to the `token` field
    /// of the marker file in the current directory.
    #[arg(long, global = true)]
    token: Option<String>,
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
    let socket_override = cli.socket;
    let token_override = cli.token;
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

    match run(request, socket_override, token_override) {
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
    token_override: Option<String>,
) -> anyhow::Result<ControlResponse> {
    unix::run(request, socket_override, token_override)
}

#[cfg(not(unix))]
fn run(
    _request: ControlRequest,
    _socket_override: Option<PathBuf>,
    _token_override: Option<String>,
) -> anyhow::Result<ControlResponse> {
    anyhow::bail!("flint-agent-control is not supported on this platform")
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
            eprintln!("flint-agent-control: thread registration did not complete in time");
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

    use agent_control_protocol::{
        AgentControlHandoff, ControlEnvelope, ControlRequest, ControlResponse, MARKER_FILE_PATH,
    };
    use anyhow::Context as _;

    /// Bounded backoff for a `NotReady` response, which means the server
    /// recognized the token but the thread's registration hasn't landed yet
    /// (see `ControlTokenState::Reserved`). Keeping the wait client-side
    /// avoids parking requests inside the store.
    const RETRY_BACKOFFS: &[Duration] = &[
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_millis(1000),
    ];

    pub(crate) fn run(
        request: ControlRequest,
        socket_override: Option<PathBuf>,
        token_override: Option<String>,
    ) -> anyhow::Result<ControlResponse> {
        let (socket_path, token) = match (socket_override, token_override) {
            (Some(socket_path), Some(token)) => (socket_path, token),
            (socket_override, token_override) => {
                let handoff = read_marker_file().context(
                    "could not determine the control socket or token -- pass --socket and \
                     --token explicitly, or run this from the working directory of a thread \
                     Flint launched (which should contain a marker file)",
                )?;
                (
                    socket_override.unwrap_or(handoff.socket),
                    token_override.unwrap_or(handoff.token),
                )
            }
        };

        let mut attempt = 0;
        loop {
            let response = send_once(&socket_path, &token, &request)?;
            if !matches!(response, ControlResponse::NotReady) || attempt >= RETRY_BACKOFFS.len() {
                return Ok(response);
            }
            std::thread::sleep(RETRY_BACKOFFS[attempt]);
            attempt += 1;
        }
    }

    fn read_marker_file() -> anyhow::Result<AgentControlHandoff> {
        let cwd = std::env::current_dir().context("failed to determine the current directory")?;
        let marker_path = cwd.join(MARKER_FILE_PATH);
        let contents = std::fs::read_to_string(&marker_path)
            .with_context(|| format!("failed to read {}", marker_path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", marker_path.display()))
    }

    fn send_once(
        socket_path: &std::path::Path,
        token: &str,
        request: &ControlRequest,
    ) -> anyhow::Result<ControlResponse> {
        let mut stream = UnixStream::connect(socket_path).with_context(|| {
            format!(
                "failed to connect to Flint's agent control socket at {}",
                socket_path.display()
            )
        })?;
        let envelope = ControlEnvelope {
            token: token.to_string(),
            request: request.clone(),
        };
        let payload = serde_json::to_vec(&envelope).context("failed to encode request")?;
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
