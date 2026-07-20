use anyhow::{Context as _, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use collections::HashMap;
use futures::{AsyncReadExt as _, AsyncWriteExt as _, FutureExt as _};
use gpui::BackgroundExecutor;
use parking_lot::Mutex;
use smol::net::{TcpListener, TcpStream};
use std::{
    fmt,
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADER_COUNT: usize = 64;

#[derive(Clone)]
pub struct ProxyCapability(Arc<str>);

impl ProxyCapability {
    fn generate() -> Self {
        Self(Arc::from(uuid::Uuid::new_v4().simple().to_string()))
    }

    fn authorization(&self) -> String {
        format!("Basic {}", STANDARD.encode(format!("flint:{}", self.0)))
    }
}

impl fmt::Debug for ProxyCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProxyCapability([REDACTED])")
    }
}

impl fmt::Display for ProxyCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

struct CapabilityPolicy {
    allowed_hosts: &'static [&'static str],
    shutdown: async_channel::Receiver<()>,
}

type CapabilityRegistry = Arc<Mutex<HashMap<String, CapabilityPolicy>>>;

pub struct ConnectProxyServer {
    local_port: u16,
    capabilities: CapabilityRegistry,
    shutdown: async_channel::Sender<()>,
}

impl ConnectProxyServer {
    pub async fn start(executor: BackgroundExecutor) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let local_port = listener.local_addr()?.port();
        let capabilities = Arc::new(Mutex::new(HashMap::default()));
        let (shutdown, shutdown_receiver) = async_channel::bounded(1);
        smol::spawn(run_listener(
            listener,
            capabilities.clone(),
            shutdown_receiver,
            executor,
        ))
        .detach();
        Ok(Self {
            local_port,
            capabilities,
            shutdown,
        })
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn acquire(
        &self,
        allowed_hosts: &'static [&'static str],
        remote_port: u16,
    ) -> ConnectProxyLease {
        let capability = ProxyCapability::generate();
        let authorization = capability.authorization();
        let (shutdown, shutdown_receiver) = async_channel::bounded(1);
        self.capabilities.lock().insert(
            authorization.clone(),
            CapabilityPolicy {
                allowed_hosts,
                shutdown: shutdown_receiver,
            },
        );
        ConnectProxyLease {
            capability,
            authorization,
            remote_port,
            capabilities: self.capabilities.clone(),
            shutdown,
        }
    }
}

impl Drop for ConnectProxyServer {
    fn drop(&mut self) {
        self.shutdown.close();
    }
}

pub struct ConnectProxyLease {
    capability: ProxyCapability,
    authorization: String,
    remote_port: u16,
    capabilities: CapabilityRegistry,
    shutdown: async_channel::Sender<()>,
}

impl ConnectProxyLease {
    pub fn proxy_url(&self) -> String {
        format!(
            "http://flint:{}@127.0.0.1:{}",
            self.capability.0, self.remote_port
        )
    }

    pub fn close(&self) {
        self.capabilities.lock().remove(&self.authorization);
        self.shutdown.close();
    }
}

impl Drop for ConnectProxyLease {
    fn drop(&mut self) {
        self.close();
    }
}

struct ConnectRequest {
    host: String,
    port: u16,
    authorization: String,
}

async fn run_listener(
    listener: TcpListener,
    capabilities: CapabilityRegistry,
    shutdown: async_channel::Receiver<()>,
    executor: BackgroundExecutor,
) {
    loop {
        let accept = listener.accept().fuse();
        let cancelled = shutdown.recv().fuse();
        futures::pin_mut!(accept, cancelled);
        futures::select! {
            connection = accept => match connection {
                Ok((stream, _)) => {
                    let capabilities = capabilities.clone();
                    let shutdown = shutdown.clone();
                    let executor = executor.clone();
                    smol::spawn(async move {
                        if let Err(error) = handle_connection(stream, &capabilities, shutdown, &executor).await {
                            log::debug!("agent CONNECT proxy denied or ended a connection: {error:#}");
                        }
                    }).detach();
                }
                Err(error) => {
                    log::warn!("agent CONNECT proxy stopped accepting connections: {error}");
                    return;
                }
            },
            _ = cancelled => return,
        }
    }
}

async fn handle_connection(
    mut client: TcpStream,
    capabilities: &CapabilityRegistry,
    server_shutdown: async_channel::Receiver<()>,
    executor: &BackgroundExecutor,
) -> Result<()> {
    let request = {
        let read = read_request(&mut client).fuse();
        let timeout = executor.timer(std::time::Duration::from_secs(10)).fuse();
        futures::pin_mut!(read, timeout);
        futures::select! {
            request = read => request,
            _ = timeout => Err(anyhow::anyhow!("proxy handshake timed out")),
        }
    };
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            client
                .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n")
                .await?;
            return Err(error);
        }
    };
    let capability_shutdown = authorize_request(&request, capabilities).map_err(|error| {
        log::debug!("agent CONNECT proxy rejected a request: {error:#}");
        error
    });
    let capability_shutdown = match capability_shutdown {
        Ok(shutdown) => shutdown,
        Err(error) => {
            client
                .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n")
                .await?;
            return Err(error);
        }
    };

    let upstream = TcpStream::connect((request.host.as_str(), request.port))
        .await
        .with_context(|| format!("failed to connect approved host {}", request.host))?;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;

    let client_to_upstream = copy_and_close(client.clone(), upstream.clone()).fuse();
    let upstream_to_client = copy_and_close(upstream, client).fuse();
    let relay = futures::future::try_join(client_to_upstream, upstream_to_client).fuse();
    let capability_cancelled = capability_shutdown.recv().fuse();
    let server_cancelled = server_shutdown.recv().fuse();
    futures::pin_mut!(relay, capability_cancelled, server_cancelled);
    futures::select! {
        result = relay => { result?; }
        _ = capability_cancelled => {}
        _ = server_cancelled => {}
    }
    Ok(())
}

async fn copy_and_close(mut reader: TcpStream, mut writer: TcpStream) -> Result<()> {
    futures::io::copy(&mut reader, &mut writer).await?;
    writer.close().await?;
    Ok(())
}

async fn read_request(client: &mut TcpStream) -> Result<ConnectRequest> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !bytes.ends_with(b"\r\n\r\n") {
        let read = client.read(&mut buffer).await?;
        if read == 0 {
            anyhow::bail!("proxy request ended before its headers");
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_HEADER_BYTES {
            anyhow::bail!("proxy request headers exceeded the size limit");
        }
    }
    let headers = std::str::from_utf8(&bytes).context("proxy request headers were not UTF-8")?;
    parse_request(headers)
}

fn parse_request(headers: &str) -> Result<ConnectRequest> {
    let mut lines = headers
        .strip_suffix("\r\n\r\n")
        .context("incomplete proxy headers")?
        .split("\r\n");
    let request_line = lines.next().context("missing proxy request line")?;
    let mut request_fields = request_line.split(' ');
    if request_fields.next() != Some("CONNECT") || request_fields.clone().count() != 2 {
        anyhow::bail!("proxy accepts CONNECT only");
    }
    let authority = request_fields.next().context("missing CONNECT authority")?;
    if request_fields.next() != Some("HTTP/1.1") {
        anyhow::bail!("proxy requires HTTP/1.1");
    }
    if !authority.is_ascii() || authority.contains('@') || authority.ends_with('.') {
        anyhow::bail!("invalid CONNECT authority");
    }
    let (host, port) = authority
        .split_once(':')
        .context("CONNECT authority must include a port")?;
    if host.is_empty() || host.parse::<IpAddr>().is_ok() || port != "443" {
        anyhow::bail!("CONNECT destination is not permitted");
    }
    if host.bytes().any(|byte| byte.is_ascii_uppercase()) {
        anyhow::bail!("CONNECT host is not permitted");
    }

    let mut authorization = None;
    let mut count = 0;
    for line in lines {
        count += 1;
        if count > MAX_HEADER_COUNT {
            anyhow::bail!("proxy request has too many headers");
        }
        let (name, value) = line.split_once(':').context("malformed proxy header")?;
        if name.eq_ignore_ascii_case("proxy-authorization")
            && authorization.replace(value.trim().to_string()).is_some()
        {
            anyhow::bail!("duplicate proxy authorization");
        }
    }
    Ok(ConnectRequest {
        host: host.to_string(),
        port: 443,
        authorization: authorization.context("missing proxy authorization")?,
    })
}

fn authorize_request(
    request: &ConnectRequest,
    capabilities: &CapabilityRegistry,
) -> Result<async_channel::Receiver<()>> {
    let capabilities = capabilities.lock();
    let policy = capabilities
        .get(&request.authorization)
        .context("proxy authorization was rejected")?;
    if !policy.allowed_hosts.contains(&request.host.as_str()) {
        anyhow::bail!("CONNECT host is not permitted");
    }
    Ok(policy.shutdown.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::net::SocketAddr;

    fn registry() -> CapabilityRegistry {
        let (_, shutdown) = async_channel::bounded(1);
        Arc::new(Mutex::new(HashMap::from_iter([(
            "Basic expected".to_string(),
            CapabilityPolicy {
                allowed_hosts: &["api.example.com"],
                shutdown,
            },
        )])))
    }

    fn parse_and_authorize(headers: &str) -> Result<ConnectRequest> {
        let request = parse_request(headers)?;
        authorize_request(&request, &registry())?;
        Ok(request)
    }

    #[test]
    fn accepts_only_authenticated_exact_host_connects() {
        let request = parse_and_authorize(
            "CONNECT api.example.com:443 HTTP/1.1\r\nProxy-Authorization: Basic expected\r\n\r\n",
        )
        .expect("approved CONNECT should parse");
        assert_eq!(request.host, "api.example.com");

        for denied in [
            "GET https://api.example.com/ HTTP/1.1\r\nProxy-Authorization: Basic expected\r\n\r\n",
            "CONNECT api.example.com.attacker.test:443 HTTP/1.1\r\nProxy-Authorization: Basic expected\r\n\r\n",
            "CONNECT API.EXAMPLE.COM:443 HTTP/1.1\r\nProxy-Authorization: Basic expected\r\n\r\n",
            "CONNECT api.example.com.:443 HTTP/1.1\r\nProxy-Authorization: Basic expected\r\n\r\n",
            "CONNECT 127.0.0.1:443 HTTP/1.1\r\nProxy-Authorization: Basic expected\r\n\r\n",
            "CONNECT api.example.com:80 HTTP/1.1\r\nProxy-Authorization: Basic expected\r\n\r\n",
            "CONNECT api.example.com:443 HTTP/1.1\r\nProxy-Authorization: Basic wrong\r\n\r\n",
        ] {
            assert!(parse_and_authorize(denied).is_err(), "accepted {denied:?}");
        }
    }

    #[test]
    fn capability_never_formats_its_secret() {
        let capability = ProxyCapability(Arc::from("top-secret"));
        assert_eq!(capability.to_string(), "[REDACTED]");
        assert_eq!(format!("{capability:?}"), "ProxyCapability([REDACTED])");
    }

    #[gpui::test]
    async fn listener_binds_only_to_ipv4_loopback(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let proxy = ConnectProxyServer::start(cx.background_executor.clone())
            .await
            .expect("proxy should bind");
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), proxy.local_port());
        TcpStream::connect(address)
            .await
            .expect("loopback listener should accept connections");
    }

    #[gpui::test]
    async fn leases_have_distinct_capabilities_and_revoke_independently(cx: &mut TestAppContext) {
        let proxy = ConnectProxyServer::start(cx.background_executor.clone())
            .await
            .expect("proxy should bind");
        let first = proxy.acquire(&["api.example.com"], 41001);
        let second = proxy.acquire(&["auth.example.com"], 41001);
        assert_ne!(first.authorization, second.authorization);
        assert_eq!(proxy.capabilities.lock().len(), 2);

        first.close();

        assert_eq!(proxy.capabilities.lock().len(), 1);
        assert!(
            proxy
                .capabilities
                .lock()
                .contains_key(&second.authorization)
        );
    }
}
