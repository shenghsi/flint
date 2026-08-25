//! The local-only server backing agent-initiated worktree
//! control: a thread's own CLI process (Codex, Claude Code, etc.) invokes
//! the `flintctl` binary (`agent_control_cli`), which sends one
//! JSON request over the platform transport per invocation.
//!
//! Caller identity is established by asking the kernel who actually
//! connected -- `LOCAL_PEERPID` on macOS, `SO_PEERCRED` on Linux, or the
//! named-pipe client PID on Windows -- and
//! walking that process's parent-PID ancestry (via `sysinfo`) looking for a
//! PID Flint recognizes as a live thread's own terminal process. There is
//! no client-presented secret: nothing is minted, delivered, or can go
//! stale by being overwritten. See `agent_control_protocol::AgentControlLocation`
//! for why environment variables and a per-thread token were tried first
//! and abandoned.
//!
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
use agent_control_protocol::ControlResult;
use agent_control_protocol::{
    ControlCommand, ControlErrorCode, ControlRequest, ControlResponse, ControlSuccess,
    CreateThreadRequest, CreateThreadWorktree, MAX_RESPONSE_BYTES, PROTOCOL_VERSION,
    RemoteControlEnvelope, RetieThreadRequest, StatusResult, TerminalControlId,
    TerminalOpenRequest, TerminalOutputMatcher, TerminalReadRequest, TerminalRunRequest,
    TerminalSendKeyRequest, TerminalSendTextRequest, TerminalSnapshot, TerminalSplitRequest,
    TerminalWaitOutputRequest,
};
#[cfg(unix)]
use agent_control_protocol::{FRAME_LENGTH_BYTES, MAX_REQUEST_BYTES, frame_payload};
#[cfg(unix)]
use anyhow::{Context as _, Result};
use collections::HashMap;
#[cfg(unix)]
use gpui::Task;
use gpui::{App, AsyncApp, Entity, EntityId, Keystroke, WindowHandle};
#[cfg(unix)]
use net::async_net::{UnixListener, UnixStream};
use settings::Settings as _;
#[cfg(unix)]
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};
use terminal_view::terminal_panel::{TerminalPanel, TerminalPlacementError};
use util::ResultExt as _;
use workspace::{AppState, MultiWorkspace, SplitDirection, Workspace};

use crate::agent_kind_registry;
use crate::store::{self, AgentThreadStore, LiveTerminalWorktree};

/// How far up a connecting process's parent chain to look for a match
/// before giving up. Generous relative to the shallow real-world case (the
/// CLI is typically a direct child of the tracked terminal process, or one
/// shell layer removed) without risking an unbounded walk.
const MAX_ANCESTRY_DEPTH: usize = 32;

#[cfg(unix)]
pub(crate) type ControlServerHandle = Task<()>;

#[cfg(windows)]
pub(crate) use crate::control_windows::ControlServerHandle;

/// Starts the accept-loop exactly once and stashes its `Task` on the
/// `AgentThreadStore` global so its lifetime is the app's, not a caller's.
#[cfg(unix)]
pub(crate) fn init(cx: &mut App) {
    let store = AgentThreadStore::global(cx);
    let socket_path = agent_control_protocol::socket_path();
    let executable_location_path = agent_control_protocol::executable_location_path();
    write_executable_location(&executable_location_path);
    let owns_socket = Arc::new(AtomicBool::new(false));
    let task = cx.spawn({
        let socket_path = socket_path.clone();
        let owns_socket = owns_socket.clone();
        async move |cx| {
            if let Err(error) = run_server(socket_path, owns_socket, store, cx).await {
                log::error!("agent_threads: agent control server did not start: {error:#}");
            }
        }
    });
    cx.on_app_quit(move |_cx| {
        let socket_path = socket_path.clone();
        let owns_socket = owns_socket.clone();
        async move {
            // Only remove these if this instance actually bound the socket --
            // an instance that detected another live owner and disabled
            // itself must never unlink files it doesn't own.
            if owns_socket.load(Ordering::Acquire) {
                std::fs::remove_file(&socket_path).ok();
            }
        }
    })
    .detach();
    let store = AgentThreadStore::global(cx);
    store.update(cx, |store, _cx| store.hold_control_server(task));
}

#[cfg(windows)]
pub(crate) fn init(cx: &mut App) {
    crate::control_windows::init(cx);
}

#[cfg(unix)]
async fn run_server(
    socket_path: PathBuf,
    owns_socket: Arc<AtomicBool>,
    store: Entity<AgentThreadStore>,
    cx: &mut AsyncApp,
) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {parent:?}"))?;
    }

    if socket_path.exists() {
        match UnixStream::connect(&socket_path).await {
            Ok(_stream) => {
                log::info!(
                    "agent_threads: another Flint instance already owns the agent control socket at {socket_path:?}; disabling this instance's control server"
                );
                return Ok(());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                // Stale: the owning process is gone. Safe to reclaim.
                std::fs::remove_file(&socket_path).ok();
            }
            Err(error) => {
                return Err(error).context("failed to probe the existing agent control socket");
            }
        }
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind the agent control socket at {socket_path:?}"))?;
    smol::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
        .await
        .with_context(|| format!("failed to set permissions on {socket_path:?}"))?;
    owns_socket.store(true, Ordering::Release);

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("failed to accept an agent control connection")?;
        let store = store.clone();
        cx.spawn(async move |cx| {
            if let Err(error) = handle_connection(stream, store, cx).await {
                log::warn!("agent_threads: agent control request failed: {error:#}");
            }
        })
        .detach();
    }
}

/// Records where this Flint instance's own `flintctl` executable
/// lives, so an agent's CLI process can discover what command to run in the
/// first place. Best-effort: if the executable can't be resolved (e.g. a
/// dev build with no bundled binary alongside it), the server still starts
/// -- an explicit platform endpoint override or a PATH-installed binary can
/// still reach it, just not via this file.
pub(crate) fn write_executable_location(executable_location_path: &std::path::Path) -> bool {
    let executable = match util::get_flintctl_path() {
        Ok(executable) => executable,
        Err(error) => {
            log::warn!(
                "agent_threads: could not resolve flintctl's own path, so agents \
                 won't be able to discover it via the marker file: {error:#}"
            );
            return false;
        }
    };
    write_executable_location_for(executable_location_path, executable)
}

pub(crate) fn write_executable_location_for(
    executable_location_path: &std::path::Path,
    executable: std::path::PathBuf,
) -> bool {
    if let Some(parent) = executable_location_path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        log::error!("failed to create agent control marker directory {parent:?}: {error:#}");
        return false;
    }
    let location = agent_control_protocol::AgentControlLocation { executable };
    let Some(json) = serde_json::to_string_pretty(&location).log_err() else {
        return false;
    };
    let temporary_path =
        executable_location_path.with_extension(format!("{}.tmp", std::process::id()));
    let result = std::fs::write(&temporary_path, json)
        .and_then(|()| replace_marker(&temporary_path, executable_location_path));
    match result {
        Ok(()) => true,
        Err(error) => {
            if let Err(cleanup_error) = std::fs::remove_file(&temporary_path)
                && cleanup_error.kind() != std::io::ErrorKind::NotFound
            {
                log::warn!("failed to clean up temporary agent control marker: {cleanup_error}");
            }
            log::error!("failed to write agent control executable marker: {error:#}");
            false
        }
    }
}

#[cfg(unix)]
pub(crate) fn replace_marker(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
pub(crate) fn replace_marker(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};
    use windows::core::PCWSTR;

    let from = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let to = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both buffers are terminated and remain alive for the call.
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING,
        )
    }
    .map_err(std::io::Error::other)
}

#[cfg(unix)]
async fn handle_connection(
    mut stream: UnixStream,
    store: Entity<AgentThreadStore>,
    cx: &mut AsyncApp,
) -> Result<()> {
    let mut length_bytes = [0; FRAME_LENGTH_BYTES];
    stream
        .read_exact(&mut length_bytes)
        .await
        .context("failed to read request length")?;
    let request_length = u32::from_be_bytes(length_bytes) as usize;
    if request_length > MAX_REQUEST_BYTES {
        anyhow::bail!("request exceeds the {MAX_REQUEST_BYTES}-byte protocol limit");
    }
    let mut request_bytes = vec![0; request_length];
    stream
        .read_exact(&mut request_bytes)
        .await
        .context("failed to read request payload")?;

    let response = match serde_json::from_slice::<ControlRequest>(&request_bytes) {
        Ok(request) => match get_peer_pid(&stream) {
            Ok(peer_pid) => {
                let mut disconnect_stream = stream.clone();
                smol::future::race(dispatch(peer_pid, &request, &store, cx), async move {
                    let mut byte = [0];
                    match disconnect_stream.read(&mut byte).await {
                        Ok(0) => ControlResponse::error(
                            ControlErrorCode::CallerNotRecognized,
                            "control client disconnected",
                        ),
                        Ok(_) => ControlResponse::error(
                            ControlErrorCode::InvalidRequest,
                            "connection contains data after its request frame",
                        ),
                        Err(error) => error_response(format_args!(
                            "failed to observe control client: {error}"
                        )),
                    }
                })
                .await
            }
            Err(error) => error_response(format_args!(
                "could not determine caller identity: {error:#}"
            )),
        },
        Err(error) => ControlResponse::error(
            ControlErrorCode::InvalidRequest,
            format!("malformed request: {error}"),
        ),
    };

    let response_bytes = serde_json::to_vec(&response).context("failed to encode response")?;
    let response_frame =
        frame_payload(&response_bytes, MAX_RESPONSE_BYTES).context("failed to frame response")?;
    stream
        .write_all(&response_frame)
        .await
        .context("failed to write response")?;
    stream.flush().await.context("failed to flush response")?;
    Ok(())
}

/// Returns the PID of whatever process is actually on the other end of
/// `stream`, per the kernel -- not anything the client claims about itself.
#[cfg(target_os = "macos")]
fn get_peer_pid(stream: &UnixStream) -> Result<u32> {
    let pid = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)
        .context("failed to read the connecting process's PID (LOCAL_PEERPID)")?;
    Ok(pid as u32)
}

#[cfg(target_os = "linux")]
fn get_peer_pid(stream: &UnixStream) -> Result<u32> {
    let credentials =
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
            .context("failed to read the connecting process's credentials (SO_PEERCRED)")?;
    Ok(credentials.pid() as u32)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn get_peer_pid(_stream: &UnixStream) -> Result<u32> {
    anyhow::bail!("peer-credential resolution is not supported on this platform")
}

/// Resolves which live thread (if any) is the peer on the other end of an
/// agent-control connection.
///
/// First walks up from `peer_pid`'s own process looking for a PID present
/// in `tracked_pids` (see `AgentThreadStore::live_terminal_pids`) -- this
/// is the strong signal, and covers the common case where the CLI's
/// tool-call shell is a direct (or one-hop) descendant of the thread's own
/// terminal process.
///
/// Falls back to matching `peer_pid`'s own current working directory
/// against each entry in `tracked_worktrees` (see
/// `AgentThreadStore::live_terminal_worktree_roots`) when ancestry finds
/// nothing within `MAX_ANCESTRY_DEPTH` hops. This matters for CLIs whose
/// tool-call shells are NOT descendants of the interactive session at all
/// -- Codex CLI, for instance, delegates shell execution to a separate,
/// already-running `codex app-server` daemon rather than forking it as its
/// own child, so no ancestor PID is ever one Flint tracks. A tool-invoked
/// shell's cwd identifies the tied worktree in the usual case. Immediately
/// after the agent creates a linked worktree, its cwd can instead identify
/// that new worktree; the shared git common directory then identifies the
/// originating repository.
///
/// A cwd can match more than one tracked thread -- two threads tied to the
/// same worktree is a real case, not just hypothetical. When that happens,
/// `resolve_by_cwd` narrows using the connecting process's own ancestry
/// process names: even though none of them is a tracked *pid* (that's why
/// we're in this fallback at all), the delegating daemon's own ancestry
/// still usually includes the CLI's own process name (e.g. "codex"),
/// which is reliably equal to its `kind_id`. Still-ambiguous or
/// no-match-at-all cases are reported as unresolved rather than guessed at.
///
/// Logs on failure, including the walked ancestry chain and the cwd
/// candidates considered. This has turned out to be load-bearing for
/// diagnosing real-world agent CLIs whose process topology doesn't match
/// what a synthetic test can model.
fn resolve_caller_thread(
    peer_pid: u32,
    tracked_pids: &HashMap<u32, EntityId>,
    tracked_worktrees: &[LiveTerminalWorktree],
) -> Option<EntityId> {
    if tracked_pids.is_empty() && tracked_worktrees.is_empty() {
        log::warn!(
            "agent_threads: agent control caller {peer_pid} could not be resolved: no threads are tracked yet"
        );
        return None;
    }
    let refresh = sysinfo::ProcessRefreshKind::nothing()
        .with_cmd(sysinfo::UpdateKind::Always)
        .with_exe(sysinfo::UpdateKind::Always)
        .with_cwd(sysinfo::UpdateKind::Always);
    let mut system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(refresh),
    );
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, refresh);

    if let Some(terminal_item_id) = walk_ancestry_for_match(peer_pid, &system, tracked_pids) {
        return Some(terminal_item_id);
    }
    if let Some(terminal_item_id) = resolve_by_cwd(peer_pid, &system, tracked_worktrees) {
        return Some(terminal_item_id);
    }

    log::warn!(
        "agent_threads: agent control caller {peer_pid} could not be resolved: ancestry {:?} \
         matched none of the tracked pids {:?}, and its cwd/kind matched none (or more than one) \
         of the tracked worktrees {:?}",
        ancestry_chain(peer_pid, &system),
        tracked_pids.keys().collect::<Vec<_>>(),
        tracked_worktrees
            .iter()
            .map(|candidate| (
                candidate.terminal_item_id,
                &candidate.tied_worktree_root,
                candidate.kind_id
            ))
            .collect::<Vec<_>>(),
    );
    None
}

fn walk_ancestry_for_match(
    peer_pid: u32,
    system: &sysinfo::System,
    tracked_pids: &HashMap<u32, EntityId>,
) -> Option<EntityId> {
    let mut current = sysinfo::Pid::from_u32(peer_pid);
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if let Some(&terminal_item_id) = tracked_pids.get(&current.as_u32()) {
            return Some(terminal_item_id);
        }
        let parent = system.process(current)?.parent()?;
        if parent == current {
            return None;
        }
        #[cfg(windows)]
        if !valid_windows_parent_hop(current.as_u32(), parent.as_u32()) {
            log::warn!(
                "agent_threads: rejected process ancestry hop {} -> {} because its creation times could not prove the relationship",
                current.as_u32(),
                parent.as_u32()
            );
            return None;
        }
        current = parent;
    }
    None
}

/// Matched against `peer_pid` itself, not an ancestor -- the delegated
/// shell's own cwd is the signal, and it's the leaf of the chain, closest
/// to where the command actually runs. Ancestors further up (a shared
/// daemon or a login shell) have no reason to share the session's cwd.
fn resolve_by_cwd(
    peer_pid: u32,
    system: &sysinfo::System,
    tracked_worktrees: &[LiveTerminalWorktree],
) -> Option<EntityId> {
    let cwd = system.process(sysinfo::Pid::from_u32(peer_pid))?.cwd()?;
    let cwd_matches: Vec<&LiveTerminalWorktree> = tracked_worktrees
        .iter()
        .filter(|candidate| path_is_within(cwd, &candidate.tied_worktree_root))
        .collect();

    if !cwd_matches.is_empty() {
        return select_cwd_candidate(peer_pid, system, &cwd_matches);
    }

    let cwd_common_dir = git_common_dir(cwd)?;
    let repository_matches = tracked_worktrees
        .iter()
        .filter(|candidate| {
            git_common_dir(&candidate.tied_worktree_root).as_ref() == Some(&cwd_common_dir)
        })
        .collect::<Vec<_>>();
    select_cwd_candidate(peer_pid, system, &repository_matches)
}

fn select_cwd_candidate(
    peer_pid: u32,
    system: &sysinfo::System,
    candidates: &[&LiveTerminalWorktree],
) -> Option<EntityId> {
    match candidates {
        [] => None,
        [only] => Some(only.terminal_item_id),
        multiple => {
            let ancestry_names = ancestry_chain(peer_pid, system)
                .into_iter()
                .map(|(_, name)| normalized_process_name(&name))
                .collect::<Vec<_>>();
            let by_kind = multiple
                .iter()
                .filter(|candidate| {
                    let kind = normalized_process_name(candidate.kind_id);
                    ancestry_names.iter().any(|name| name == &kind)
                })
                .collect::<Vec<_>>();
            match by_kind.as_slice() {
                [only] => Some(only.terminal_item_id),
                _ => None,
            }
        }
    }
}

#[cfg(unix)]
fn path_is_within(path: &std::path::Path, root: &std::path::Path) -> bool {
    path == root || path.starts_with(root)
}

#[cfg(windows)]
fn path_is_within(path: &std::path::Path, root: &std::path::Path) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(root) = std::fs::canonicalize(root) else {
        return false;
    };
    let path_components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>();
    let root_components = root
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>();
    path_components.starts_with(&root_components)
}

fn normalized_process_name(name: &str) -> String {
    let normalized = name.to_lowercase();
    normalized
        .strip_suffix(".exe")
        .unwrap_or(&normalized)
        .to_string()
}

#[cfg(windows)]
fn valid_windows_parent_hop(child_pid: u32, parent_pid: u32) -> bool {
    let Some(child_created) = windows_process_creation_time(child_pid) else {
        return false;
    };
    let Some(parent_created) = windows_process_creation_time(parent_pid) else {
        return false;
    };
    valid_windows_parent_creation_times(child_created, parent_created)
}

#[cfg(windows)]
fn valid_windows_parent_creation_times(child_created: u64, parent_created: u64) -> bool {
    child_created >= parent_created
}

#[cfg(windows)]
pub(crate) fn windows_process_creation_time(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: the returned process handle is owned and closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all FILETIME pointers are valid writable storage.
    let result =
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
    // SAFETY: process is the valid owned handle returned by OpenProcess.
    if let Err(error) = unsafe { CloseHandle(process) } {
        log::warn!("agent_threads: failed to close process {pid} after querying times: {error}");
    }
    result.ok()?;
    Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
}

fn git_common_dir(path: &std::path::Path) -> Option<PathBuf> {
    let repository_root = path
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())?;
    let dot_git = repository_root.join(".git");
    if dot_git.is_dir() {
        return std::fs::canonicalize(dot_git).ok();
    }

    let dot_git_contents = std::fs::read_to_string(&dot_git).ok()?;
    let git_dir = dot_git_contents.trim().strip_prefix("gitdir:")?.trim();
    let git_dir = std::path::Path::new(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        repository_root.join(git_dir)
    };
    let common_dir = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
    std::fs::canonicalize(git_dir.join(common_dir.trim())).ok()
}

/// Walks up from `peer_pid` through its process ancestry (closest first),
/// returning each hop's pid and process name. Used both for
/// `resolve_by_cwd`'s same-kind disambiguation and for diagnostic logging
/// on failure.
fn ancestry_chain(peer_pid: u32, system: &sysinfo::System) -> Vec<(u32, String)> {
    let mut chain = Vec::new();
    let mut current = sysinfo::Pid::from_u32(peer_pid);
    for _ in 0..MAX_ANCESTRY_DEPTH {
        let Some(process) = system.process(current) else {
            chain.push((current.as_u32(), "<gone>".to_string()));
            break;
        };
        chain.push((
            current.as_u32(),
            process.name().to_string_lossy().into_owned(),
        ));
        match process.parent() {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    chain
}

pub(crate) async fn dispatch(
    peer_pid: u32,
    request: &ControlRequest,
    store: &Entity<AgentThreadStore>,
    cx: &mut AsyncApp,
) -> ControlResponse {
    if request.protocol.major != PROTOCOL_VERSION.major {
        return ControlResponse::error(
            ControlErrorCode::UnsupportedProtocol,
            format!(
                "unsupported protocol major {}; server supports {}",
                request.protocol.major, PROTOCOL_VERSION.major
            ),
        );
    }
    if matches!(request.command, ControlCommand::Status) {
        let (flint_version, release_channel) = cx.update(|cx| {
            (
                release_channel::AppVersion::global(cx).to_string(),
                release_channel::ReleaseChannel::try_global(cx)
                    .unwrap_or(*release_channel::RELEASE_CHANNEL)
                    .dev_name()
                    .to_string(),
            )
        });
        return ControlResponse::ok(ControlSuccess::Status(StatusResult {
            flint_version,
            protocol_version: PROTOCOL_VERSION,
            release_channel,
            capabilities: command_capabilities(),
        }));
    }
    let (tracked_pids, tracked_worktrees) = store.read_with(cx, |store, cx| {
        (
            store.live_terminal_pids(cx),
            store.live_terminal_worktree_roots(),
        )
    });
    let records = cx.update(crate::terminal_control::records);
    let caller_record = resolve_terminal_caller(peer_pid, &records).or_else(|| {
        let terminal_item_id = resolve_caller_thread(peer_pid, &tracked_pids, &tracked_worktrees)?;
        records.iter().find(|record| {
            record
                .view
                .upgrade()
                .is_some_and(|view| view.entity_id() == terminal_item_id)
        })
    });

    if matches!(
        request.command,
        ControlCommand::TerminalCurrent
            | ControlCommand::TerminalList(_)
            | ControlCommand::TerminalOpen(_)
            | ControlCommand::TerminalSplit(_)
            | ControlCommand::TerminalRead(_)
            | ControlCommand::TerminalSendText(_)
            | ControlCommand::TerminalSendKey(_)
            | ControlCommand::TerminalRun(_)
            | ControlCommand::TerminalWaitOutput(_)
    ) {
        let Some(caller_record) = caller_record else {
            return ControlResponse::not_ready();
        };
        return dispatch_terminal(caller_record, &records, request, cx).await;
    }

    let agent_control_enabled =
        cx.update(|cx| crate::AgentThreadSettings::get_global(cx).agent_control);
    if !agent_control_enabled {
        return error_response("agent_threads.agent_control is disabled");
    }
    let Some(caller_record) = caller_record else {
        return ControlResponse::not_ready();
    };
    let Some(terminal_item_id) = caller_record
        .view
        .upgrade()
        .filter(|view| view.read_with(cx, |view, _cx| view.is_agent_thread()))
        .map(|view| view.entity_id())
    else {
        return ControlResponse::error(
            ControlErrorCode::CallerNotAgentThread,
            "caller is not an Agent Thread terminal",
        );
    };

    dispatch_for_caller(terminal_item_id, request, store, cx).await
}

pub(crate) async fn dispatch_remote(
    remote_connection_id: crate::terminal_control::RemoteConnectionId,
    envelope: &RemoteControlEnvelope,
    store: &Entity<AgentThreadStore>,
    cx: &mut AsyncApp,
) -> ControlResponse {
    let request = &envelope.control_request;
    if request.protocol.major != PROTOCOL_VERSION.major {
        return ControlResponse::error(
            ControlErrorCode::UnsupportedProtocol,
            format!(
                "unsupported protocol major {}; server supports {}",
                request.protocol.major, PROTOCOL_VERSION.major
            ),
        );
    }

    let caller_record = cx.update(|cx| {
        crate::terminal_control::remote_record(
            remote_connection_id,
            &envelope.remote_terminal_registration_id,
            cx,
        )
    });
    let Some(caller_record) = caller_record else {
        return ControlResponse::error(
            ControlErrorCode::RemoteSessionStale,
            "remote terminal registration is not live on this connection",
        );
    };

    if matches!(request.command, ControlCommand::Status) {
        let (flint_version, release_channel) = cx.update(|cx| {
            (
                release_channel::AppVersion::global(cx).to_string(),
                release_channel::ReleaseChannel::try_global(cx)
                    .unwrap_or(*release_channel::RELEASE_CHANNEL)
                    .dev_name()
                    .to_string(),
            )
        });
        return ControlResponse::ok(ControlSuccess::Status(StatusResult {
            flint_version,
            protocol_version: PROTOCOL_VERSION,
            release_channel,
            capabilities: command_capabilities(),
        }));
    }

    let records = cx.update(crate::terminal_control::records);
    if matches!(
        request.command,
        ControlCommand::TerminalCurrent
            | ControlCommand::TerminalList(_)
            | ControlCommand::TerminalOpen(_)
            | ControlCommand::TerminalSplit(_)
            | ControlCommand::TerminalRead(_)
            | ControlCommand::TerminalSendText(_)
            | ControlCommand::TerminalSendKey(_)
            | ControlCommand::TerminalRun(_)
            | ControlCommand::TerminalWaitOutput(_)
    ) {
        return dispatch_terminal(&caller_record, &records, request, cx).await;
    }

    let agent_control_enabled =
        cx.update(|cx| crate::AgentThreadSettings::get_global(cx).agent_control);
    if !agent_control_enabled {
        return error_response("agent_threads.agent_control is disabled");
    }
    let Some(caller_workspace) = caller_record.workspace.upgrade() else {
        return ControlResponse::error(
            ControlErrorCode::InvalidPlacement,
            "terminal workspace closed",
        );
    };
    if let Err(response) = validate_terminal_route(&caller_record, &caller_workspace, cx) {
        return *response;
    }
    let Some(terminal_item_id) = caller_record
        .view
        .upgrade()
        .filter(|view| view.read_with(cx, |view, _cx| view.is_agent_thread()))
        .map(|view| view.entity_id())
    else {
        return ControlResponse::error(
            ControlErrorCode::CallerNotAgentThread,
            "caller is not an Agent Thread terminal",
        );
    };

    dispatch_for_caller(terminal_item_id, request, store, cx).await
}

fn resolve_terminal_caller(
    peer_pid: u32,
    records: &[crate::terminal_control::TerminalControlRecord],
) -> Option<&crate::terminal_control::TerminalControlRecord> {
    let refresh = sysinfo::ProcessRefreshKind::nothing()
        .with_exe(sysinfo::UpdateKind::Always)
        .with_cwd(sysinfo::UpdateKind::Always);
    let mut system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(refresh),
    );
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, refresh);
    let tracked = records
        .iter()
        .filter_map(|record| {
            record.view.upgrade().and_then(|view| match record.caller {
                crate::terminal_control::TerminalControlCaller::Local { root_process_id } => {
                    Some((root_process_id, view.entity_id()))
                }
                crate::terminal_control::TerminalControlCaller::Remote { .. } => None,
            })
        })
        .collect::<HashMap<_, _>>();
    let view_id = walk_ancestry_for_match(peer_pid, &system, &tracked)?;
    records.iter().find(|record| {
        record
            .view
            .upgrade()
            .is_some_and(|view| view.entity_id() == view_id)
    })
}

async fn dispatch_terminal(
    caller: &crate::terminal_control::TerminalControlRecord,
    records: &[crate::terminal_control::TerminalControlRecord],
    request: &ControlRequest,
    cx: &mut AsyncApp,
) -> ControlResponse {
    match &request.command {
        ControlCommand::TerminalCurrent => cx.update(|cx| {
            crate::terminal_control::metadata(caller, cx)
                .map(|metadata| ControlResponse::ok(ControlSuccess::TerminalCurrent(metadata)))
                .unwrap_or_else(|| terminal_not_found(&caller.id))
        }),
        ControlCommand::TerminalList(options) => cx.update(|cx| {
            let caller_workspace = crate::terminal_control::workspace_id(caller, cx);
            let terminals = records
                .iter()
                .filter(|record| options.all || record.id != caller.id)
                .filter(|record| {
                    crate::terminal_control::workspace_id(record, cx) == caller_workspace
                })
                .filter_map(|record| crate::terminal_control::metadata(record, cx))
                .collect();
            ControlResponse::ok(ControlSuccess::TerminalList(terminals))
        }),
        ControlCommand::TerminalOpen(options) => terminal_open(caller, options, cx).await,
        ControlCommand::TerminalSplit(options) => {
            terminal_split(caller, records, options, cx).await
        }
        ControlCommand::TerminalRead(read) => terminal_read(caller, records, read, cx),
        ControlCommand::TerminalSendText(input) => terminal_send_text(caller, records, input, cx),
        ControlCommand::TerminalSendKey(input) => terminal_send_keys(caller, records, input, cx),
        ControlCommand::TerminalRun(input) => terminal_run(caller, records, input, cx),
        ControlCommand::TerminalWaitOutput(wait) => {
            terminal_wait_output(caller, records, wait, cx).await
        }
        _ => ControlResponse::error(
            ControlErrorCode::InvalidRequest,
            "terminal control command is not implemented",
        ),
    }
}

async fn terminal_open(
    caller: &crate::terminal_control::TerminalControlRecord,
    request: &TerminalOpenRequest,
    cx: &mut AsyncApp,
) -> ControlResponse {
    let Some(view) = caller.view.upgrade() else {
        return terminal_not_found(&caller.id);
    };
    let Some(workspace) = view.read_with(cx, |view, _| view.workspace_handle().upgrade()) else {
        return ControlResponse::error(
            ControlErrorCode::InvalidPlacement,
            "terminal workspace closed",
        );
    };
    if let Err(response) = validate_terminal_route(caller, &workspace, cx) {
        return *response;
    }
    if let Err(response) = validate_local_working_directory(caller, request.cwd.as_deref()) {
        return *response;
    }
    let cwd = request
        .cwd
        .clone()
        .or_else(|| usable_terminal_working_directory(caller, cx));
    let pane = match workspace.read_with(cx, |workspace, _| {
        workspace.pane_for_item_id(view.entity_id())
    }) {
        Some(pane) => pane,
        None => {
            return ControlResponse::error(
                ControlErrorCode::InvalidPlacement,
                "caller terminal has no owning pane",
            );
        }
    };
    let Some(window_handle) = workspace_window(caller, &workspace, cx) else {
        return ControlResponse::error(
            ControlErrorCode::InvalidPlacement,
            "terminal workspace has no window",
        );
    };
    let creation = window_handle.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            TerminalPanel::add_terminal_view_to_pane(
                workspace,
                pane,
                request.focus,
                request.focus,
                window,
                cx,
                |project, cx| project.create_terminal_shell(cwd, cx),
            )
        })
    });
    let creation = match creation {
        Ok(creation) => creation,
        Err(error) => {
            return ControlResponse::error(ControlErrorCode::InvalidPlacement, error.to_string());
        }
    };
    terminal_creation_response(caller, creation.await, cx)
}

async fn terminal_split(
    caller: &crate::terminal_control::TerminalControlRecord,
    records: &[crate::terminal_control::TerminalControlRecord],
    request: &TerminalSplitRequest,
    cx: &mut AsyncApp,
) -> ControlResponse {
    let direction = match parse_split_direction(&request.direction) {
        Ok(direction) => direction,
        Err(response) => return *response,
    };
    let target = if request.current {
        caller
    } else if let Some(terminal_id) = request.terminal_id.as_ref() {
        match accessible_terminal(caller, records, terminal_id, cx) {
            Ok(record) => record,
            Err(response) => return *response,
        }
    } else {
        return ControlResponse::error(
            ControlErrorCode::InvalidPlacement,
            "terminal split requires one target",
        );
    };
    let Some(view) = target.view.upgrade() else {
        return terminal_not_found(&target.id);
    };
    let Some(workspace) = view.read_with(cx, |view, _| view.workspace_handle().upgrade()) else {
        return ControlResponse::error(
            ControlErrorCode::InvalidPlacement,
            "terminal workspace closed",
        );
    };
    if let Err(response) = validate_terminal_route(target, &workspace, cx) {
        return *response;
    }
    if let Err(response) = validate_local_working_directory(target, request.cwd.as_deref()) {
        return *response;
    }
    let cwd = request
        .cwd
        .clone()
        .or_else(|| usable_terminal_working_directory(target, cx));
    let pane = match workspace.read_with(cx, |workspace, _| {
        workspace.pane_for_item_id(view.entity_id())
    }) {
        Some(pane) => pane,
        None => {
            return ControlResponse::error(
                ControlErrorCode::InvalidPlacement,
                "selected terminal has no owning pane",
            );
        }
    };
    let Some(window_handle) = workspace_window(target, &workspace, cx) else {
        return ControlResponse::error(
            ControlErrorCode::InvalidPlacement,
            "terminal workspace has no window",
        );
    };
    let creation = window_handle.update(cx, |_, window, cx| {
        let panel = workspace
            .read(cx)
            .panel::<TerminalPanel>(cx)
            .ok_or_else(|| anyhow::anyhow!("terminal panel is unavailable"))?;
        anyhow::Ok(panel.update(cx, |panel, cx| {
            panel.create_adjacent_terminal_view(
                pane,
                direction,
                request.focus,
                window,
                cx,
                |project, cx| project.create_terminal_shell(cwd, cx),
            )
        }))
    });
    let creation = match creation {
        Ok(Ok(creation)) => creation,
        Ok(Err(error)) | Err(error) => {
            return ControlResponse::error(ControlErrorCode::InvalidPlacement, error.to_string());
        }
    };
    terminal_creation_response(caller, creation.await, cx)
}

fn workspace_window(
    record: &crate::terminal_control::TerminalControlRecord,
    workspace: &Entity<Workspace>,
    cx: &mut AsyncApp,
) -> Option<WindowHandle<MultiWorkspace>> {
    if let Some(window) = record.window {
        return Some(window);
    }
    cx.update(|cx| {
        AppState::try_global(cx)?
            .workspace_store
            .read(cx)
            .workspaces_with_windows()
            .find_map(|(window, candidate)| {
                candidate
                    .upgrade()
                    .filter(|candidate| candidate == workspace)
                    .and_then(|_| window.downcast::<MultiWorkspace>())
            })
    })
}

fn validate_terminal_route(
    record: &crate::terminal_control::TerminalControlRecord,
    workspace: &Entity<Workspace>,
    cx: &mut AsyncApp,
) -> std::result::Result<(), Box<ControlResponse>> {
    let workspace_is_remote = workspace.read_with(cx, |workspace, cx| {
        workspace.project().read(cx).remote_client().is_some()
    });
    let terminal_is_remote = matches!(
        record.caller,
        crate::terminal_control::TerminalControlCaller::Remote { .. }
    );
    if workspace_is_remote == terminal_is_remote {
        Ok(())
    } else {
        Err(Box::new(ControlResponse::error(
            ControlErrorCode::TerminalRouteMismatch,
            "terminal route does not match its workspace",
        )))
    }
}

fn validate_local_working_directory(
    record: &crate::terminal_control::TerminalControlRecord,
    cwd: Option<&std::path::Path>,
) -> std::result::Result<(), Box<ControlResponse>> {
    if matches!(
        record.caller,
        crate::terminal_control::TerminalControlCaller::Remote { .. }
    ) {
        return Ok(());
    }
    if let Some(cwd) = cwd
        && (!cwd.is_absolute() || !cwd.is_dir())
    {
        return Err(Box::new(ControlResponse::error(
            ControlErrorCode::InvalidWorkingDirectory,
            format!("{} is not an absolute existing directory", cwd.display()),
        )));
    }
    Ok(())
}

fn usable_terminal_working_directory(
    record: &crate::terminal_control::TerminalControlRecord,
    cx: &AsyncApp,
) -> Option<PathBuf> {
    let working_directory = record
        .terminal
        .upgrade()
        .and_then(|terminal| terminal.read_with(cx, |terminal, _| terminal.working_directory()))?;
    if working_directory.as_os_str().is_empty() {
        return None;
    }
    if matches!(
        record.caller,
        crate::terminal_control::TerminalControlCaller::Remote { .. }
    ) || working_directory.is_absolute() && working_directory.is_dir()
    {
        Some(working_directory)
    } else {
        None
    }
}

fn terminal_creation_response(
    caller: &crate::terminal_control::TerminalControlRecord,
    result: anyhow::Result<(
        gpui::WeakEntity<terminal::Terminal>,
        gpui::WeakEntity<terminal_view::TerminalView>,
    )>,
    cx: &mut AsyncApp,
) -> ControlResponse {
    let (terminal, view) = match result {
        Ok(created) => created,
        Err(error) => {
            let code = if error.downcast_ref::<TerminalPlacementError>().is_some() {
                ControlErrorCode::TerminalPlacementFailed
            } else if matches!(
                caller.caller,
                crate::terminal_control::TerminalControlCaller::Remote { .. }
            ) {
                ControlErrorCode::RemoteTerminalCreateFailed
            } else {
                ControlErrorCode::TerminalCreateFailed
            };
            return ControlResponse::error(code, error.to_string());
        }
    };
    cx.update(|cx| {
        let (Some(terminal), Some(view)) = (terminal.upgrade(), view.upgrade()) else {
            return ControlResponse::error(
                ControlErrorCode::TerminalPlacementFailed,
                "created terminal closed before registration",
            );
        };
        crate::terminal_control::records(cx)
            .iter()
            .find(|record| {
                record
                    .terminal
                    .upgrade()
                    .is_some_and(|candidate| candidate == terminal)
            })
            .and_then(|record| crate::terminal_control::metadata(record, cx))
            .map(|metadata| ControlResponse::ok(ControlSuccess::TerminalCreated(metadata)))
            .unwrap_or_else(|| {
                if let (Some(workspace), Some(window)) =
                    (view.read(cx).workspace_handle().upgrade(), caller.window)
                {
                    let view_id = view.entity_id();
                    window
                        .update(cx, |_, window, cx| {
                            if let Some(pane) = workspace.read(cx).pane_for_item_id(view_id) {
                                pane.update(cx, |pane, cx| {
                                    pane.remove_item(view_id, false, true, window, cx)
                                });
                            }
                        })
                        .ok();
                }
                ControlResponse::error(
                    ControlErrorCode::TerminalPlacementFailed,
                    "created terminal was not registered",
                )
            })
    })
}

fn parse_split_direction(
    direction: &str,
) -> std::result::Result<SplitDirection, Box<ControlResponse>> {
    match direction {
        "left" => Ok(SplitDirection::Left),
        "right" => Ok(SplitDirection::Right),
        "up" => Ok(SplitDirection::Up),
        "down" => Ok(SplitDirection::Down),
        _ => Err(Box::new(ControlResponse::error(
            ControlErrorCode::InvalidSplitDirection,
            format!("invalid split direction {direction:?}"),
        ))),
    }
}

fn terminal_read(
    caller: &crate::terminal_control::TerminalControlRecord,
    records: &[crate::terminal_control::TerminalControlRecord],
    request: &TerminalReadRequest,
    cx: &mut AsyncApp,
) -> ControlResponse {
    if request.lines > agent_control_protocol::MAX_READ_LINES {
        return ControlResponse::error(
            ControlErrorCode::InvalidRequest,
            format!(
                "line count exceeds {}",
                agent_control_protocol::MAX_READ_LINES
            ),
        );
    }
    if request.since.is_some()
        && request.source != agent_control_protocol::TerminalReadSource::Recent
    {
        return ControlResponse::error(
            ControlErrorCode::InvalidRequest,
            "since is only supported with the default recent source",
        );
    }
    let Some(record) = records
        .iter()
        .find(|record| record.id == request.terminal_id)
    else {
        return terminal_not_found(&request.terminal_id);
    };
    let outside_workspace = cx.update(|cx| {
        crate::terminal_control::workspace_id(record, cx)
            != crate::terminal_control::workspace_id(caller, cx)
    });
    if outside_workspace {
        return ControlResponse::error(
            ControlErrorCode::TerminalOutsideWorkspace,
            "terminal belongs to another workspace",
        );
    }
    cx.update(|cx| {
        let Some(terminal) = record.terminal.upgrade() else {
            return terminal_not_found(&request.terminal_id);
        };
        let Some(metadata) = crate::terminal_control::metadata(record, cx) else {
            return terminal_not_found(&request.terminal_id);
        };
        if let Some(since) = request.since.clone() {
            return match terminal.read(cx).control_snapshot_since(
                terminal_read_cursor(since),
                request.lines,
                RESPONSE_TEXT_BYTE_BUDGET,
                agent_control_protocol::MAX_READ_LINES,
            ) {
                Ok(snapshot) => {
                    ControlResponse::ok(ControlSuccess::TerminalRead(TerminalSnapshot {
                        terminal: metadata,
                        source: request.source,
                        text: snapshot.text,
                        alternate_screen: snapshot.alternate_screen,
                        truncated: false,
                        cursor: protocol_read_cursor(snapshot.cursor),
                    }))
                }
                Err(terminal::ControlReadCursorExpired) => ControlResponse::error(
                    ControlErrorCode::CursorExpired,
                    "cursor is older than the terminal's retained scrollback; read again without since",
                ),
            };
        }
        let snapshot = terminal
            .read(cx)
            .control_snapshot(terminal_snapshot_source(request.source), request.lines);
        let (text, truncated) = bounded_terminal_text(snapshot.text);
        ControlResponse::ok(ControlSuccess::TerminalRead(TerminalSnapshot {
            terminal: metadata,
            source: request.source,
            text,
            alternate_screen: snapshot.alternate_screen,
            truncated,
            cursor: protocol_read_cursor(snapshot.cursor),
        }))
    })
}

fn terminal_not_found(id: &TerminalControlId) -> ControlResponse {
    ControlResponse::error(
        ControlErrorCode::TerminalNotFound,
        format!("terminal {} was not found", id.0),
    )
}

const RESPONSE_METADATA_ALLOWANCE: usize = 4096;
const RESPONSE_TEXT_BYTE_BUDGET: usize = MAX_RESPONSE_BYTES - RESPONSE_METADATA_ALLOWANCE;

fn bounded_terminal_text(mut text: String) -> (String, bool) {
    if text.len() <= RESPONSE_TEXT_BYTE_BUDGET {
        return (text, false);
    }
    let mut end = RESPONSE_TEXT_BYTE_BUDGET;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

fn terminal_read_cursor(
    cursor: agent_control_protocol::TerminalReadCursor,
) -> terminal::ControlReadCursor {
    terminal::ControlReadCursor {
        anchor: cursor.anchor,
    }
}

fn protocol_read_cursor(
    cursor: terminal::ControlReadCursor,
) -> agent_control_protocol::TerminalReadCursor {
    agent_control_protocol::TerminalReadCursor {
        anchor: cursor.anchor,
    }
}

fn terminal_snapshot_source(
    source: agent_control_protocol::TerminalReadSource,
) -> terminal::ControlSnapshotSource {
    match source {
        agent_control_protocol::TerminalReadSource::Visible => {
            terminal::ControlSnapshotSource::Visible
        }
        agent_control_protocol::TerminalReadSource::Recent => {
            terminal::ControlSnapshotSource::Recent
        }
        agent_control_protocol::TerminalReadSource::RecentUnwrapped => {
            terminal::ControlSnapshotSource::RecentUnwrapped
        }
        agent_control_protocol::TerminalReadSource::Detection => {
            terminal::ControlSnapshotSource::Detection
        }
    }
}

fn accessible_terminal<'a>(
    caller: &crate::terminal_control::TerminalControlRecord,
    records: &'a [crate::terminal_control::TerminalControlRecord],
    id: &TerminalControlId,
    cx: &mut AsyncApp,
) -> Result<&'a crate::terminal_control::TerminalControlRecord, Box<ControlResponse>> {
    let record = records
        .iter()
        .find(|record| &record.id == id)
        .ok_or_else(|| Box::new(terminal_not_found(id)))?;
    let outside_workspace = cx.update(|cx| {
        crate::terminal_control::workspace_id(record, cx)
            != crate::terminal_control::workspace_id(caller, cx)
    });
    if outside_workspace {
        return Err(Box::new(ControlResponse::error(
            ControlErrorCode::TerminalOutsideWorkspace,
            "terminal belongs to another workspace",
        )));
    }
    Ok(record)
}

fn terminal_send_text(
    caller: &crate::terminal_control::TerminalControlRecord,
    records: &[crate::terminal_control::TerminalControlRecord],
    request: &TerminalSendTextRequest,
    cx: &mut AsyncApp,
) -> ControlResponse {
    if request.text.contains('\0') {
        return ControlResponse::error(ControlErrorCode::InvalidRequest, "text contains NUL");
    }
    let record = match accessible_terminal(caller, records, &request.terminal_id, cx) {
        Ok(record) => record,
        Err(response) => return *response,
    };
    cx.update(|cx| {
        let Some(terminal) = record.terminal.upgrade() else {
            return terminal_not_found(&request.terminal_id);
        };
        if terminal.read(cx).has_exited() {
            return ControlResponse::error(
                ControlErrorCode::TerminalExited,
                "terminal process has exited",
            );
        }
        terminal.update(cx, |terminal, _cx| {
            terminal.input(request.text.clone().into_bytes());
        });
        ControlResponse::ok(ControlSuccess::TerminalInputAccepted)
    })
}

fn terminal_send_keys(
    caller: &crate::terminal_control::TerminalControlRecord,
    records: &[crate::terminal_control::TerminalControlRecord],
    request: &TerminalSendKeyRequest,
    cx: &mut AsyncApp,
) -> ControlResponse {
    let keys = match request
        .keys
        .iter()
        .map(|key| {
            if !agent_control_protocol::is_supported_terminal_key(key) {
                anyhow::bail!("unsupported terminal key {key:?}");
            }
            Keystroke::parse(key).map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(keys) => keys,
        Err(error) => {
            return ControlResponse::error(ControlErrorCode::InvalidKey, error.to_string());
        }
    };
    let record = match accessible_terminal(caller, records, &request.terminal_id, cx) {
        Ok(record) => record,
        Err(response) => return *response,
    };
    cx.update(|cx| {
        let Some(terminal) = record.terminal.upgrade() else {
            return terminal_not_found(&request.terminal_id);
        };
        if terminal.read(cx).has_exited() {
            return ControlResponse::error(
                ControlErrorCode::TerminalExited,
                "terminal process has exited",
            );
        }
        let option_as_meta =
            terminal::terminal_settings::TerminalSettings::get_global(cx).option_as_meta;
        terminal.update(cx, |terminal, _cx| {
            for key in &keys {
                terminal.try_keystroke(key, option_as_meta);
            }
        });
        ControlResponse::ok(ControlSuccess::TerminalInputAccepted)
    })
}

fn terminal_run(
    caller: &crate::terminal_control::TerminalControlRecord,
    records: &[crate::terminal_control::TerminalControlRecord],
    request: &TerminalRunRequest,
    cx: &mut AsyncApp,
) -> ControlResponse {
    if request.command.contains('\0') {
        return ControlResponse::error(ControlErrorCode::InvalidRequest, "command contains NUL");
    }
    let record = match accessible_terminal(caller, records, &request.terminal_id, cx) {
        Ok(record) => record,
        Err(response) => return *response,
    };
    cx.update(|cx| {
        let Some(terminal) = record.terminal.upgrade() else {
            return terminal_not_found(&request.terminal_id);
        };
        if terminal.read(cx).has_exited() {
            return ControlResponse::error(
                ControlErrorCode::TerminalExited,
                "terminal process has exited",
            );
        }
        let option_as_meta =
            terminal::terminal_settings::TerminalSettings::get_global(cx).option_as_meta;
        let accepted = terminal.update(cx, |terminal, _cx| {
            input_terminal_run(terminal, &request.command, option_as_meta)
        });
        if !accepted {
            return ControlResponse::error(
                ControlErrorCode::Internal,
                "could not map the terminal Enter key",
            );
        }
        ControlResponse::ok(ControlSuccess::TerminalInputAccepted)
    })
}

fn input_terminal_run(
    terminal: &mut terminal::Terminal,
    command: &str,
    option_as_meta: bool,
) -> bool {
    let Ok(enter) = Keystroke::parse("enter") else {
        return false;
    };
    terminal.input(command.as_bytes().to_vec());
    terminal.try_keystroke(&enter, option_as_meta)
}

async fn terminal_wait_output(
    caller: &crate::terminal_control::TerminalControlRecord,
    records: &[crate::terminal_control::TerminalControlRecord],
    request: &TerminalWaitOutputRequest,
    cx: &mut AsyncApp,
) -> ControlResponse {
    let matcher: Box<dyn Fn(&str) -> bool> = match &request.matcher {
        TerminalOutputMatcher::Literal(pattern) => {
            let pattern = pattern.clone();
            Box::new(move |text| text.contains(&pattern))
        }
        TerminalOutputMatcher::Regex(pattern) => match regex::Regex::new(pattern) {
            Ok(regex) => Box::new(move |text| regex.is_match(text)),
            Err(error) => {
                return ControlResponse::error(ControlErrorCode::InvalidPattern, error.to_string());
            }
        },
    };
    let record = match accessible_terminal(caller, records, &request.terminal_id, cx) {
        Ok(record) => record.clone(),
        Err(response) => return *response,
    };
    let generation = record.generation;
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_millis(request.timeout_millis))
        .unwrap_or_else(std::time::Instant::now);
    let Some((output_events, _subscription)) =
        cx.update(|cx| crate::terminal_control::observe_output(&record, cx))
    else {
        return terminal_not_found(&request.terminal_id);
    };
    loop {
        let snapshot = cx.update(|cx| {
            if record.generation != generation {
                return None;
            }
            let terminal = record.terminal.upgrade()?;
            let metadata = crate::terminal_control::metadata(&record, cx)?;
            let terminal_snapshot = terminal
                .read(cx)
                .control_snapshot(terminal_snapshot_source(request.source), request.lines);
            let (text, truncated) = bounded_terminal_text(terminal_snapshot.text);
            Some(TerminalSnapshot {
                terminal: metadata,
                source: request.source,
                text,
                alternate_screen: terminal_snapshot.alternate_screen,
                truncated,
                cursor: protocol_read_cursor(terminal_snapshot.cursor),
            })
        });
        let Some(snapshot) = snapshot else {
            return terminal_not_found(&request.terminal_id);
        };
        if matcher(&snapshot.text) {
            return ControlResponse::ok(ControlSuccess::TerminalWaitOutput(snapshot));
        }
        if std::time::Instant::now() >= deadline {
            return ControlResponse::error(
                ControlErrorCode::Timeout,
                "terminal output did not match before the timeout",
            );
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let output_event = async { output_events.recv().await.is_ok() };
        let timeout = async {
            cx.background_executor().timer(remaining).await;
            false
        };
        if !smol::future::race(output_event, timeout).await {
            return ControlResponse::error(
                ControlErrorCode::Timeout,
                "terminal output did not match before the timeout",
            );
        }
    }
}

/// The authorization-independent core: everything past "which thread is
/// this." Split out from `dispatch` so tests can exercise the retie/create
/// business logic directly against a known thread, without needing a real
/// peer-credentialed connection.
async fn dispatch_for_caller(
    terminal_item_id: EntityId,
    request: &ControlRequest,
    store: &Entity<AgentThreadStore>,
    cx: &mut AsyncApp,
) -> ControlResponse {
    match &request.command {
        ControlCommand::ThreadRetie(request) => {
            handle_retie_thread(terminal_item_id, request, cx).await
        }
        ControlCommand::ThreadCreate(request) => {
            handle_create_thread(terminal_item_id, request, store, cx).await
        }
        _ => ControlResponse::error(
            ControlErrorCode::InvalidRequest,
            "terminal control command is not implemented",
        ),
    }
}

pub(crate) fn error_response(error: impl std::fmt::Display) -> ControlResponse {
    ControlResponse::error(ControlErrorCode::Internal, error.to_string())
}

fn command_capabilities() -> Vec<String> {
    [
        "status",
        "thread-retie",
        "thread-create",
        "terminal-current",
        "terminal-list",
        "terminal-open",
        "terminal-split",
        "terminal-read",
        "terminal-read-since",
        "terminal-send-text",
        "terminal-send-key",
        "terminal-run",
        "terminal-wait-output",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

async fn handle_retie_thread(
    terminal_item_id: EntityId,
    request: &RetieThreadRequest,
    cx: &mut AsyncApp,
) -> ControlResponse {
    // Shallow existence/directory validation is the only authorization
    // check performed on the raw path in this pass; the tie actually
    // committed is always re-derived from the destination workspace's own
    // resolved worktree root inside `store::retie_thread`, never this path.
    if !request.worktree.is_dir() {
        return error_response(format_args!(
            "{} is not an existing directory",
            request.worktree.display()
        ));
    }

    let window_handle = match cx.update(|cx| {
        AgentThreadStore::global(cx)
            .read(cx)
            .thread_window(terminal_item_id)
    }) {
        Ok(window_handle) => window_handle,
        Err(error) => return error_response(error),
    };

    match store::retie_thread(
        terminal_item_id,
        request.worktree.clone(),
        window_handle,
        cx,
    )
    .await
    {
        Ok((tie, _persistence)) => {
            ControlResponse::ok(ControlSuccess::Retied { worktree: tie.root })
        }
        Err(error) => error_response(error),
    }
}

async fn handle_create_thread(
    terminal_item_id: EntityId,
    request: &CreateThreadRequest,
    store: &Entity<AgentThreadStore>,
    cx: &mut AsyncApp,
) -> ControlResponse {
    if request.worktree == CreateThreadWorktree::New && request.split.is_some() {
        return ControlResponse::error(
            ControlErrorCode::InvalidPlacement,
            "--split cannot be used with --worktree new",
        );
    }
    if let Some(direction) = request.split.as_deref()
        && let Err(response) = parse_split_direction(direction)
    {
        return *response;
    }
    let Some(kind) = agent_kind_registry()
        .into_iter()
        .find(|kind| kind.id == request.agent)
    else {
        return error_response(format_args!("unknown agent kind {:?}", request.agent));
    };
    if kind.initial_prompt_strategy == crate::InitialPromptStrategy::Unsupported {
        return error_response(format_args!(
            "{} does not support a seeded initial prompt",
            kind.label
        ));
    }

    let (source_workspace, terminal_view) = match store.read_with(cx, |store, _| {
        store.thread_workspace_and_terminal(terminal_item_id)
    }) {
        Ok(pair) => pair,
        Err(error) => return error_response(error),
    };
    let window_handle = match store.read_with(cx, |store, _| store.thread_window(terminal_item_id))
    {
        Ok(window_handle) => window_handle,
        Err(error) => return error_response(error),
    };
    let creation_error_code = source_workspace.read_with(cx, |workspace, cx| {
        if workspace.project().read(cx).remote_client().is_some() {
            ControlErrorCode::RemoteTerminalCreateFailed
        } else {
            ControlErrorCode::TerminalCreateFailed
        }
    });
    let prompt = request.prompt.trim();
    if prompt.is_empty() || prompt.starts_with('-') {
        return ControlResponse::error(
            creation_error_code,
            "initial prompt must contain text and must not start with '-'",
        );
    }

    match request.worktree {
        CreateThreadWorktree::Current => {
            let Some(source_pane) = source_workspace.read_with(cx, |workspace, _| {
                workspace.pane_for_item_id(terminal_view.entity_id())
            }) else {
                return ControlResponse::error(
                    ControlErrorCode::InvalidPlacement,
                    "caller terminal has no owning pane",
                );
            };
            let placement = if let Some(direction) = request.split.as_deref() {
                let direction = match parse_split_direction(direction) {
                    Ok(direction) => direction,
                    Err(response) => return *response,
                };
                store::SeededThreadPlacement::Split {
                    pane: source_pane,
                    direction,
                    focus: request.focus,
                }
            } else {
                store::SeededThreadPlacement::Tab {
                    pane: source_pane,
                    focus: request.focus,
                }
            };
            let launch = window_handle.update(cx, |_, window, cx| {
                source_workspace.update(cx, |workspace, cx| {
                    let worktree =
                        crate::history::project_worktree_roots(workspace.project().read(cx), cx)
                            .into_iter()
                            .next();
                    let task = store::launch_seeded_thread_at(
                        workspace,
                        &kind,
                        &request.prompt,
                        Some(placement),
                        window,
                        cx,
                    );
                    (worktree, task)
                })
            });
            let (worktree, task) = match launch {
                Ok(launch) => launch,
                Err(error) => return error_response(error),
            };
            seeded_launch_response(worktree, task.await, creation_error_code, cx)
        }
        CreateThreadWorktree::New => {
            let action = flint_actions::CreateWorktree {
                worktree_name: request.name.clone(),
                branch_target: flint_actions::NewWorktreeBranchTarget::CurrentBranch,
            };
            let created_task = window_handle.update(cx, |_, window, cx| {
                source_workspace.update(cx, |workspace, cx| {
                    git_ui::worktree_service::create_worktree_workspace(
                        workspace, &action, window, None, cx,
                    )
                })
            });
            let created_task = match created_task {
                Ok(task) => task,
                Err(error) => return error_response(error),
            };
            let created = match created_task.await {
                Ok(created) => created,
                Err(error) => return error_response(error),
            };
            let launch = window_handle.update(cx, |_, window, cx| {
                created.workspace.update(cx, |workspace, cx| {
                    let worktree =
                        crate::history::project_worktree_roots(workspace.project().read(cx), cx)
                            .into_iter()
                            .next();
                    let task =
                        store::launch_seeded_thread(workspace, &kind, &request.prompt, window, cx);
                    (worktree, task)
                })
            });
            let (worktree, task) = match launch {
                Ok(launch) => launch,
                Err(error) => return error_response(error),
            };
            let launch = task.await;
            if request.focus
                && let Ok(launch) = &launch
            {
                let terminal_view = launch.terminal_view.clone();
                if let Err(error) = window_handle.update(cx, |multi_workspace, window, cx| {
                    multi_workspace.activate(
                        created.workspace.clone(),
                        Some(source_workspace.downgrade()),
                        window,
                        cx,
                    );
                    created.workspace.update(cx, |workspace, cx| {
                        let pane = workspace
                            .pane_for_item_id(terminal_view.entity_id())
                            .ok_or_else(|| anyhow::anyhow!("created Agent Thread has no pane"))?;
                        pane.update(cx, |pane, cx| {
                            let index = pane.index_for_item(&terminal_view).ok_or_else(|| {
                                anyhow::anyhow!("created Agent Thread terminal is not in its pane")
                            })?;
                            pane.activate_item(index, true, true, window, cx);
                            anyhow::Ok(())
                        })
                    })
                }) {
                    return ControlResponse::error(
                        ControlErrorCode::TerminalPlacementFailed,
                        error.to_string(),
                    );
                }
            }
            seeded_launch_response(worktree, launch, creation_error_code, cx)
        }
    }
}

fn seeded_launch_response(
    worktree: Option<PathBuf>,
    result: anyhow::Result<store::SeededThreadLaunch>,
    creation_error_code: ControlErrorCode,
    cx: &mut AsyncApp,
) -> ControlResponse {
    let launch = match result {
        Ok(launch) => launch,
        Err(error) => {
            let code = if error.downcast_ref::<TerminalPlacementError>().is_some() {
                ControlErrorCode::TerminalPlacementFailed
            } else {
                creation_error_code
            };
            return ControlResponse::error(code, error.to_string());
        }
    };
    if !launch.seeded {
        return ControlResponse::error(
            creation_error_code,
            "agent kind does not support a seeded initial prompt",
        );
    }
    let Some(worktree) = worktree else {
        return error_response("could not resolve the new thread's worktree");
    };
    cx.update(|cx| {
        crate::terminal_control::records(cx)
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view == launch.terminal_view)
            })
            .and_then(|record| crate::terminal_control::metadata(record, cx))
            .map(|mut terminal| {
                if terminal.working_directory.is_none() {
                    terminal.working_directory = Some(worktree.clone());
                }
                ControlResponse::ok(ControlSuccess::ThreadCreated { worktree, terminal })
            })
            .unwrap_or_else(|| {
                ControlResponse::error(
                    ControlErrorCode::TerminalPlacementFailed,
                    "created Agent Thread was not registered",
                )
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::Fs;
    use gpui::{AppContext as _, EntityId, Focusable as _, TestAppContext, WindowHandle};
    use project::{FakeFs, Project};
    use settings::{AgentThreadCommandContent, AgentThreadSettingsContent, SettingsStore};
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::sync::Arc;
    use std::sync::LazyLock;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicBool, Ordering};
    use terminal_view::TerminalView;
    use workspace::MultiWorkspace;

    // Tests that actually spawn the echo command need a `cwd` that exists on
    // disk -- mirrors panel.rs's own test fixture for the same reason.
    static SPAWNING_TEST_ROOT: LazyLock<String> =
        LazyLock::new(|| std::env::temp_dir().to_string_lossy().into_owned());

    fn init_test(cx: &mut TestAppContext) {
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
    fn status_reports_terminal_creation_capabilities_separately() {
        let capabilities = command_capabilities();
        assert!(
            capabilities
                .iter()
                .any(|capability| capability == "terminal-open")
        );
        assert!(
            capabilities
                .iter()
                .any(|capability| capability == "terminal-split")
        );
    }

    #[gpui::test]
    fn managed_seeded_launch_failures_reach_typed_control_responses(cx: &mut TestAppContext) {
        let mut async_cx = cx.to_async();
        for message in [
            "managed agent preparation failed",
            "managed agent preparation was cancelled",
            "managed agent preparation is already in progress",
            "managed agent launch failed",
        ] {
            let response = seeded_launch_response(
                Some(PathBuf::from("/remote/worktree")),
                Err(anyhow::anyhow!(message)),
                ControlErrorCode::RemoteTerminalCreateFailed,
                &mut async_cx,
            );
            assert!(matches!(
                response.result,
                ControlResult::Error(ref error)
                    if error.code == ControlErrorCode::RemoteTerminalCreateFailed
                        && error.message == message
            ));
        }
    }

    fn echo_command(label: &str, root_path: &str) -> AgentThreadCommandContent {
        AgentThreadCommandContent {
            command: Some("echo".to_string()),
            args: Some(vec![label.to_string()]),
            env: Some(collections::HashMap::default()),
            cwd: Some(PathBuf::from(root_path)),
            initialization_command: None,
            hidden: None,
            default_launch_option: None,
        }
    }

    fn configure_echo_threads(cx: &mut TestAppContext, root_path: &str) {
        cx.update_global(|store: &mut SettingsStore, cx| {
            store.update_user_settings(cx, |settings| {
                settings.agent_threads = Some(AgentThreadSettingsContent {
                    codex: Some(echo_command("codex", root_path)),
                    ..Default::default()
                });
            });
        });
    }

    async fn init_workspace(
        cx: &mut TestAppContext,
        root_path: &'static str,
    ) -> WindowHandle<MultiWorkspace> {
        let fs = FakeFs::new(cx.executor());
        cx.update(|cx| <dyn Fs>::set_global(fs.clone(), cx));
        let project = Project::test(fs, [std::path::Path::new(root_path)], cx).await;
        cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx))
    }

    fn codex_kind() -> crate::AgentKindDefinition {
        agent_kind_registry()
            .into_iter()
            .find(|kind| kind.id == "codex")
            .expect("codex should be registered")
    }

    fn launch_codex_thread(window_handle: &WindowHandle<MultiWorkspace>, cx: &mut TestAppContext) {
        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    crate::launch_new_thread_with_default(workspace, &codex_kind(), window, cx);
                });
            })
            .expect("failed to launch codex thread");
    }

    fn live_codex_threads(
        cx: &mut TestAppContext,
        project_root: &str,
    ) -> Vec<store::AgentThreadMetadata> {
        cx.update(|cx| {
            AgentThreadStore::global(cx)
                .read(cx)
                .live_threads_for_project(
                    "codex",
                    &[PathBuf::from(project_root)],
                    &store::TieResolution::not_ready(),
                )
        })
    }

    // See panel.rs's identical helper: spawning the underlying process (and
    // its ConPTY on Windows) happens on a real OS thread, so registration
    // can lag behind a plain `run_until_parked()`.
    async fn wait_for_live_count(cx: &mut TestAppContext, project_root: &str, expected: usize) {
        for _ in 0..50 {
            cx.run_until_parked();
            if live_codex_threads(cx, project_root).len() >= expected {
                return;
            }
            cx.executor()
                .timer(std::time::Duration::from_millis(50))
                .await;
        }
    }

    fn terminal_views(
        window_handle: &WindowHandle<MultiWorkspace>,
        cx: &mut TestAppContext,
    ) -> Vec<Entity<TerminalView>> {
        window_handle
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().read(cx);
                let mut views = Vec::new();
                for pane in workspace.panes() {
                    views.extend(pane.read(cx).items_of_type::<TerminalView>());
                }
                views
            })
            .expect("failed to collect terminal views")
    }

    /// Spins up a workspace with one live, locally-spawned echo "codex"
    /// thread and returns its `terminal_item_id` -- the fixture every
    /// `dispatch_for_caller`-level test below builds on, since that
    /// function takes the caller's identity as a plain parameter rather
    /// than resolving it itself.
    async fn spawn_live_codex_thread(
        cx: &mut TestAppContext,
    ) -> (WindowHandle<MultiWorkspace>, EntityId) {
        cx.executor().allow_parking();
        init_test(cx);
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root);
        let window_handle = init_workspace(cx, root).await;

        launch_codex_thread(&window_handle, cx);
        wait_for_live_count(cx, root, 1).await;

        let terminal_item_id = terminal_views(&window_handle, cx)[0].entity_id();
        (window_handle, terminal_item_id)
    }

    #[cfg(unix)]
    #[test]
    fn get_peer_pid_returns_the_actual_connecting_process() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir for the socket");
        let socket_path = temp_dir.path().join("get-peer-pid-test.sock");
        let listener =
            std::os::unix::net::UnixListener::bind(&socket_path).expect("failed to bind");

        let accept_thread = std::thread::spawn(move || {
            let (accepted, _) = listener.accept().expect("failed to accept");
            accepted
        });
        let _client = std::os::unix::net::UnixStream::connect(&socket_path)
            .expect("failed to connect to the test socket");
        let accepted = accept_thread.join().expect("accept thread panicked");

        // `get_peer_pid` takes our async `UnixStream`, not the std one used
        // above to drive the synchronous accept -- convert via the same
        // owned-fd path `net::async_net` itself uses.
        let accepted: UnixStream = accepted
            .try_into()
            .expect("failed to wrap the accepted stream");
        let peer_pid = get_peer_pid(&accepted).expect("failed to read the peer pid");
        assert_eq!(
            peer_pid,
            std::process::id(),
            "the peer should be this same test process, since it dialed the socket itself"
        );
    }

    #[test]
    fn resolve_caller_thread_walks_up_to_a_tracked_parent() {
        let this_pid = std::process::id();
        let mut child = spawn_test_child(None);
        let child_pid = child.id();

        let tracked: HashMap<u32, EntityId> = HashMap::from_iter([(this_pid, EntityId::from(42))]);

        let resolved = resolve_caller_thread(child_pid, &tracked, &[]);
        child.kill().log_err();

        assert_eq!(
            resolved,
            Some(EntityId::from(42)),
            "the child's parent is this test process, which is the tracked PID"
        );
    }

    #[test]
    fn resolve_caller_thread_gives_up_after_the_depth_limit_or_no_match() {
        // PID 1 (init/launchd) is never going to be in any realistic
        // `tracked` map; walking from it should terminate (not hang) and
        // report no match.
        let tracked: HashMap<u32, EntityId> = HashMap::from_iter([(999_999, EntityId::from(1))]);
        assert_eq!(resolve_caller_thread(1, &tracked, &[]), None);
    }

    #[test]
    fn resolve_caller_thread_short_circuits_on_an_empty_tracked_map() {
        assert_eq!(
            resolve_caller_thread(std::process::id(), &HashMap::default(), &[]),
            None
        );
    }

    /// `std::env::temp_dir()` is `/var/...` on macOS, a symlink to
    /// `/private/var/...` -- the kernel reports a spawned process's cwd
    /// already resolved, so comparing against the unresolved path would
    /// spuriously fail. Real worktree roots aren't normally under a
    /// symlinked prefix like this, so `resolve_by_cwd` itself doesn't
    /// canonicalize; only these tests need to.
    fn canonical_temp_dir() -> PathBuf {
        std::fs::canonicalize(std::env::temp_dir())
            .expect("failed to canonicalize the system temp dir")
    }

    fn live_terminal_worktree(
        terminal_item_id: EntityId,
        tied_worktree_root: PathBuf,
        kind_id: &'static str,
    ) -> LiveTerminalWorktree {
        LiveTerminalWorktree {
            terminal_item_id,
            tied_worktree_root,
            kind_id,
        }
    }

    fn spawn_test_child(cwd: Option<&std::path::Path>) -> smol::process::Child {
        #[cfg(unix)]
        let mut command = {
            let mut command = smol::process::Command::new("sleep");
            command.arg("5");
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = smol::process::Command::new("cmd.exe");
            command.args(["/C", "ping -n 6 127.0.0.1 > nul"]);
            command
        };
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn a process inspection test child")
    }

    fn test_child_kind_id() -> &'static str {
        if cfg!(windows) { "cmd" } else { "sleep" }
    }

    #[test]
    fn terminal_text_truncation_keeps_utf8_valid() {
        let text = "界".repeat(MAX_RESPONSE_BYTES);
        let (text, truncated) = bounded_terminal_text(text);
        assert!(truncated);
        assert!(text.len() <= MAX_RESPONSE_BYTES);
        assert!(text.chars().all(|character| character == '界'));
    }

    #[test]
    fn executable_marker_replaces_an_older_flintctl_path() {
        let directory = tempfile::tempdir().expect("create marker directory");
        let marker = directory
            .path()
            .join("agent-control-stable-executable.json");
        std::fs::write(&marker, r#"{"executable":"/old/flintctl"}"#).expect("write old marker");
        let current = PathBuf::from("/current/Flint.app/Contents/MacOS/flintctl");

        assert!(write_executable_location_for(&marker, current.clone()));

        let location: agent_control_protocol::AgentControlLocation =
            serde_json::from_slice(&std::fs::read(&marker).expect("read current marker"))
                .expect("decode current marker");
        assert_eq!(location.executable, current);
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_containment_handles_case_and_mixed_separators() {
        let root = tempfile::tempdir().expect("create path test root");
        let nested = root.path().join("Nested");
        std::fs::create_dir(&nested).expect("create nested directory");
        let differently_cased_root = PathBuf::from(root.path().to_string_lossy().to_uppercase());
        let mixed_separator_nested = PathBuf::from(nested.to_string_lossy().replace('\\', "/"));

        assert!(path_is_within(
            &mixed_separator_nested,
            &differently_cased_root
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_process_names_ignore_exe_suffix_and_case() {
        assert_eq!(normalized_process_name("CoDeX.EXE"), "codex");
        assert_eq!(normalized_process_name("CoDeX.Exe"), "codex");
        assert_eq!(normalized_process_name("codex.exe"), "codex");
    }

    #[cfg(windows)]
    #[test]
    fn windows_parent_created_after_its_reported_child_is_rejected() {
        assert!(!valid_windows_parent_creation_times(100, 101));
        assert!(valid_windows_parent_creation_times(101, 100));
    }

    fn linked_worktree_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp_dir = tempfile::tempdir().expect("failed to create a linked-worktree fixture");
        let main_root = temp_dir.path().join("main");
        let linked_root = temp_dir.path().join("linked");
        let common_dir = main_root.join(".git");
        let linked_git_dir = common_dir.join("worktrees/linked");
        std::fs::create_dir_all(&linked_git_dir).expect("failed to create git admin directories");
        std::fs::create_dir_all(&linked_root).expect("failed to create linked worktree");
        std::fs::write(
            linked_root.join(".git"),
            format!("gitdir: {}\n", linked_git_dir.display()),
        )
        .expect("failed to write linked worktree git file");
        std::fs::write(linked_git_dir.join("commondir"), "../..\n")
            .expect("failed to write common-dir pointer");
        (temp_dir, main_root, linked_root)
    }

    /// Models Codex CLI's real-world topology: the connecting process's
    /// ancestry never reaches a tracked PID (it's delegated through a
    /// daemon that isn't a descendant of any tracked terminal), but its cwd
    /// is still the tracked thread's own tied worktree root.
    #[test]
    fn resolve_caller_thread_falls_back_to_cwd_when_ancestry_has_no_match() {
        let worktree_root = canonical_temp_dir();
        let mut child = spawn_test_child(Some(&worktree_root));
        let child_pid = child.id();

        let no_pids: HashMap<u32, EntityId> = HashMap::default();
        let worktrees = [live_terminal_worktree(
            EntityId::from(7),
            worktree_root,
            "codex",
        )];

        let resolved = resolve_caller_thread(child_pid, &no_pids, &worktrees);
        child.kill().log_err();

        assert_eq!(
            resolved,
            Some(EntityId::from(7)),
            "ancestry has nothing to match, but the child's own cwd is the tracked worktree root"
        );
    }

    #[test]
    fn resolve_caller_thread_matches_a_new_linked_worktree_to_its_repository() {
        let (_temp_dir, main_root, linked_root) = linked_worktree_fixture();
        let mut child = spawn_test_child(Some(&linked_root));
        let child_pid = child.id();
        let worktrees = [live_terminal_worktree(
            EntityId::from(7),
            main_root,
            "codex",
        )];

        let resolved = resolve_caller_thread(child_pid, &HashMap::default(), &worktrees);
        child.kill().log_err();

        assert_eq!(resolved, Some(EntityId::from(7)));
    }

    #[test]
    fn resolve_caller_thread_refuses_an_ambiguous_repository_match() {
        let (_temp_dir, main_root, linked_root) = linked_worktree_fixture();
        let mut child = spawn_test_child(Some(&linked_root));
        let child_pid = child.id();
        let worktrees = [
            live_terminal_worktree(EntityId::from(7), main_root.clone(), "codex"),
            live_terminal_worktree(EntityId::from(8), main_root, "codex"),
        ];

        let resolved = resolve_caller_thread(child_pid, &HashMap::default(), &worktrees);
        child.kill().log_err();

        assert_eq!(resolved, None);
    }

    /// Two threads of the *same* kind tied to the same worktree root is a
    /// real, if rare, case (e.g. two Codex sessions in the same worktree)
    /// -- neither cwd nor kind can tell them apart, so this must report
    /// unresolved rather than pick one.
    #[test]
    fn resolve_caller_thread_refuses_an_ambiguous_cwd_match() {
        let worktree_root = canonical_temp_dir();
        let mut child = spawn_test_child(Some(&worktree_root));
        let child_pid = child.id();

        let no_pids: HashMap<u32, EntityId> = HashMap::default();
        let worktrees = [
            live_terminal_worktree(EntityId::from(7), worktree_root.clone(), "codex"),
            live_terminal_worktree(EntityId::from(8), worktree_root, "codex"),
        ];

        let resolved = resolve_caller_thread(child_pid, &no_pids, &worktrees);
        child.kill().log_err();

        assert_eq!(resolved, None);
    }

    /// The real bug this fallback exists for: a Codex thread and a
    /// different-kind thread (here modeled as "claude") both tied to the
    /// same worktree. cwd alone is ambiguous between them, but the
    /// connecting process's own ancestry contains "sleep" standing in for
    /// the CLI's name -- narrowing to the one candidate whose `kind_id`
    /// appears there.
    #[test]
    fn resolve_caller_thread_disambiguates_same_worktree_by_kind() {
        let worktree_root = canonical_temp_dir();
        let mut child = spawn_test_child(Some(&worktree_root));
        let child_pid = child.id();

        let no_pids: HashMap<u32, EntityId> = HashMap::default();
        // "zzz-decoy-kind" (rather than a real registered kind id like
        // "claude") avoids colliding with this *test's own* ancestry --
        // running under `cargo test` inside a real agent thread terminal
        // means a real kind name can genuinely appear as a live ancestor,
        // which would make this assertion flaky depending on what spawned
        // the test run.
        let worktrees = [
            live_terminal_worktree(
                EntityId::from(7),
                worktree_root.clone(),
                test_child_kind_id(),
            ),
            live_terminal_worktree(EntityId::from(8), worktree_root, "zzz-decoy-kind"),
        ];

        let resolved = resolve_caller_thread(child_pid, &no_pids, &worktrees);
        child.kill().log_err();

        assert_eq!(
            resolved,
            Some(EntityId::from(7)),
            "the connecting process's own name matches only the first candidate's kind_id"
        );
    }

    /// A matching ancestry PID must win even when a (spurious) cwd
    /// candidate is also present, so ancestry stays the authoritative
    /// signal whenever it's available.
    #[test]
    fn resolve_caller_thread_prefers_ancestry_over_cwd() {
        let this_pid = std::process::id();
        let decoy_root = canonical_temp_dir();
        let mut child = spawn_test_child(Some(&decoy_root));
        let child_pid = child.id();

        let tracked: HashMap<u32, EntityId> = HashMap::from_iter([(this_pid, EntityId::from(42))]);
        // A decoy that WOULD resolve via the cwd fallback, to a different
        // thread, if ancestry didn't take priority -- the child's actual
        // cwd is `decoy_root`.
        let worktrees = [live_terminal_worktree(
            EntityId::from(99),
            decoy_root,
            test_child_kind_id(),
        )];

        let resolved = resolve_caller_thread(child_pid, &tracked, &worktrees);
        child.kill().log_err();

        assert_eq!(resolved, Some(EntityId::from(42)));
    }

    #[gpui::test]
    async fn dispatch_for_caller_rejects_a_nonexistent_retie_directory(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::current(ControlCommand::ThreadRetie(RetieThreadRequest {
            worktree: PathBuf::from("/definitely/does/not/exist/anywhere"),
        }));
        let mut async_cx = cx.to_async();
        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;
        assert!(matches!(response.result, ControlResult::Error(_)));
    }

    #[gpui::test]
    async fn dispatch_for_caller_moves_the_terminal_on_retie(cx: &mut TestAppContext) {
        let (window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let root_b = std::env::temp_dir().join("agent_control_dispatch_retie_test");
        std::fs::create_dir_all(&root_b).expect("failed to create the retie target directory");

        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::current(ControlCommand::ThreadRetie(RetieThreadRequest {
            worktree: root_b.clone(),
        }));
        let mut async_cx = cx.to_async();
        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;
        match response.result {
            ControlResult::Ok(ControlSuccess::Retied { worktree }) => {
                assert_eq!(worktree, root_b);
            }
            other => panic!("expected a successful retie, got {other:?}"),
        }
        cx.run_until_parked();
        let _ = window_handle;
    }

    #[gpui::test]
    async fn terminal_current_list_read_and_immediate_wait_use_the_registered_terminal(
        cx: &mut TestAppContext,
    ) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let caller = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .cloned()
            .expect("spawned terminal must be registered");
        let mut async_cx = cx.to_async();

        let current = dispatch_terminal(
            &caller,
            &records,
            &ControlRequest::current(ControlCommand::TerminalCurrent),
            &mut async_cx,
        )
        .await;
        let id = match current.result {
            ControlResult::Ok(ControlSuccess::TerminalCurrent(metadata)) => metadata.id,
            other => panic!("expected terminal current result, got {other:?}"),
        };

        let list = dispatch_terminal(
            &caller,
            &records,
            &ControlRequest::current(ControlCommand::TerminalList(
                agent_control_protocol::TerminalListRequest { all: true },
            )),
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            list.result,
            ControlResult::Ok(ControlSuccess::TerminalList(ref terminals))
                if terminals.iter().any(|terminal| terminal.id == id)
        ));

        let read = dispatch_terminal(
            &caller,
            &records,
            &ControlRequest::current(ControlCommand::TerminalRead(TerminalReadRequest {
                terminal_id: id.clone(),
                source: agent_control_protocol::TerminalReadSource::Recent,
                lines: 120,
                since: None,
            })),
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            read.result,
            ControlResult::Ok(ControlSuccess::TerminalRead(_))
        ));

        let wait = dispatch_terminal(
            &caller,
            &records,
            &ControlRequest::current(ControlCommand::TerminalWaitOutput(
                TerminalWaitOutputRequest {
                    terminal_id: id,
                    source: agent_control_protocol::TerminalReadSource::Recent,
                    lines: 120,
                    matcher: TerminalOutputMatcher::Literal(String::new()),
                    timeout_millis: 1_000,
                },
            )),
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            wait.result,
            ControlResult::Ok(ControlSuccess::TerminalWaitOutput(_))
        ));
    }

    #[gpui::test]
    async fn terminal_split_rejects_a_raw_invalid_direction(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let caller = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .cloned()
            .expect("spawned terminal must be registered");
        let request = ControlRequest::current(ControlCommand::TerminalSplit(
            agent_control_protocol::TerminalSplitRequest {
                current: true,
                terminal_id: None,
                direction: "diagonal".to_string(),
                cwd: None,
                focus: false,
            },
        ));
        let mut async_cx = cx.to_async();

        let response = dispatch_terminal(&caller, &records, &request, &mut async_cx).await;

        assert!(matches!(
            response.result,
            ControlResult::Error(ref error)
                if error.code == ControlErrorCode::InvalidSplitDirection
        ));
    }

    #[gpui::test]
    async fn terminal_open_rejects_a_nonexistent_local_working_directory(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let caller = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .cloned()
            .expect("spawned terminal must be registered");
        let request = ControlRequest::current(ControlCommand::TerminalOpen(
            agent_control_protocol::TerminalOpenRequest {
                cwd: Some(PathBuf::from("/definitely/not/a/real/flint-directory")),
                focus: false,
            },
        ));
        let mut async_cx = cx.to_async();

        let response = dispatch_terminal(&caller, &records, &request, &mut async_cx).await;

        assert!(matches!(
            response.result,
            ControlResult::Error(ref error)
                if error.code == ControlErrorCode::InvalidWorkingDirectory
        ));
    }

    #[gpui::test]
    async fn terminal_open_returns_registered_metadata_in_the_callers_pane(
        cx: &mut TestAppContext,
    ) {
        let (window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let caller = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .cloned()
            .expect("spawned terminal must be registered");
        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    if workspace.panel::<TerminalPanel>(cx).is_none() {
                        let panel = cx.new(|cx| TerminalPanel::new(workspace, window, cx));
                        workspace.add_panel(panel, window, cx);
                    }
                });
            })
            .expect("install terminal panel");
        let source_pane = caller
            .workspace
            .upgrade()
            .and_then(|workspace| {
                workspace.read_with(cx, |workspace, _| {
                    workspace.pane_for_item_id(terminal_item_id)
                })
            })
            .expect("caller pane");
        let source_item_id = source_pane.read_with(cx, |pane, _| {
            pane.active_item().expect("caller item").item_id()
        });
        let request = ControlRequest::current(ControlCommand::TerminalOpen(
            agent_control_protocol::TerminalOpenRequest {
                cwd: None,
                focus: false,
            },
        ));
        let mut async_cx = cx.to_async();

        let response = dispatch_terminal(&caller, &records, &request, &mut async_cx).await;

        let metadata = match response.result {
            ControlResult::Ok(ControlSuccess::TerminalCreated(metadata)) => metadata,
            other => panic!("expected terminal-created, got {other:?}"),
        };
        assert!(!metadata.is_agent_thread);
        assert_ne!(metadata.id, caller.id);
        window_handle
            .update(cx, |_, _, cx| {
                assert_eq!(source_pane.read(cx).items_len(), 2);
                assert_eq!(
                    source_pane
                        .read(cx)
                        .active_item()
                        .expect("caller stays active")
                        .item_id(),
                    source_item_id
                );
            })
            .expect("inspect caller pane");

        let focused_request = ControlRequest::current(ControlCommand::TerminalOpen(
            agent_control_protocol::TerminalOpenRequest {
                cwd: None,
                focus: true,
            },
        ));
        let focused_response =
            dispatch_terminal(&caller, &records, &focused_request, &mut async_cx).await;
        let focused_id = match focused_response.result {
            ControlResult::Ok(ControlSuccess::TerminalCreated(metadata)) => metadata.id,
            other => panic!("expected focused terminal-created, got {other:?}"),
        };
        let focused_view_id = cx
            .update(crate::terminal_control::records)
            .into_iter()
            .find(|record| record.id == focused_id)
            .and_then(|record| record.view.upgrade())
            .map(|view| view.entity_id())
            .expect("focused terminal view");
        window_handle
            .update(cx, |_, window, cx| {
                assert_eq!(
                    source_pane
                        .read(cx)
                        .active_item()
                        .expect("new terminal is active")
                        .item_id(),
                    focused_view_id
                );
                assert!(
                    source_pane
                        .read(cx)
                        .focus_handle(cx)
                        .contains_focused(window, cx)
                );
            })
            .expect("inspect focused terminal");
    }

    #[gpui::test]
    async fn terminal_split_places_a_registered_terminal_beside_the_selected_center_terminal(
        cx: &mut TestAppContext,
    ) {
        let (window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let caller = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .cloned()
            .expect("spawned terminal must be registered");
        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    if workspace.panel::<TerminalPanel>(cx).is_none() {
                        let panel = cx.new(|cx| TerminalPanel::new(workspace, window, cx));
                        workspace.add_panel(panel, window, cx);
                    }
                });
            })
            .expect("install terminal panel");
        let request = ControlRequest::current(ControlCommand::TerminalSplit(
            agent_control_protocol::TerminalSplitRequest {
                current: true,
                terminal_id: None,
                direction: "right".to_string(),
                cwd: None,
                focus: false,
            },
        ));
        let mut async_cx = cx.to_async();

        let response = dispatch_terminal(&caller, &records, &request, &mut async_cx).await;

        assert!(matches!(
            response.result,
            ControlResult::Ok(ControlSuccess::TerminalCreated(ref metadata))
                if !metadata.is_agent_thread && metadata.id != caller.id
        ));
        window_handle
            .update(cx, |multi_workspace, _, cx| {
                assert_eq!(multi_workspace.workspace().read(cx).panes().len(), 2);
            })
            .expect("inspect workspace panes");
    }

    #[gpui::test]
    async fn terminal_split_places_a_registered_terminal_beside_the_selected_panel_terminal(
        cx: &mut TestAppContext,
    ) {
        let (window_handle, _terminal_item_id) = spawn_live_codex_thread(cx).await;
        let terminal_panel = window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    let panel = cx.new(|cx| TerminalPanel::new(workspace, window, cx));
                    workspace.add_panel(panel.clone(), window, cx);
                    panel
                })
            })
            .expect("install terminal panel");
        let terminal = window_handle
            .update(cx, |_, window, cx| {
                terminal_panel.update(cx, |panel, cx| {
                    let pane = (*panel.panes().first().expect("initial panel pane")).clone();
                    panel.add_terminal_shell_to_pane(pane, None, true, true, window, cx)
                })
            })
            .expect("start panel terminal")
            .await
            .expect("create panel terminal")
            .upgrade()
            .expect("panel terminal remains open");
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let caller = records
            .iter()
            .find(|record| {
                record
                    .terminal
                    .upgrade()
                    .is_some_and(|candidate| candidate == terminal)
            })
            .cloned()
            .expect("panel terminal is registered");
        let request = ControlRequest::current(ControlCommand::TerminalSplit(
            agent_control_protocol::TerminalSplitRequest {
                current: true,
                terminal_id: None,
                direction: "right".to_string(),
                cwd: None,
                focus: false,
            },
        ));
        let mut async_cx = cx.to_async();

        let response = dispatch_terminal(&caller, &records, &request, &mut async_cx).await;

        assert!(matches!(
            response.result,
            ControlResult::Ok(ControlSuccess::TerminalCreated(_))
        ));
        window_handle
            .read_with(cx, |multi_workspace, cx| {
                assert_eq!(multi_workspace.workspace().read(cx).panes().len(), 1);
                assert_eq!(terminal_panel.read(cx).panes().len(), 2);
            })
            .expect("inspect terminal panel panes");
    }

    #[gpui::test]
    async fn ordinary_terminal_can_create_terminals_but_cannot_create_agent_threads(
        cx: &mut TestAppContext,
    ) {
        let (window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    if workspace.panel::<TerminalPanel>(cx).is_none() {
                        let panel = cx.new(|cx| TerminalPanel::new(workspace, window, cx));
                        workspace.add_panel(panel, window, cx);
                    }
                });
            })
            .expect("install terminal panel");
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let caller = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .cloned()
            .expect("spawned Agent Thread must be registered");
        let mut async_cx = cx.to_async();
        let created = terminal_open(
            &caller,
            &agent_control_protocol::TerminalOpenRequest {
                cwd: None,
                focus: false,
            },
            &mut async_cx,
        )
        .await;
        let plain_id = match created.result {
            ControlResult::Ok(ControlSuccess::TerminalCreated(metadata)) => metadata.id,
            other => panic!("expected plain terminal creation, got {other:?}"),
        };
        let plain = async_cx
            .update(crate::terminal_control::records)
            .into_iter()
            .find(|record| record.id == plain_id)
            .expect("plain terminal record");
        let plain_pid = plain
            .terminal
            .upgrade()
            .and_then(|terminal| terminal.read_with(&async_cx, |terminal, _| terminal.pid()))
            .map(|pid| pid.as_u32())
            .expect("plain terminal process id");
        let store = async_cx.update(|cx| AgentThreadStore::global(cx));

        let open = dispatch(
            plain_pid,
            &ControlRequest::current(ControlCommand::TerminalOpen(
                agent_control_protocol::TerminalOpenRequest {
                    cwd: None,
                    focus: false,
                },
            )),
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            open.result,
            ControlResult::Ok(ControlSuccess::TerminalCreated(_))
        ));

        let split = dispatch(
            plain_pid,
            &ControlRequest::current(ControlCommand::TerminalSplit(
                agent_control_protocol::TerminalSplitRequest {
                    current: true,
                    terminal_id: None,
                    direction: "right".to_string(),
                    cwd: None,
                    focus: false,
                },
            )),
            &store,
            &mut async_cx,
        )
        .await;
        assert!(
            matches!(
                split.result,
                ControlResult::Ok(ControlSuccess::TerminalCreated(_))
            ),
            "ordinary terminal split failed: {split:?}"
        );

        let thread = dispatch(
            plain_pid,
            &ControlRequest::current(ControlCommand::ThreadCreate(CreateThreadRequest {
                worktree: CreateThreadWorktree::Current,
                name: None,
                agent: "codex".to_string(),
                prompt: "must be rejected".to_string(),
                split: None,
                focus: false,
            })),
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            thread.result,
            ControlResult::Error(ref error)
                if error.code == ControlErrorCode::CallerNotAgentThread
        ));
    }

    #[gpui::test]
    async fn remote_dispatch_is_bound_to_connection_and_registration(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let (_other_window_handle, other_terminal_item_id) = spawn_live_codex_thread(cx).await;
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let local_record = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .cloned()
            .expect("spawned terminal must be registered");
        let other_terminal_id = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == other_terminal_item_id)
            })
            .map(|record| record.id.clone())
            .expect("other terminal must be registered");
        let terminal = local_record
            .terminal
            .upgrade()
            .expect("terminal must be live");
        let view = local_record.view.upgrade().expect("view must be live");
        let workspace = local_record
            .workspace
            .upgrade()
            .expect("workspace must be live");
        let connection_id = crate::terminal_control::RemoteConnectionId {
            client_entity_id: 7,
            generation: 1,
        };
        let registration_id =
            agent_control_protocol::RemoteTerminalRegistrationId("remote-terminal-7".to_string());
        cx.update(|cx| {
            crate::terminal_control::register_remote_terminal(
                connection_id,
                registration_id.clone(),
                terminal,
                view,
                workspace,
                cx,
            )
        });
        let envelope = RemoteControlEnvelope {
            remote_terminal_registration_id: registration_id,
            control_request: ControlRequest::current(ControlCommand::TerminalCurrent),
        };
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let mut async_cx = cx.to_async();

        let wrong_connection = dispatch_remote(
            crate::terminal_control::RemoteConnectionId {
                client_entity_id: 8,
                generation: 1,
            },
            &envelope,
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            wrong_connection.result,
            ControlResult::Error(ref error)
                if error.code == ControlErrorCode::RemoteSessionStale
        ));

        let wrong_generation = dispatch_remote(
            crate::terminal_control::RemoteConnectionId {
                client_entity_id: 7,
                generation: 2,
            },
            &envelope,
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            wrong_generation.result,
            ControlResult::Error(ref error)
                if error.code == ControlErrorCode::RemoteSessionStale
        ));

        let current = dispatch_remote(connection_id, &envelope, &store, &mut async_cx).await;
        let ControlResult::Ok(ControlSuccess::TerminalCurrent(current)) = current.result else {
            panic!("remote terminal current did not return terminal metadata");
        };

        let request = |command| RemoteControlEnvelope {
            remote_terminal_registration_id: envelope.remote_terminal_registration_id.clone(),
            control_request: ControlRequest::current(command),
        };
        let listed = dispatch_remote(
            connection_id,
            &request(ControlCommand::TerminalList(
                agent_control_protocol::TerminalListRequest { all: false },
            )),
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            listed.result,
            ControlResult::Ok(ControlSuccess::TerminalList(ref terminals))
                if terminals.iter().all(|terminal| terminal.id != current.id)
        ));
        let listed_all = dispatch_remote(
            connection_id,
            &request(ControlCommand::TerminalList(
                agent_control_protocol::TerminalListRequest { all: true },
            )),
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            listed_all.result,
            ControlResult::Ok(ControlSuccess::TerminalList(ref terminals))
                if terminals.iter().any(|terminal| terminal.id == current.id)
        ));
        let read = dispatch_remote(
            connection_id,
            &request(ControlCommand::TerminalRead(TerminalReadRequest {
                terminal_id: current.id,
                source: agent_control_protocol::TerminalReadSource::Recent,
                lines: 120,
                since: None,
            })),
            &store,
            &mut async_cx,
        )
        .await;
        let ControlResult::Ok(ControlSuccess::TerminalRead(snapshot)) = read.result else {
            panic!("remote terminal read did not return a snapshot");
        };
        let incremental = dispatch_remote(
            connection_id,
            &request(ControlCommand::TerminalRead(TerminalReadRequest {
                terminal_id: snapshot.terminal.id.clone(),
                source: agent_control_protocol::TerminalReadSource::Recent,
                lines: 120,
                since: Some(snapshot.cursor),
            })),
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            incremental.result,
            ControlResult::Ok(ControlSuccess::TerminalRead(_))
        ));
        let expired = dispatch_remote(
            connection_id,
            &request(ControlCommand::TerminalRead(TerminalReadRequest {
                terminal_id: snapshot.terminal.id,
                source: agent_control_protocol::TerminalReadSource::Recent,
                lines: 120,
                since: Some(agent_control_protocol::TerminalReadCursor {
                    anchor: "expired-remote-cursor".to_string(),
                }),
            })),
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            expired.result,
            ControlResult::Error(ref error) if error.code == ControlErrorCode::CursorExpired
        ));
        let outside_workspace = dispatch_remote(
            connection_id,
            &request(ControlCommand::TerminalRead(TerminalReadRequest {
                terminal_id: other_terminal_id,
                source: agent_control_protocol::TerminalReadSource::Recent,
                lines: 120,
                since: None,
            })),
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            outside_workspace.result,
            ControlResult::Error(ref error)
                if error.code == ControlErrorCode::TerminalOutsideWorkspace
        ));

        let mismatched_creation = dispatch_remote(
            connection_id,
            &request(ControlCommand::TerminalOpen(
                agent_control_protocol::TerminalOpenRequest {
                    cwd: None,
                    focus: false,
                },
            )),
            &store,
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            mismatched_creation.result,
            ControlResult::Error(ref error)
                if error.code == ControlErrorCode::TerminalRouteMismatch
        ));

        async_cx
            .update(|cx| crate::terminal_control::invalidate_remote_connection(connection_id, cx));
        let disconnected = dispatch_remote(connection_id, &envelope, &store, &mut async_cx).await;
        assert!(matches!(
            disconnected.result,
            ControlResult::Error(ref error)
                if error.code == ControlErrorCode::RemoteSessionStale
        ));
    }

    #[gpui::test]
    async fn terminal_read_since_rejects_a_non_recent_source(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let caller = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .expect("spawned terminal must be registered");
        let mut async_cx = cx.to_async();

        let response = dispatch_terminal(
            caller,
            &records,
            &ControlRequest::current(ControlCommand::TerminalRead(TerminalReadRequest {
                terminal_id: caller.id.clone(),
                source: agent_control_protocol::TerminalReadSource::Visible,
                lines: 120,
                since: Some(agent_control_protocol::TerminalReadCursor {
                    anchor: String::new(),
                }),
            })),
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            response.result,
            ControlResult::Error(ref error) if error.code == ControlErrorCode::InvalidRequest
        ));
    }

    #[gpui::test]
    async fn terminal_read_since_round_trips_the_cursor_and_rejects_an_unrelated_one(
        cx: &mut TestAppContext,
    ) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let caller = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .expect("spawned terminal must be registered");
        let mut async_cx = cx.to_async();

        let read_request = |since| {
            ControlRequest::current(ControlCommand::TerminalRead(TerminalReadRequest {
                terminal_id: caller.id.clone(),
                source: agent_control_protocol::TerminalReadSource::Recent,
                lines: 120,
                since,
            }))
        };

        let first = dispatch_terminal(caller, &records, &read_request(None), &mut async_cx).await;
        let cursor = match first.result {
            ControlResult::Ok(ControlSuccess::TerminalRead(snapshot)) => snapshot.cursor,
            other => panic!("expected a successful read, got {other:?}"),
        };

        // Reusing the cursor with nothing new in between must succeed with
        // an empty delta, not an error -- exercises the same wiring a real
        // "read, then read since" round trip would.
        let unchanged =
            dispatch_terminal(caller, &records, &read_request(Some(cursor)), &mut async_cx).await;
        assert!(matches!(
            unchanged.result,
            ControlResult::Ok(ControlSuccess::TerminalRead(ref snapshot)) if snapshot.text.is_empty()
        ));

        let expired = dispatch_terminal(
            caller,
            &records,
            &read_request(Some(agent_control_protocol::TerminalReadCursor {
                anchor: "text that was never on this terminal".to_string(),
            })),
            &mut async_cx,
        )
        .await;
        assert!(matches!(
            expired.result,
            ControlResult::Error(ref error) if error.code == ControlErrorCode::CursorExpired
        ));
    }

    #[gpui::test]
    async fn terminal_run_writes_enter_separately_from_command_text(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        cx.run_until_parked();
        let records = cx.update(crate::terminal_control::records);
        let terminal = records
            .iter()
            .find(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.entity_id() == terminal_item_id)
            })
            .and_then(|record| record.terminal.upgrade())
            .expect("spawned terminal must be registered");

        let input_log = terminal.update(cx, |terminal, _cx| {
            terminal.take_input_log();
            input_terminal_run(terminal, "claude", false);
            terminal.take_input_log()
        });

        assert_eq!(input_log, vec![b"claude".to_vec(), b"\r".to_vec()]);
    }

    #[gpui::test]
    async fn dispatch_for_caller_rejects_an_unknown_agent(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::current(ControlCommand::ThreadCreate(CreateThreadRequest {
            worktree: CreateThreadWorktree::Current,
            name: None,
            agent: "not-a-real-agent".to_string(),
            prompt: "do the thing".to_string(),
            split: None,
            focus: false,
        }));
        let mut async_cx = cx.to_async();
        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;
        assert!(matches!(response.result, ControlResult::Error(_)));
    }

    #[gpui::test]
    async fn dispatch_for_caller_rejects_split_for_a_new_worktree(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::current(ControlCommand::ThreadCreate(CreateThreadRequest {
            worktree: CreateThreadWorktree::New,
            name: Some("unused-worktree".to_string()),
            agent: "codex".to_string(),
            prompt: "do the thing".to_string(),
            split: Some("right".to_string()),
            focus: false,
        }));
        let mut async_cx = cx.to_async();

        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;

        assert!(matches!(
            response.result,
            ControlResult::Error(ref error) if error.code == ControlErrorCode::InvalidPlacement
        ));
    }

    #[gpui::test]
    async fn dispatch_for_caller_rejects_an_unseedable_prompt_without_starting_a_thread(
        cx: &mut TestAppContext,
    ) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::current(ControlCommand::ThreadCreate(CreateThreadRequest {
            worktree: CreateThreadWorktree::Current,
            name: None,
            agent: "codex".to_string(),
            prompt: "   ".to_string(),
            split: None,
            focus: false,
        }));
        let mut async_cx = cx.to_async();

        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;

        assert!(matches!(response.result, ControlResult::Error(_)));
        cx.run_until_parked();
        let agent_thread_count = cx
            .update(crate::terminal_control::records)
            .into_iter()
            .filter(|record| {
                record
                    .view
                    .upgrade()
                    .is_some_and(|view| view.read_with(cx, |view, _| view.is_agent_thread()))
            })
            .count();
        assert_eq!(agent_thread_count, 1);
    }

    #[gpui::test]
    async fn dispatch_for_caller_seeds_a_new_sibling_thread(cx: &mut TestAppContext) {
        let (window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let source_pane = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .pane_for_item_id(terminal_item_id)
            })
            .expect("read source pane")
            .expect("source pane");
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::current(ControlCommand::ThreadCreate(CreateThreadRequest {
            worktree: CreateThreadWorktree::Current,
            name: None,
            agent: "codex".to_string(),
            prompt: "do the thing".to_string(),
            split: None,
            focus: false,
        }));
        let mut async_cx = cx.to_async();
        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;
        match response.result {
            ControlResult::Ok(ControlSuccess::ThreadCreated { worktree, terminal }) => {
                assert_eq!(worktree, PathBuf::from(SPAWNING_TEST_ROOT.as_str()));
                assert!(terminal.is_agent_thread);
                assert_eq!(terminal.working_directory, Some(worktree));
            }
            other => panic!("expected a successful create-thread, got {other:?}"),
        }
        wait_for_live_count(cx, SPAWNING_TEST_ROOT.as_str(), 2).await;
        window_handle
            .update(cx, |_, window, cx| {
                assert_eq!(source_pane.read(cx).items_len(), 2);
                assert_eq!(
                    source_pane
                        .read(cx)
                        .active_item()
                        .expect("caller remains active")
                        .item_id(),
                    terminal_item_id
                );
                assert!(
                    source_pane
                        .read(cx)
                        .focus_handle(cx)
                        .contains_focused(window, cx)
                );
            })
            .expect("inspect source pane");
    }

    #[gpui::test]
    async fn dispatch_for_caller_places_a_sibling_thread_in_each_split_direction(
        cx: &mut TestAppContext,
    ) {
        for direction in ["left", "right", "up", "down"] {
            let (window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
            cx.run_until_parked();
            window_handle
                .update(cx, |multi_workspace, window, cx| {
                    multi_workspace.workspace().update(cx, |workspace, cx| {
                        if workspace.panel::<TerminalPanel>(cx).is_none() {
                            let panel = cx.new(|cx| TerminalPanel::new(workspace, window, cx));
                            workspace.add_panel(panel, window, cx);
                        }
                    });
                })
                .expect("install terminal panel");
            let source_pane = window_handle
                .read_with(cx, |multi_workspace, cx| {
                    multi_workspace
                        .workspace()
                        .read(cx)
                        .pane_for_item_id(terminal_item_id)
                })
                .expect("read source pane")
                .expect("source pane");
            let store = cx.update(|cx| AgentThreadStore::global(cx));
            let request =
                ControlRequest::current(ControlCommand::ThreadCreate(CreateThreadRequest {
                    worktree: CreateThreadWorktree::Current,
                    name: None,
                    agent: "codex".to_string(),
                    prompt: format!("split {direction}"),
                    split: Some(direction.to_string()),
                    focus: false,
                }));
            let mut async_cx = cx.to_async();

            let response =
                dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;

            assert!(matches!(
                response.result,
                ControlResult::Ok(ControlSuccess::ThreadCreated { ref terminal, .. })
                    if terminal.is_agent_thread
            ));
            window_handle
                .update(cx, |multi_workspace, window, cx| {
                    assert_eq!(multi_workspace.workspace().read(cx).panes().len(), 2);
                    assert_eq!(source_pane.read(cx).items_len(), 1);
                    assert!(
                        source_pane
                            .read(cx)
                            .focus_handle(cx)
                            .contains_focused(window, cx)
                    );
                })
                .expect("inspect split placement");
        }
    }

    #[gpui::test]
    async fn dispatch_for_caller_focuses_a_sibling_thread_only_when_requested(
        cx: &mut TestAppContext,
    ) {
        let (window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let source_pane = window_handle
            .read_with(cx, |multi_workspace, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .pane_for_item_id(terminal_item_id)
            })
            .expect("read source pane")
            .expect("source pane");
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::current(ControlCommand::ThreadCreate(CreateThreadRequest {
            worktree: CreateThreadWorktree::Current,
            name: None,
            agent: "codex".to_string(),
            prompt: "focus the sibling".to_string(),
            split: None,
            focus: true,
        }));
        let mut async_cx = cx.to_async();

        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;

        let terminal_id = match response.result {
            ControlResult::Ok(ControlSuccess::ThreadCreated { terminal, .. }) => terminal.id,
            other => panic!("expected created thread, got {other:?}"),
        };
        let created_view_id = cx
            .update(crate::terminal_control::records)
            .into_iter()
            .find(|record| record.id == terminal_id)
            .and_then(|record| record.view.upgrade())
            .map(|view| view.entity_id())
            .expect("created thread view");
        window_handle
            .update(cx, |_, window, cx| {
                assert_eq!(
                    source_pane
                        .read(cx)
                        .active_item()
                        .expect("created thread is active")
                        .item_id(),
                    created_view_id
                );
                assert!(
                    source_pane
                        .read(cx)
                        .focus_handle(cx)
                        .contains_focused(window, cx)
                );
            })
            .expect("inspect focused sibling");
    }

    #[gpui::test]
    async fn dispatch_reports_not_ready_when_no_thread_is_tracked(cx: &mut TestAppContext) {
        init_test(cx);
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::current(ControlCommand::ThreadRetie(RetieThreadRequest {
            worktree: std::env::temp_dir(),
        }));
        let mut async_cx = cx.to_async();
        // No thread has ever been spawned in this test, so no PID is
        // tracked and any peer -- including this very test process -- gets
        // NotReady rather than being matched to something it isn't.
        let response = dispatch(std::process::id(), &request, &store, &mut async_cx).await;
        assert!(matches!(response.result, ControlResult::NotReady));
    }

    /// The one real end-to-end test: talks to `run_server`/`handle_connection`
    /// over an actual Unix socket (a temp path, never the real
    /// `paths::data_dir()` one `control::init` binds), covering the wire
    /// framing the `dispatch_for_caller`-level tests above don't touch --
    /// JSON encode and length framing on the client, decode on the server,
    /// real peer-credential retrieval, and the response written
    /// back. The connecting process here is the test binary itself, which
    /// (correctly) can never resolve to the live thread's own tracked PID --
    /// that thread's terminal process is a *child* of this test binary, not
    /// an ancestor of it, so this exercises the NotReady/rejection path
    /// rather than a successful match; `resolve_caller_thread`'s own tests
    /// cover the match logic with a real ancestor relationship.
    #[cfg(unix)]
    #[gpui::test]
    async fn control_server_round_trips_a_request_over_a_real_socket(cx: &mut TestAppContext) {
        let (_window_handle, _terminal_item_id) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));

        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir for the socket");
        let socket_path = temp_dir.path().join("agent-control-test.sock");
        let owns_socket = Arc::new(AtomicBool::new(false));

        let server_cx = cx.to_async();
        let _server_task = server_cx.spawn({
            let socket_path = socket_path.clone();
            let owns_socket = owns_socket.clone();
            async move |cx| {
                run_server(socket_path, owns_socket, store, cx).await.ok();
            }
        });

        for _ in 0..50 {
            if owns_socket.load(Ordering::Acquire) {
                break;
            }
            cx.run_until_parked();
            cx.executor()
                .timer(std::time::Duration::from_millis(20))
                .await;
        }
        assert!(
            owns_socket.load(Ordering::Acquire),
            "the server should have bound the socket before the deadline"
        );

        let mut stream = UnixStream::connect(&socket_path)
            .await
            .expect("failed to connect to the test socket");
        let request = ControlRequest::current(ControlCommand::ThreadRetie(RetieThreadRequest {
            worktree: std::env::temp_dir(),
        }));
        let payload = serde_json::to_vec(&request).expect("failed to encode request");
        let frame = frame_payload(&payload, MAX_REQUEST_BYTES).expect("failed to frame request");
        stream
            .write_all(&frame)
            .await
            .expect("failed to write request");
        let mut response_length = [0; FRAME_LENGTH_BYTES];
        stream
            .read_exact(&mut response_length)
            .await
            .expect("failed to read response length");
        let mut response_bytes = vec![0; u32::from_be_bytes(response_length) as usize];
        stream
            .read_exact(&mut response_bytes)
            .await
            .expect("failed to read response payload");
        let response: ControlResponse =
            serde_json::from_slice(&response_bytes).expect("failed to decode response");
        assert!(
            matches!(response.result, ControlResult::NotReady),
            "this test process is a child, not an ancestor, of the tracked terminal's process, \
             so it can never resolve -- got {response:?}"
        );
    }
}
