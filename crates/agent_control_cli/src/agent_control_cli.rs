//! `flint-agent-control`: the binary an agent thread's own CLI process (Codex,
//! Claude Code, etc.) invokes to ask Flint to re-tie itself to a different
//! worktree, or spawn a sibling thread. Talks to `agent_threads::control`'s
//! Unix socket server over `agent_control_protocol`'s wire types. Unix-only
//! for this pass -- see the `run` stub below for why the crate still builds
//! (but does nothing) on Windows, where it participates in CI as a workspace
//! member without ever being bundled.
//!
//! Sends bare, unauthenticated-looking requests on purpose: the server
//! establishes caller identity itself, from the kernel-reported PID of
//! whatever process actually connected to the socket (see
//! `agent_threads::control`'s peer-credential resolution), not from
//! anything this binary presents. There is nothing here to mint, deliver,
//! or leak. `--socket` is computed the same way the server computes it
//! (`agent_control_protocol::socket_path()`) and is only ever an override
//! for testing.

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
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
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

    match run(request, socket_override) {
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

#[cfg(not(unix))]
fn run(
    _request: ControlRequest,
    _socket_override: Option<PathBuf>,
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
            eprintln!(
                "flint-agent-control: Flint did not recognize this caller in time -- is this \
                 running inside a Flint agent thread terminal?"
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
