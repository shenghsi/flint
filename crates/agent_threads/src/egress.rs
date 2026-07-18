use crate::connect_proxy::{ConnectProxyLease, ConnectProxyServer};
use anyhow::Result;
use collections::HashMap;
use gpui::EntityId;
use std::sync::{Arc, Weak};

pub struct AgentEgressManager {
    sessions: async_lock::Mutex<HashMap<EntityId, Weak<AgentEgressSession>>>,
}

impl AgentEgressManager {
    pub fn new() -> Self {
        Self {
            sessions: async_lock::Mutex::new(HashMap::default()),
        }
    }

    pub async fn acquire(
        &self,
        remote_client_id: EntityId,
        connection: Arc<dyn remote::RemoteConnection>,
        allowed_hosts: &'static [&'static str],
    ) -> Result<AgentEgressLease> {
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.strong_count() > 0);
        let session = if let Some(session) = sessions.get(&remote_client_id).and_then(Weak::upgrade)
        {
            session
        } else {
            let proxy = ConnectProxyServer::start().await?;
            let forward = connection
                .open_reverse_port_forward(proxy.local_port())
                .await?;
            let session = Arc::new(AgentEgressSession {
                remote_port: forward.remote_port(),
                proxy,
                _forward: forward,
            });
            sessions.insert(remote_client_id, Arc::downgrade(&session));
            session
        };
        let capability = session.proxy.acquire(allowed_hosts, session.remote_port);
        Ok(AgentEgressLease {
            capability,
            _session: session,
        })
    }
}

struct AgentEgressSession {
    remote_port: u16,
    proxy: ConnectProxyServer,
    _forward: remote::RemotePortForward,
}

pub struct AgentEgressLease {
    capability: ConnectProxyLease,
    _session: Arc<AgentEgressSession>,
}

impl AgentEgressLease {
    pub fn proxy_url(&self) -> String {
        self.capability.proxy_url()
    }
}
