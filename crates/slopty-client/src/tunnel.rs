//! Ports forwarded from a worker: what listens in its shells is reachable here on
//! `127.0.0.1:<same port>`.
//!
//! The same port keeps origins, cookies, OAuth redirects and a dev server's worker check working.
//! When it is taken here (the worker on this very Mac, or another worker's forward) the next
//! free one serves instead, and the forward says so. Each accepted connection is one tunnel
//! stream to the worker, which joins it to its own `127.0.0.1:<port>`; a finished direction is
//! a half-closed socket.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use slopty_core::SessionId;
use slopty_net::Connection;
use slopty_proto::orchestration::Port;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// How far past a taken port a forward looks for a free one before it takes any port.
const NEXT_FREE: u16 = 20;

/// Bytes moved per read in either direction.
const CHUNK: usize = 64 * 1024;

/// How long an accepted connection waits for its tunnel stream before it is reset. Opening
/// waits only while the worker grants no more streams; a reset lets the browser retry rather
/// than hang on a socket nothing will ever answer.
const OPEN_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

/// One port of a worker, as this client serves it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Forward {
    /// The port as the worker reported it.
    pub port: Port,
    /// Where it is reachable here; `None` when no local port could be had.
    pub local: Option<u16>,
}

impl Forward {
    /// The address to open in a browser here.
    #[must_use]
    pub fn url(&self) -> Option<String> {
        self.local.map(|local| format!("http://localhost:{local}"))
    }

    /// The address as the worker names it, the same on every client: what a browser tile's
    /// item holds.
    #[must_use]
    pub fn worker_url(&self) -> String {
        format!("http://localhost:{}/", self.port.number)
    }
}

struct Listener {
    local: Option<u16>,
    task: Option<JoinHandle<()>>,
}

/// The forwards of one worker link.
pub struct Forwards {
    conn: Connection,
    /// Ports each session reported.
    sessions: HashMap<SessionId, Vec<Port>>,
    /// Ports asked for by name (a browser tile's address), kept while the link lives whether
    /// or not a session lists them.
    pinned: BTreeSet<u16>,
    /// One listener per worker port, whichever sessions report it.
    listeners: BTreeMap<u16, Listener>,
}

impl std::fmt::Debug for Forwards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forwards").field("ports", &self.listeners.keys()).finish_non_exhaustive()
    }
}

impl Forwards {
    /// Forwards over `conn`, none yet.
    #[must_use]
    pub fn new(conn: Connection) -> Self {
        Self { conn, sessions: HashMap::new(), pinned: BTreeSet::new(), listeners: BTreeMap::new() }
    }

    /// `session` listens on `ports` now (the whole set). Listeners start for new ports and stop
    /// for ports neither a session nor a pin has any more; returns the session's forwards.
    /// Call it inside the link's runtime.
    pub fn update(&mut self, session: SessionId, ports: Vec<Port>) -> Vec<Forward> {
        if ports.is_empty() {
            self.sessions.remove(&session);
        } else {
            self.sessions.insert(session, ports.clone());
        }
        self.listen();
        ports
            .into_iter()
            .map(|port| {
                let local = self.local(port.number);
                Forward { port, local }
            })
            .collect()
    }

    /// Serve the worker's `port` here until the link goes, whether or not a session lists it;
    /// where it is reachable here. Call it inside the link's runtime.
    pub fn pin(&mut self, port: u16) -> Option<u16> {
        self.pinned.insert(port);
        self.listen();
        self.local(port)
    }

    /// Where the worker's `port` is reachable here, if it is served.
    #[must_use]
    pub fn local(&self, port: u16) -> Option<u16> {
        self.listeners.get(&port).and_then(|l| l.local)
    }

    /// A listener for every port a session lists or a pin holds, and none for the rest.
    fn listen(&mut self) {
        let wanted: BTreeSet<u16> = self
            .sessions
            .values()
            .flatten()
            .map(|p| p.number)
            .chain(self.pinned.iter().copied())
            .collect();
        self.listeners.retain(|port, listener| {
            let keep = wanted.contains(port);
            if !keep && let Some(task) = listener.task.take() {
                task.abort();
            }
            keep
        });
        for port in wanted {
            if self.listeners.contains_key(&port) {
                continue;
            }
            let listener = if let Some(tcp) = bind(port) {
                let local = tcp.local_addr().ok().map(|a| a.port());
                let conn = self.conn.clone();
                tracing::info!(port, ?local, "forwarding");
                Listener { local, task: Some(tokio::spawn(accept(tcp, conn, port))) }
            } else {
                tracing::warn!(port, "no local port to forward on");
                Listener { local: None, task: None }
            };
            self.listeners.insert(port, listener);
        }
    }

    /// Stop every listener (the link is going).
    pub fn clear(&mut self) {
        for listener in self.listeners.values_mut() {
            if let Some(task) = listener.task.take() {
                task.abort();
            }
        }
        self.listeners.clear();
        self.sessions.clear();
        self.pinned.clear();
    }
}

impl Drop for Forwards {
    fn drop(&mut self) {
        self.clear();
    }
}

/// `127.0.0.1:port`, else the next free port after it, else any.
fn bind(port: u16) -> Option<TcpListener> {
    let tries = (0..=NEXT_FREE).filter_map(|n| port.checked_add(n)).chain([0]);
    for candidate in tries {
        if candidate == 0 && port != 0 {
            tracing::debug!(port, "the ports after it are taken too; any port");
        }
        if let Ok(listener) = exclusive(candidate) {
            return Some(listener);
        }
    }
    None
}

/// A listener on `127.0.0.1:port` without `SO_REUSEADDR`. With it (what `TcpListener::bind`
/// sets), BSD sockets let a loopback bind shadow a server listening on every interface, so a
/// worker on this very Mac would hand its own server's connections to the forward, which
/// hands them back to the worker: a loop.
fn exclusive(port: u16) -> std::io::Result<TcpListener> {
    let socket = tokio::net::TcpSocket::new_v4()?;
    socket.set_reuseaddr(false)?;
    socket.bind(std::net::SocketAddr::from(([127, 0, 0, 1], port)))?;
    socket.listen(1024)
}

async fn accept(tcp: TcpListener, conn: Connection, port: u16) {
    loop {
        match tcp.accept().await {
            Ok((socket, from)) => {
                tracing::debug!(port, %from, "tunnel");
                let conn = conn.clone();
                tokio::spawn(async move {
                    if let Err(e) = splice(socket, &conn, port, OPEN_WITHIN).await {
                        tracing::debug!(port, error = %e, "tunnel ended");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(port, error = %e, "forward accept");
                return;
            }
        }
    }
}

/// Join a local connection to a tunnel stream until both directions finish. A stream not had
/// `within` resets the connection.
async fn splice(
    socket: TcpStream,
    conn: &Connection,
    port: u16,
    within: std::time::Duration,
) -> Result<(), String> {
    // Keystrokes into a forwarded dev server's websocket are as latency-bound as a terminal's.
    let _nodelay = socket.set_nodelay(true);
    let opened = tokio::time::timeout(within, slopty_net::streams::open_tunnel(conn, port)).await;
    let (mut send, mut recv) = match opened {
        Ok(opened) => opened.map_err(|e| e.to_string())?,
        Err(_elapsed) => {
            // A zero linger turns the close into a reset: the peer sees a refused request, not
            // an empty answer.
            let _linger = socket.set_zero_linger();
            return Err(format!("no tunnel stream within {within:?}"));
        }
    };
    let (mut from_app, mut to_app) = socket.into_split();
    let up = async {
        let mut buf = vec![0_u8; CHUNK];
        loop {
            let n = from_app.read(&mut buf).await.map_err(|e| e.to_string())?;
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                return send.finish().map_err(|e| e.to_string());
            };
            send.write_all(chunk).await.map_err(|e| e.to_string())?;
        }
    };
    let down = async {
        while let Some(chunk) = recv.chunk(CHUNK).await.map_err(|e| e.to_string())? {
            to_app.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        to_app.shutdown().await.map_err(|e| e.to_string())
    };
    let (up, down) = tokio::join!(up, down);
    up.and(down)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use tokio::io::AsyncReadExt as _;
    use tokio::net::{TcpListener, TcpStream};

    use super::splice;

    /// A connection to a peer that grants every stream it allows and never closes one: the next
    /// tunnel has to wait for a stream that will not come.
    #[tokio::test]
    async fn a_connection_with_no_stream_to_be_had_is_reset() {
        let loopback = SocketAddr::from(([127, 0, 0, 1], 0));
        let worker = slopty_net::endpoint::bind(loopback, true).unwrap();
        let client = slopty_net::endpoint::bind(loopback, false).unwrap();
        let at = worker.local_addr().unwrap();
        let (conn, _worker_side) =
            tokio::join!(async { client.connect(at, "worker").unwrap().await.unwrap() }, async {
                worker.accept().await.unwrap().await.unwrap()
            },);
        let mut held = Vec::new();
        for _ in 0..slopty_net::endpoint::MAX_STREAMS {
            held.push(conn.open_bi().await.unwrap());
        }

        let local = TcpListener::bind(loopback).await.unwrap();
        let mut browser = TcpStream::connect(local.local_addr().unwrap()).await.unwrap();
        let (accepted, _from) = local.accept().await.unwrap();
        let ended = splice(accepted, &conn, 80, Duration::from_millis(200)).await;
        assert_eq!(held.len(), usize::try_from(slopty_net::endpoint::MAX_STREAMS).unwrap());
        assert!(ended.unwrap_err().contains("no tunnel stream"));
        let mut byte = [0_u8; 1];
        let read = browser.read(&mut byte).await;
        assert_eq!(read.unwrap_err().kind(), std::io::ErrorKind::ConnectionReset);
    }
}
