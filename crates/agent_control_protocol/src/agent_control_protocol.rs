//! Wire types shared between `agent_control_cli` (the `flint-agent-control`
//! binary an agent's own CLI process invokes) and `agent_threads::control`
//! (the Unix socket server inside Flint that handles the request). Kept
//! dependency-free of GPUI/terminal so the CLI binary stays small.

use std::path::PathBuf;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// `stable` | `dev` | `nightly` | `preview`. A local re-derivation of
/// `release_channel::RELEASE_CHANNEL_NAME`'s own logic, not a dependency on
/// that crate: `release_channel` itself depends on `gpui`, which would drag
/// the entire GPUI/rendering stack into `agent_control_cli`, defeating this
/// crate's whole purpose of keeping that binary small.
static RELEASE_CHANNEL_NAME: LazyLock<String> = LazyLock::new(|| {
    if cfg!(debug_assertions) {
        std::env::var("ZED_RELEASE_CHANNEL").unwrap_or_else(|_| {
            include_str!("../../flint/RELEASE_CHANNEL")
                .trim()
                .to_string()
        })
    } else {
        include_str!("../../flint/RELEASE_CHANNEL")
            .trim()
            .to_string()
    }
});

/// The control socket's path, computed identically by the server
/// (`agent_threads::control`) and the client (`agent_control_cli`) so
/// neither needs to hand the other a value at runtime -- it's a pure
/// function of the user's data directory and release channel, unaffected by
/// whatever a coding agent's own subprocess exec strips from custom
/// environment variables (the reason this crate exists at all: see
/// `AgentControlLocation`'s doc comment for the env-var failure this
/// replaced).
pub fn socket_path() -> PathBuf {
    paths::data_dir().join(format!("agent-control-{}.sock", *RELEASE_CHANNEL_NAME))
}

/// Path to the marker file at which Flint records where its own
/// `flint-agent-control` executable lives, so an agent's own CLI process can
/// discover what command to run in the first place. One location per
/// release channel; written once when Flint's control server starts (see
/// `agent_threads::control::init`) and removed on quit -- not per-thread,
/// since the executable's location is the same for every thread in one
/// Flint session.
pub fn executable_location_path() -> PathBuf {
    paths::data_dir().join(format!(
        "agent-control-{}-executable.json",
        *RELEASE_CHANNEL_NAME
    ))
}

/// Contents of the file at `executable_location_path()`.
///
/// This is the only handoff data left on disk. The control socket's path is
/// independently computable (`socket_path()`), and caller identity is
/// established by the server asking the kernel who actually connected
/// (`SO_PEERCRED` on Linux, `LOCAL_PEERPID` on macOS) rather than by a
/// client-presented secret -- so there's no per-thread token to mint,
/// deliver, or collide over. The original design used environment variables
/// for all of this, but some coding-agent CLIs run their own shell commands
/// through a subprocess exec that silently strips custom env vars (observed
/// with Codex CLI's shell execution path, independent of its documented
/// `shell_environment_policy` setting -- explicit `inherit = "all"` did not
/// fix it); a plain file read is not subject to that filtering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentControlLocation {
    pub executable: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum ControlRequest {
    RetieThread(RetieThreadRequest),
    CreateThread(CreateThreadRequest),
}

/// Re-tie the calling thread to a different worktree. `worktree` is only
/// ever used to find-or-open the destination workspace -- the committed tie
/// is derived from that workspace's own resolved worktree root, never from
/// this raw path, so `..` segments, symlinks, or case differences can't
/// produce an unmatchable tie.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetieThreadRequest {
    pub worktree: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CreateThreadWorktree {
    /// Launch the new thread in the calling thread's own worktree.
    Current,
    /// Create a new linked worktree (background, non-activating) and launch
    /// the new thread there.
    New,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateThreadRequest {
    pub worktree: CreateThreadWorktree,
    /// Branch/directory name hint for `worktree: New`; ignored for
    /// `worktree: Current`.
    pub name: Option<String>,
    pub agent: String,
    pub prompt: String,
}

/// The connecting process could not yet be matched to a registered thread --
/// either because it hasn't finished registering (the terminal spawned very
/// recently and `AgentThreadStore::register` hasn't run yet), or because it
/// genuinely isn't one Flint is tracking. These two cases are indistinguishable
/// from the server's side without extra bookkeeping this design deliberately
/// avoids, so both get `NotReady` rather than a hard error: the CLI retries
/// with bounded backoff, and a request that's never going to match simply
/// exhausts its retries and reports failure like any other unauthorized call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ControlResponse {
    Ok(ControlSuccess),
    NotReady,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ControlSuccess {
    Retied { worktree: PathBuf },
    ThreadCreated { worktree: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_control_location_round_trips_through_json() {
        let location = AgentControlLocation {
            executable: PathBuf::from("/Applications/Flint.app/Contents/MacOS/flint-agent-control"),
        };
        let json = serde_json::to_string(&location).expect("serialize");
        let decoded: AgentControlLocation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.executable, location.executable);
    }

    #[test]
    fn retie_thread_request_round_trips_through_json() {
        let request = ControlRequest::RetieThread(RetieThreadRequest {
            worktree: PathBuf::from("/repo/worktrees/feature"),
        });
        let json = serde_json::to_string(&request).expect("serialize");
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ControlRequest::RetieThread(request) => {
                assert_eq!(request.worktree, PathBuf::from("/repo/worktrees/feature"));
            }
            other => panic!("expected RetieThread, got {other:?}"),
        }
    }

    #[test]
    fn create_thread_request_round_trips_through_json() {
        let request = ControlRequest::CreateThread(CreateThreadRequest {
            worktree: CreateThreadWorktree::New,
            name: Some("feature-x".to_string()),
            agent: "codex".to_string(),
            prompt: "implement the thing".to_string(),
        });
        let json = serde_json::to_string(&request).expect("serialize");
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ControlRequest::CreateThread(request) => {
                assert_eq!(request.worktree, CreateThreadWorktree::New);
                assert_eq!(request.name.as_deref(), Some("feature-x"));
                assert_eq!(request.agent, "codex");
                assert_eq!(request.prompt, "implement the thing");
            }
            other => panic!("expected CreateThread, got {other:?}"),
        }
    }

    #[test]
    fn responses_round_trip_through_json() {
        for response in [
            ControlResponse::Ok(ControlSuccess::Retied {
                worktree: PathBuf::from("/repo/worktrees/feature"),
            }),
            ControlResponse::Ok(ControlSuccess::ThreadCreated {
                worktree: PathBuf::from("/repo/worktrees/feature"),
            }),
            ControlResponse::NotReady,
            ControlResponse::Error {
                message: "unrecognized caller".to_string(),
            },
        ] {
            let json = serde_json::to_string(&response).expect("serialize");
            let _decoded: ControlResponse = serde_json::from_str(&json).expect("deserialize");
        }
    }

    #[test]
    fn socket_path_and_executable_location_path_are_release_channel_scoped_siblings() {
        let socket = socket_path();
        let executable_location = executable_location_path();
        assert_eq!(socket.parent(), executable_location.parent());
        assert_ne!(socket, executable_location);
    }
}
