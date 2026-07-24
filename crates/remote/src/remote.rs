/// Advertised by `flint-remote-server` in its `RemoteStarted` message when it
/// can serve the host-owned agent thread history index over
/// `StreamAgentThreadHistory`. A client that does not see this capability falls
/// back to the legacy client-side scanner.
pub const AGENT_THREAD_HISTORY_INDEX_CAPABILITY: &str = "agent-thread-history-index-v1";

pub mod json_log;
pub mod protocol;
pub mod proxy;
pub mod remote_client;
pub mod remote_identity;
mod transport;

#[cfg(target_os = "windows")]
pub use remote_client::OpenWslPath;
pub use remote_client::{
    CommandTemplate, ConnectionIdentifier, ConnectionSharing, ConnectionState, Interactive,
    LocalPortForward, RemoteArch, RemoteClient, RemoteClientDelegate, RemoteClientEvent,
    RemoteConnection, RemoteConnectionOptions, RemoteLibc, RemoteOs, RemotePlatform,
    RemotePortForward, connect, has_active_connection,
};
pub use remote_identity::{
    RemoteConnectionIdentity, remote_connection_identity, same_remote_connection_identity,
};
pub use transport::docker::DockerConnectionOptions;
pub use transport::ssh::{SshConnectionOptions, SshPortForwardOption};
pub use transport::wsl::WslConnectionOptions;
#[cfg(target_os = "windows")]
pub use transport::wsl::wsl_path_to_windows_path;

#[cfg(any(test, feature = "test-support"))]
pub use transport::mock::{
    MockConnection, MockConnectionOptions, MockConnectionRegistry, MockDelegate,
};
