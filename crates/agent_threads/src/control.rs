//! The local-only Unix socket server backing agent-initiated worktree
//! control: a thread's own CLI process (Codex, Claude Code, etc.) invokes
//! the `flint-agent-control` binary (`agent_control_cli`), which sends one
//! JSON request over this socket per invocation.
//!
//! Caller identity is established by asking the kernel who actually
//! connected -- `LOCAL_PEERPID` on macOS, `SO_PEERCRED` on Linux -- and
//! walking that process's parent-PID ancestry (via `sysinfo`) looking for a
//! PID Flint recognizes as a live thread's own terminal process. There is
//! no client-presented secret: nothing is minted, delivered, or can go
//! stale by being overwritten. See `agent_control_protocol::AgentControlLocation`
//! for why environment variables and a per-thread token were tried first
//! and abandoned.
//!
//! Unix only: gated at the `mod control;` declaration in `agent_threads.rs`
//! and at every cross-module call site, since the feature doesn't exist on
//! other platforms for this pass.

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_control_protocol::{
    ControlRequest, ControlResponse, ControlSuccess, CreateThreadRequest, CreateThreadWorktree,
    RetieThreadRequest,
};
use anyhow::{Context as _, Result};
use collections::HashMap;
use gpui::{App, AsyncApp, Entity, EntityId};
use net::async_net::{UnixListener, UnixStream};
use settings::Settings as _;
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};
use util::ResultExt as _;
use workspace::Workspace;

use crate::agent_kind_registry;
use crate::store::{self, AgentThreadStore};

/// How far up a connecting process's parent chain to look for a match
/// before giving up. Generous relative to the shallow real-world case (the
/// CLI is typically a direct child of the tracked terminal process, or one
/// shell layer removed) without risking an unbounded walk.
const MAX_ANCESTRY_DEPTH: usize = 32;

/// Starts the accept-loop exactly once and stashes its `Task` on the
/// `AgentThreadStore` global so its lifetime is the app's, not a caller's.
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
    store.update(cx, |store, _cx| store.hold_control_server_task(task));
}

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
/// -- an explicit `--socket` override or a PATH-installed binary can still
/// reach it, just not via this file.
fn write_executable_location(executable_location_path: &std::path::Path) {
    let executable = match util::get_flint_agent_control_path() {
        Ok(executable) => executable,
        Err(error) => {
            log::warn!(
                "agent_threads: could not resolve flint-agent-control's own path, so agents \
                 won't be able to discover it via the marker file: {error:#}"
            );
            return;
        }
    };
    let location = agent_control_protocol::AgentControlLocation { executable };
    let Some(json) = serde_json::to_string_pretty(&location).log_err() else {
        return;
    };
    std::fs::write(executable_location_path, json).log_err();
}

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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn get_peer_pid(_stream: &UnixStream) -> Result<u32> {
    anyhow::bail!("peer-credential resolution is not supported on this platform")
}

/// Walks up from `peer_pid`'s own process looking for a PID present in
/// `tracked` (see `AgentThreadStore::live_terminal_pids`). `None` if no
/// tracked PID appears within `MAX_ANCESTRY_DEPTH` hops -- including the
/// common, harmless case where the ancestry walk starts before the walk
/// even has a `tracked` entry to find, i.e. the caller connected before its
/// own thread finished registering.
fn resolve_caller_thread(peer_pid: u32, tracked: &HashMap<u32, EntityId>) -> Option<EntityId> {
    if tracked.is_empty() {
        return None;
    }
    let mut system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing()),
    );
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );

    let mut current = sysinfo::Pid::from_u32(peer_pid);
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if let Some(&terminal_item_id) = tracked.get(&current.as_u32()) {
            return Some(terminal_item_id);
        }
        let parent = system.process(current)?.parent()?;
        if parent == current {
            return None;
        }
        current = parent;
    }
    None
}

async fn dispatch(
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

    let tracked = store.read_with(cx, |store, cx| store.live_terminal_pids(cx));
    let terminal_item_id = match resolve_caller_thread(peer_pid, &tracked) {
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

fn error_response(error: impl std::fmt::Display) -> ControlResponse {
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
    use std::sync::LazyLock;
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
        let mut child = smol::process::Command::new("sleep")
            .arg("5")
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn a real child process for the ancestry test");
        let child_pid = child.id();

        let tracked: HashMap<u32, EntityId> = HashMap::from_iter([(this_pid, EntityId::from(42))]);

        let resolved = resolve_caller_thread(child_pid, &tracked);
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
        assert_eq!(resolve_caller_thread(1, &tracked), None);
    }

    #[test]
    fn resolve_caller_thread_short_circuits_on_an_empty_tracked_map() {
        assert_eq!(
            resolve_caller_thread(std::process::id(), &HashMap::default()),
            None
        );
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
