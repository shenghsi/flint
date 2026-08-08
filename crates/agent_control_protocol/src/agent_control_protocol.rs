//! Wire types shared between `agent_control_cli` (the `flint-agent-control`
//! binary an agent's own CLI process invokes) and `agent_threads::control`
//! (the Unix socket server inside Flint that handles the request). Kept
//! dependency-free of GPUI/terminal so the CLI binary stays small.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Environment variable naming the Unix socket path a spawned agent thread's
/// terminal should connect to.
pub const SOCKET_ENV_VAR: &str = "FLINT_AGENT_CONTROL_SOCKET";

/// Environment variable naming the per-thread token a spawned agent thread's
/// terminal should present when connecting. The token alone resolves caller
/// identity server-side -- nothing else the client sends is trusted.
pub const TOKEN_ENV_VAR: &str = "FLINT_AGENT_CONTROL_TOKEN";

/// Environment variable naming the resolved path to the `flint-agent-control`
/// executable itself, as injected into a spawned agent thread's terminal.
pub const EXECUTABLE_ENV_VAR: &str = "FLINT_AGENT_CONTROL";

/// One request sent over the socket, wrapped with the token that identifies
/// the calling thread. The token travels alongside the request rather than
/// inside it so the server can reject an unknown/expired token before ever
/// looking at the request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlEnvelope {
    pub token: String,
    #[serde(flatten)]
    pub request: ControlRequest,
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

/// The token was recognized but its thread registration hasn't landed yet
/// (see `ControlTokenState::Reserved` on the server) -- distinct from an
/// auth failure so the CLI knows to retry with bounded backoff instead of
/// giving up immediately.
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
    fn retie_thread_envelope_round_trips_through_json() {
        let envelope = ControlEnvelope {
            token: "abc123".to_string(),
            request: ControlRequest::RetieThread(RetieThreadRequest {
                worktree: PathBuf::from("/repo/worktrees/feature"),
            }),
        };
        let json = serde_json::to_string(&envelope).expect("serialize");
        let decoded: ControlEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.token, "abc123");
        match decoded.request {
            ControlRequest::RetieThread(request) => {
                assert_eq!(request.worktree, PathBuf::from("/repo/worktrees/feature"));
            }
            other => panic!("expected RetieThread, got {other:?}"),
        }
    }

    #[test]
    fn create_thread_envelope_round_trips_through_json() {
        let envelope = ControlEnvelope {
            token: "xyz789".to_string(),
            request: ControlRequest::CreateThread(CreateThreadRequest {
                worktree: CreateThreadWorktree::New,
                name: Some("feature-x".to_string()),
                agent: "codex".to_string(),
                prompt: "implement the thing".to_string(),
            }),
        };
        let json = serde_json::to_string(&envelope).expect("serialize");
        let decoded: ControlEnvelope = serde_json::from_str(&json).expect("deserialize");
        match decoded.request {
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
                message: "unknown token".to_string(),
            },
        ] {
            let json = serde_json::to_string(&response).expect("serialize");
            let _decoded: ControlResponse = serde_json::from_str(&json).expect("deserialize");
        }
    }
}
