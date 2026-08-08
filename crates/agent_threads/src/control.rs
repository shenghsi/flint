//! The local-only Unix socket server backing agent-initiated worktree
//! control: a thread's own CLI process (Codex, Claude Code, etc.) invokes
//! the `flint-agent-control` binary (`agent_control_cli`), which sends one
//! JSON request over this socket per invocation. See the design doc's
//! "Stage 2 -- Agent-initiated worktree creation/tying" section for the
//! full rationale, especially the socket ownership/cleanup and token
//! lifecycle requirements this module implements.
//!
//! Unix only: gated at the `mod control;` declaration in `agent_threads.rs`
//! and at every cross-module call site, since the feature doesn't exist on
//! other platforms for this pass.

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_control_protocol::{
    ControlEnvelope, ControlRequest, ControlResponse, ControlSuccess, CreateThreadRequest,
    CreateThreadWorktree, RetieThreadRequest,
};
use anyhow::{Context as _, Result};
use gpui::{App, AsyncApp, Entity, EntityId};
use net::async_net::{UnixListener, UnixStream};
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};
use workspace::Workspace;

use crate::agent_kind_registry;
use crate::store::{self, AgentThreadStore, ControlTokenState};

pub(crate) fn socket_path() -> PathBuf {
    paths::data_dir().join(format!(
        "agent-control-{}.sock",
        *release_channel::RELEASE_CHANNEL_NAME
    ))
}

/// Starts the accept-loop exactly once and stashes its `Task` on the
/// `AgentThreadStore` global so its lifetime is the app's, not a caller's.
/// Safe to call unconditionally at crate init: whether any thread actually
/// gets a token is gated per-spawn by `agent_threads.agent_control` and the
/// local/Unix checks in `store::spawn_thread_task_inner`, so an idle server
/// with no live tokens is harmless.
pub(crate) fn init(cx: &mut App) {
    let store = AgentThreadStore::global(cx);
    let socket_path = socket_path();
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
            // Only remove the socket if this instance actually bound it --
            // an instance that detected another live owner and disabled
            // itself must never unlink a socket it doesn't own.
            if owns_socket.load(Ordering::Acquire) {
                std::fs::remove_file(&socket_path).ok();
            }
        }
    })
    .detach();
    let store = AgentThreadStore::global(cx);
    store.update(cx, |store, _cx| store.hold_control_server_task(task));
}

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

    let response = match serde_json::from_slice::<ControlEnvelope>(&request_bytes) {
        Ok(envelope) => dispatch(&envelope, &store, cx).await,
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

async fn dispatch(
    envelope: &ControlEnvelope,
    store: &Entity<AgentThreadStore>,
    cx: &mut AsyncApp,
) -> ControlResponse {
    let resolution = store.read_with(cx, |store, _| store.control_token_state(&envelope.token));
    let terminal_item_id = match resolution {
        None => {
            return ControlResponse::Error {
                message: "unknown or expired control token".to_string(),
            };
        }
        Some(ControlTokenState::Reserved) => return ControlResponse::NotReady,
        Some(ControlTokenState::Bound(terminal_item_id)) => terminal_item_id,
    };

    match &envelope.request {
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
    /// thread and returns its real, store-minted control token -- the
    /// fixture every dispatch-level test below builds on.
    async fn spawn_live_codex_thread(
        cx: &mut TestAppContext,
    ) -> (WindowHandle<MultiWorkspace>, EntityId, String) {
        cx.executor().allow_parking();
        init_test(cx);
        let root = SPAWNING_TEST_ROOT.as_str();
        configure_echo_threads(cx, root);
        let window_handle = init_workspace(cx, root).await;

        launch_codex_thread(&window_handle, cx);
        wait_for_live_count(cx, root, 1).await;

        let terminal_item_id = terminal_views(&window_handle, cx)[0].entity_id();
        let token = cx.update(|cx| {
            AgentThreadStore::global(cx).update(cx, |store, _| {
                store.bind_control_token_for_test(terminal_item_id)
            })
        });
        (window_handle, terminal_item_id, token)
    }

    #[gpui::test]
    async fn dispatch_rejects_an_unknown_token(cx: &mut TestAppContext) {
        init_test(cx);
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let envelope = ControlEnvelope {
            token: "not-a-real-token".to_string(),
            request: ControlRequest::RetieThread(RetieThreadRequest {
                worktree: std::env::temp_dir(),
            }),
        };
        let mut async_cx = cx.to_async();
        let response = dispatch(&envelope, &store, &mut async_cx).await;
        assert!(matches!(response, ControlResponse::Error { .. }));
    }

    #[gpui::test]
    async fn dispatch_reports_not_ready_for_a_reserved_token(cx: &mut TestAppContext) {
        init_test(cx);
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let token = store.update(cx, |store, _| store.reserve_control_token());
        let envelope = ControlEnvelope {
            token,
            request: ControlRequest::RetieThread(RetieThreadRequest {
                worktree: std::env::temp_dir(),
            }),
        };
        let mut async_cx = cx.to_async();
        let response = dispatch(&envelope, &store, &mut async_cx).await;
        assert!(matches!(response, ControlResponse::NotReady));
    }

    #[gpui::test]
    async fn handle_retie_thread_rejects_a_nonexistent_directory(cx: &mut TestAppContext) {
        let (_window_handle, _terminal_item_id, token) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let envelope = ControlEnvelope {
            token,
            request: ControlRequest::RetieThread(RetieThreadRequest {
                worktree: PathBuf::from("/definitely/does/not/exist/anywhere"),
            }),
        };
        let mut async_cx = cx.to_async();
        let response = dispatch(&envelope, &store, &mut async_cx).await;
        assert!(matches!(response, ControlResponse::Error { .. }));
    }

    #[gpui::test]
    async fn handle_retie_thread_moves_the_terminal_via_dispatch(cx: &mut TestAppContext) {
        let (window_handle, terminal_item_id, token) = spawn_live_codex_thread(cx).await;
        let root_b = std::env::temp_dir().join("agent_control_dispatch_retie_test");
        std::fs::create_dir_all(&root_b).expect("failed to create the retie target directory");

        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let envelope = ControlEnvelope {
            token,
            request: ControlRequest::RetieThread(RetieThreadRequest {
                worktree: root_b.clone(),
            }),
        };
        let mut async_cx = cx.to_async();
        let response = dispatch(&envelope, &store, &mut async_cx).await;
        match response {
            ControlResponse::Ok(ControlSuccess::Retied { worktree }) => {
                assert_eq!(worktree, root_b);
            }
            other => panic!("expected a successful retie, got {other:?}"),
        }
        cx.run_until_parked();
        let _ = (window_handle, terminal_item_id);
    }

    #[gpui::test]
    async fn handle_create_thread_rejects_an_unknown_agent(cx: &mut TestAppContext) {
        let (_window_handle, _terminal_item_id, token) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let envelope = ControlEnvelope {
            token,
            request: ControlRequest::CreateThread(CreateThreadRequest {
                worktree: CreateThreadWorktree::Current,
                name: None,
                agent: "not-a-real-agent".to_string(),
                prompt: "do the thing".to_string(),
            }),
        };
        let mut async_cx = cx.to_async();
        let response = dispatch(&envelope, &store, &mut async_cx).await;
        assert!(matches!(response, ControlResponse::Error { .. }));
    }

    #[gpui::test]
    async fn handle_create_thread_current_seeds_a_new_sibling_thread(cx: &mut TestAppContext) {
        let (_window_handle, _terminal_item_id, token) = spawn_live_codex_thread(cx).await;
        let store = cx.update(|cx| AgentThreadStore::global(cx));
        let envelope = ControlEnvelope {
            token,
            request: ControlRequest::CreateThread(CreateThreadRequest {
                worktree: CreateThreadWorktree::Current,
                name: None,
                agent: "codex".to_string(),
                prompt: "do the thing".to_string(),
            }),
        };
        let mut async_cx = cx.to_async();
        let response = dispatch(&envelope, &store, &mut async_cx).await;
        match response {
            ControlResponse::Ok(ControlSuccess::ThreadCreated { worktree }) => {
                assert_eq!(worktree, PathBuf::from(SPAWNING_TEST_ROOT.as_str()));
            }
            other => panic!("expected a successful create-thread, got {other:?}"),
        }
        wait_for_live_count(cx, SPAWNING_TEST_ROOT.as_str(), 2).await;
    }

    /// The one real end-to-end test: talks to `run_server`/`handle_connection`
    /// over an actual Unix socket (a temp path, never the real
    /// `paths::data_dir()` one `control::init` binds), covering the wire
    /// framing the dispatch-level tests above don't touch -- JSON encode on
    /// the client, `read_to_end`-until-EOF plus decode on the server, and
    /// the response written back. Also covers token rejection for an
    /// unknown token over the real socket, not just via direct `dispatch`.
    #[gpui::test]
    async fn control_server_round_trips_requests_over_a_real_socket(cx: &mut TestAppContext) {
        let (_window_handle, _terminal_item_id, token) = spawn_live_codex_thread(cx).await;
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

        async fn round_trip(
            socket_path: &std::path::Path,
            envelope: &ControlEnvelope,
        ) -> ControlResponse {
            let mut stream = UnixStream::connect(socket_path)
                .await
                .expect("failed to connect to the test socket");
            let payload = serde_json::to_vec(envelope).expect("failed to encode request");
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
            serde_json::from_slice(&response_bytes).expect("failed to decode response")
        }

        let retie_response = round_trip(
            &socket_path,
            &ControlEnvelope {
                token: token.clone(),
                request: ControlRequest::RetieThread(RetieThreadRequest {
                    worktree: std::env::temp_dir(),
                }),
            },
        )
        .await;
        cx.run_until_parked();
        assert!(
            matches!(
                retie_response,
                ControlResponse::Ok(ControlSuccess::Retied { .. })
            ),
            "expected a successful retie over the real socket, got {retie_response:?}"
        );

        let unknown_token_response = round_trip(
            &socket_path,
            &ControlEnvelope {
                token: "an-unknown-token".to_string(),
                request: ControlRequest::RetieThread(RetieThreadRequest {
                    worktree: std::env::temp_dir(),
                }),
            },
        )
        .await;
        assert!(
            matches!(unknown_token_response, ControlResponse::Error { .. }),
            "expected an unknown token to be rejected over the real socket, got {unknown_token_response:?}"
        );
    }
}
