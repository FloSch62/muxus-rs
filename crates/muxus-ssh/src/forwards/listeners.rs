//! The three kinds of forward on one connection: local (-L) and dynamic (-D) listeners on
//! 127.0.0.1, and remote (-R) listeners the server opens for us.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::Channel;
use russh::client::Msg;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::AbortHandle;

use super::socks::{self, SocksRequest};
use crate::Problem;
use crate::connection::ManagedConnection;
use crate::connection::transport::ServerChannelSink;

/// All listeners bind 127.0.0.1 only — Muxus is a local single-user tool.
const LOOPBACK: &str = "127.0.0.1";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A forwarding rule as one bind brings it up.
#[derive(Debug, Clone)]
pub(crate) struct Rule {
    /// The forward id, for logs.
    pub id: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

async fn listen(port: u16) -> Result<TcpListener, Problem> {
    TcpListener::bind((LOOPBACK, port)).await.map_err(|err| {
        Problem::bad_request(format!("could not listen on {LOOPBACK}:{port}: {err}"))
    })
}

/// Accepts until aborted. Dropping the task closes the listener; connections already accepted
/// keep running until either side ends them, as with Node's `server.close()`.
fn serve<F, Fut>(listener: TcpListener, rule: Rule, mut on_socket: F) -> AbortHandle
where
    F: FnMut(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((socket, _)) => {
                    let _ = socket.set_nodelay(true);
                    tokio::spawn(on_socket(socket));
                }
                Err(err) => {
                    // Out of descriptors and the like: wait a little instead of spinning.
                    tracing::warn!(
                        %err, forward = %rule.id,
                        "forward listener could not accept a connection"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    })
    .abort_handle()
}

/// -L: listen locally, open a direct-tcpip channel per client.
pub(crate) async fn start_local(
    conn: Arc<ManagedConnection>,
    rule: Rule,
) -> Result<AbortHandle, Problem> {
    let listener = listen(rule.bind_port).await?;
    let target = rule.clone();
    Ok(serve(listener, rule, move |mut socket| {
        let conn = conn.clone();
        let target = target.clone();
        async move {
            let (address, port) = match socket.local_addr() {
                Ok(addr) => (addr.ip().to_string(), u32::from(addr.port())),
                Err(_) => (LOOPBACK.to_string(), 0),
            };
            match conn
                .open_direct_tcpip(
                    &target.target_host,
                    u32::from(target.target_port),
                    &address,
                    port,
                )
                .await
            {
                Ok(channel) => {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                }
                Err(err) => {
                    tracing::warn!(%err, forward = %target.id, "forwardOut failed");
                }
            }
        }
    }))
}

/// -D: minimal SOCKS5 (no auth, CONNECT only) tunneling through the connection.
pub(crate) async fn start_dynamic(
    conn: Arc<ManagedConnection>,
    rule: Rule,
) -> Result<AbortHandle, Problem> {
    let listener = listen(rule.bind_port).await?;
    Ok(serve(listener, rule, move |mut socket| {
        let conn = conn.clone();
        async move {
            let Ok(SocksRequest::Connect { host, port }) = socks::read_request(&mut socket).await
            else {
                return;
            };
            let (address, peer_port) = match socket.peer_addr() {
                Ok(addr) => (addr.ip().to_string(), u32::from(addr.port())),
                Err(_) => (LOOPBACK.to_string(), 0),
            };
            match conn
                .open_direct_tcpip(&host, u32::from(port), &address, peer_port)
                .await
            {
                Ok(channel) => {
                    if socks::reply(&mut socket, socks::REPLY_SUCCEEDED)
                        .await
                        .is_err()
                    {
                        let _ = channel.close().await;
                        return;
                    }
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                }
                Err(err) => {
                    tracing::debug!(%err, %host, port, "SOCKS connect through the forward failed");
                    let _ = socks::reply(&mut socket, socks::REPLY_CONNECTION_REFUSED).await;
                    let _ = tokio::io::AsyncWriteExt::shutdown(&mut socket).await;
                }
            }
        }
    }))
}

/// Remote forwards of one connection by their server-side port: the connection's single
/// receiver for `forwarded-tcpip` channels.
#[derive(Default)]
pub(crate) struct RemoteDispatch {
    routes: Mutex<HashMap<u32, (String, u16)>>,
}

impl ServerChannelSink for RemoteDispatch {
    fn accepts_forwarded_tcpip(&self, _connected_address: &str, connected_port: u32) -> bool {
        lock(&self.routes).contains_key(&connected_port)
    }

    fn forwarded_tcpip(
        &self,
        channel: Channel<Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
    ) {
        let Some((host, port)) = lock(&self.routes).get(&connected_port).cloned() else {
            return;
        };
        tokio::spawn(async move {
            match TcpStream::connect((host.as_str(), port)).await {
                Ok(mut socket) => {
                    let _ = socket.set_nodelay(true);
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                }
                Err(err) => {
                    tracing::debug!(
                        %err, %host, port,
                        "remote forward could not reach its local target"
                    );
                    let _ = channel.close().await;
                }
            }
        });
    }
}

/// Every connection's [`RemoteDispatch`], by connection id.
pub(crate) type RemoteDispatchers = Arc<Mutex<HashMap<String, Arc<RemoteDispatch>>>>;

/// A remote forward that is up. Dropping it stops routing its channels and asks the server to
/// close the listener.
pub(crate) struct RemoteRoute {
    dispatchers: RemoteDispatchers,
    dispatch: Arc<RemoteDispatch>,
    conn: Arc<ManagedConnection>,
    port: u32,
}

/// -R: ask the server to listen; route incoming channels to the local target.
pub(crate) async fn start_remote(
    dispatchers: &RemoteDispatchers,
    conn: Arc<ManagedConnection>,
    rule: Rule,
) -> Result<RemoteRoute, Problem> {
    let requested = u32::from(rule.bind_port);
    let allocated = conn
        .handle()
        .tcpip_forward(LOOPBACK, requested)
        .await
        // ssh2's words for a refused `tcpip-forward`, which the client shows.
        .map_err(|_| {
            Problem::bad_request(format!(
                "remote bind failed: Unable to bind to {LOOPBACK}:{requested}"
            ))
        })?;
    // The server names the port it picked only when asked for any port.
    let port = if requested == 0 { allocated } else { requested };
    let dispatch = {
        let mut all = lock(dispatchers);
        let dispatch = all
            .entry(conn.id.clone())
            .or_insert_with(|| {
                let dispatch = Arc::new(RemoteDispatch::default());
                conn.transport.add_channel_sink(dispatch.clone());
                dispatch
            })
            .clone();
        lock(&dispatch.routes).insert(port, (rule.target_host, rule.target_port));
        dispatch
    };
    Ok(RemoteRoute {
        dispatchers: dispatchers.clone(),
        dispatch,
        conn,
        port,
    })
}

impl Drop for RemoteRoute {
    fn drop(&mut self) {
        {
            let mut all = lock(&self.dispatchers);
            let empty = {
                let mut routes = lock(&self.dispatch.routes);
                routes.remove(&self.port);
                routes.is_empty()
            };
            if empty {
                if all
                    .get(&self.conn.id)
                    .is_some_and(|d| Arc::ptr_eq(d, &self.dispatch))
                {
                    all.remove(&self.conn.id);
                }
                let sink: Arc<dyn ServerChannelSink> = self.dispatch.clone();
                self.conn.transport.remove_channel_sink(&sink);
            }
        }
        // The connection may already be gone, and the server's listener with it.
        if self.conn.is_closed() {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let conn = self.conn.clone();
            let port = self.port;
            runtime.spawn(async move {
                let _ = conn.handle().cancel_tcpip_forward(LOOPBACK, port).await;
            });
        }
    }
}
