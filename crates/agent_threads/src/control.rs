//! The local-only server backing agent-initiated worktree
//! control: a thread's own CLI process (Codex, Claude Code, etc.) invokes
//! the `flint-agent-control` binary (`agent_control_cli`), which sends one
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

use agent_control_protocol::{
    ControlRequest, ControlResponse, ControlSuccess, CreateThreadRequest, CreateThreadWorktree,
    RetieThreadRequest,
};
#[cfg(unix)]
use anyhow::{Context as _, Result};
use collections::HashMap;
#[cfg(unix)]
use gpui::Task;
use gpui::{App, AsyncApp, Entity, EntityId};
#[cfg(unix)]
use net::async_net::{UnixListener, UnixStream};
use settings::Settings as _;
#[cfg(unix)]
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};
use util::ResultExt as _;
use workspace::Workspace;

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
    let owns_socket = Arc::new(AtomicBool::new(false));
    let task = cx.spawn({
        let socket_path = socket_path.clone();
        let executable_location_path = executable_location_path.clone();
        let owns_socket = owns_socket.clone();
        async move |cx| {
            if let Err(error) = run_server(
                socket_path,
                executable_location_path,
                owns_socket,
                store,
                cx,
            )
            .await
            {
                log::error!("agent_threads: agent control server did not start: {error:#}");
            }
        }
    });
    cx.on_app_quit(move |_cx| {
        let socket_path = socket_path.clone();
        let executable_location_path = executable_location_path.clone();
        let owns_socket = owns_socket.clone();
        async move {
            // Only remove these if this instance actually bound the socket --
            // an instance that detected another live owner and disabled
            // itself must never unlink files it doesn't own.
            if owns_socket.load(Ordering::Acquire) {
                std::fs::remove_file(&socket_path).ok();
                std::fs::remove_file(&executable_location_path).ok();
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
    executable_location_path: PathBuf,
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

    write_executable_location(&executable_location_path);

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

/// Records where this Flint instance's own `flint-agent-control` executable
/// lives, so an agent's CLI process can discover what command to run in the
/// first place. Best-effort: if the executable can't be resolved (e.g. a
/// dev build with no bundled binary alongside it), the server still starts
/// -- an explicit platform endpoint override or a PATH-installed binary can
/// still reach it, just not via this file.
pub(crate) fn write_executable_location(executable_location_path: &std::path::Path) -> bool {
    let executable = match util::get_flint_agent_control_path() {
        Ok(executable) => executable,
        Err(error) => {
            log::warn!(
                "agent_threads: could not resolve flint-agent-control's own path, so agents \
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
fn replace_marker(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
fn replace_marker(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
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
    let mut request_bytes = Vec::new();
    stream
        .read_to_end(&mut request_bytes)
        .await
        .context("failed to read request")?;

    let response = match serde_json::from_slice::<ControlRequest>(&request_bytes) {
        Ok(request) => match get_peer_pid(&stream) {
            Ok(peer_pid) => dispatch(peer_pid, &request, &store, cx).await,
            Err(error) => error_response(format_args!(
                "could not determine caller identity: {error:#}"
            )),
        },
        Err(error) => ControlResponse::Error {
            message: format!("malformed request: {error}"),
        },
    };

    let response_bytes = serde_json::to_vec(&response).context("failed to encode response")?;
    stream
        .write_all(&response_bytes)
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
    let agent_control_enabled =
        cx.update(|cx| crate::AgentThreadSettings::get_global(cx).agent_control);
    if !agent_control_enabled {
        return error_response("agent_threads.agent_control is disabled");
    }

    let (tracked_pids, tracked_worktrees) = store.read_with(cx, |store, cx| {
        (
            store.live_terminal_pids(cx),
            store.live_terminal_worktree_roots(),
        )
    });
    let terminal_item_id = match resolve_caller_thread(peer_pid, &tracked_pids, &tracked_worktrees)
    {
        Some(terminal_item_id) => terminal_item_id,
        None => return ControlResponse::NotReady,
    };

    dispatch_for_caller(terminal_item_id, request, store, cx).await
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
    match request {
        ControlRequest::RetieThread(request) => {
            handle_retie_thread(terminal_item_id, request, cx).await
        }
        ControlRequest::CreateThread(request) => {
            handle_create_thread(terminal_item_id, request, store, cx).await
        }
    }
}

pub(crate) fn error_response(error: impl std::fmt::Display) -> ControlResponse {
    ControlResponse::Error {
        message: error.to_string(),
    }
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
            ControlResponse::Ok(ControlSuccess::Retied { worktree: tie.root })
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

    let (source_workspace, _terminal_view) = match store.read_with(cx, |store, _| {
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

    match request.worktree {
        CreateThreadWorktree::Current => {
            match window_handle.update(cx, |_, window, cx| {
                source_workspace.update(cx, |workspace, cx| {
                    launch_seeded_and_respond(workspace, &kind, &request.prompt, window, cx)
                })
            }) {
                Ok(response) => response,
                Err(error) => error_response(error),
            }
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
            match window_handle.update(cx, |_, window, cx| {
                created.workspace.update(cx, |workspace, cx| {
                    launch_seeded_and_respond(workspace, &kind, &request.prompt, window, cx)
                })
            }) {
                Ok(response) => response,
                Err(error) => error_response(error),
            }
        }
    }
}

fn launch_seeded_and_respond(
    workspace: &mut Workspace,
    kind: &crate::AgentKindDefinition,
    prompt: &str,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<Workspace>,
) -> ControlResponse {
    let seeded = store::launch_seeded_thread(workspace, kind, prompt, window, cx);
    if !seeded {
        return error_response(format_args!(
            "{} does not support a seeded initial prompt",
            kind.label
        ));
    }
    match crate::history::project_worktree_roots(workspace.project().read(cx), cx)
        .into_iter()
        .next()
    {
        Some(worktree) => ControlResponse::Ok(ControlSuccess::ThreadCreated { worktree }),
        None => error_response("could not resolve the new thread's worktree"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::Fs;
    use gpui::{EntityId, TestAppContext, WindowHandle};
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
        let request = ControlRequest::RetieThread(RetieThreadRequest {
            worktree: PathBuf::from("/definitely/does/not/exist/anywhere"),
        });
        let mut async_cx = cx.to_async();
        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;
        assert!(matches!(response, ControlResponse::Error { .. }));
    }

    #[gpui::test]
    async fn dispatch_for_caller_moves_the_terminal_on_retie(cx: &mut TestAppContext) {
        let (window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let root_b = std::env::temp_dir().join("agent_control_dispatch_retie_test");
        std::fs::create_dir_all(&root_b).expect("failed to create the retie target directory");

        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::RetieThread(RetieThreadRequest {
            worktree: root_b.clone(),
        });
        let mut async_cx = cx.to_async();
        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;
        match response {
            ControlResponse::Ok(ControlSuccess::Retied { worktree }) => {
                assert_eq!(worktree, root_b);
            }
            other => panic!("expected a successful retie, got {other:?}"),
        }
        cx.run_until_parked();
        let _ = window_handle;
    }

    #[gpui::test]
    async fn dispatch_for_caller_rejects_an_unknown_agent(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::CreateThread(CreateThreadRequest {
            worktree: CreateThreadWorktree::Current,
            name: None,
            agent: "not-a-real-agent".to_string(),
            prompt: "do the thing".to_string(),
        });
        let mut async_cx = cx.to_async();
        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;
        assert!(matches!(response, ControlResponse::Error { .. }));
    }

    #[gpui::test]
    async fn dispatch_for_caller_seeds_a_new_sibling_thread(cx: &mut TestAppContext) {
        let (_window_handle, terminal_item_id) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::CreateThread(CreateThreadRequest {
            worktree: CreateThreadWorktree::Current,
            name: None,
            agent: "codex".to_string(),
            prompt: "do the thing".to_string(),
        });
        let mut async_cx = cx.to_async();
        let response = dispatch_for_caller(terminal_item_id, &request, &store, &mut async_cx).await;
        match response {
            ControlResponse::Ok(ControlSuccess::ThreadCreated { worktree }) => {
                assert_eq!(worktree, PathBuf::from(SPAWNING_TEST_ROOT.as_str()));
            }
            other => panic!("expected a successful create-thread, got {other:?}"),
        }
        wait_for_live_count(cx, SPAWNING_TEST_ROOT.as_str(), 2).await;
    }

    #[gpui::test]
    async fn dispatch_reports_not_ready_when_no_thread_is_tracked(cx: &mut TestAppContext) {
        init_test(cx);
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let request = ControlRequest::RetieThread(RetieThreadRequest {
            worktree: std::env::temp_dir(),
        });
        let mut async_cx = cx.to_async();
        // No thread has ever been spawned in this test, so no PID is
        // tracked and any peer -- including this very test process -- gets
        // NotReady rather than being matched to something it isn't.
        let response = dispatch(std::process::id(), &request, &store, &mut async_cx).await;
        assert!(matches!(response, ControlResponse::NotReady));
    }

    /// The one real end-to-end test: talks to `run_server`/`handle_connection`
    /// over an actual Unix socket (a temp path, never the real
    /// `paths::data_dir()` one `control::init` binds), covering the wire
    /// framing the `dispatch_for_caller`-level tests above don't touch --
    /// JSON encode on the client, `read_to_end`-until-EOF plus decode on the
    /// server, real peer-credential retrieval, and the response written
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
        let executable_location_path = temp_dir.path().join("agent-control-test-executable.json");
        let owns_socket = Arc::new(AtomicBool::new(false));

        let server_cx = cx.to_async();
        let _server_task = server_cx.spawn({
            let socket_path = socket_path.clone();
            let owns_socket = owns_socket.clone();
            async move |cx| {
                run_server(
                    socket_path,
                    executable_location_path,
                    owns_socket,
                    store,
                    cx,
                )
                .await
                .ok();
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
        let request = ControlRequest::RetieThread(RetieThreadRequest {
            worktree: std::env::temp_dir(),
        });
        let payload = serde_json::to_vec(&request).expect("failed to encode request");
        stream
            .write_all(&payload)
            .await
            .expect("failed to write request");
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("failed to shut down the write half");
        let mut response_bytes = Vec::new();
        stream
            .read_to_end(&mut response_bytes)
            .await
            .expect("failed to read response");
        let response: ControlResponse =
            serde_json::from_slice(&response_bytes).expect("failed to decode response");
        assert!(
            matches!(response, ControlResponse::NotReady),
            "this test process is a child, not an ancestor, of the tracked terminal's process, \
             so it can never resolve -- got {response:?}"
        );
    }
}
