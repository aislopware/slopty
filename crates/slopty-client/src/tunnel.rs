//! Ports forwarded from a worker: what listens in its shells is reachable here on
//! `127.0.0.1:<same port>`.
//!
//! The same port keeps origins, cookies, OAuth redirects and a dev server's host check working.
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

/// One port of a worker, as this client serves it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Forward {
    /// The port as the worker reported it.
    pub port: Port,
    /// Where it is reachable here; `None` when no local port could be had.
    pub local: Option<u16>,
}

impl Forward {
    /// The address to open in a browser.
    #[must_use]
    pub fn url(&self) -> Option<String> {
        self.local.map(|local| format!("http://localhost:{local}"))
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
        Self { conn, sessions: HashMap::new(), listeners: BTreeMap::new() }
    }

    /// `session` listens on `ports` now (the whole set). Listeners start for new ports and stop
    /// for ports no session has any more; returns the session's forwards.
    pub fn update(&mut self, session: SessionId, ports: Vec<Port>) -> Vec<Forward> {
        if ports.is_empty() {
            self.sessions.remove(&session);
        } else {
            self.sessions.insert(session, ports.clone());
        }
        let wanted: BTreeSet<u16> = self.sessions.values().flatten().map(|p| p.number).collect();
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
        ports
            .into_iter()
            .map(|port| {
                let local = self.listeners.get(&port.number).and_then(|l| l.local);
                Forward { port, local }
            })
            .collect()
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
                    if let Err(e) = splice(socket, &conn, port).await {
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

/// Join a local connection to a tunnel stream until both directions finish.
async fn splice(socket: TcpStream, conn: &Connection, port: u16) -> Result<(), String> {
    let (mut send, mut recv) =
        slopty_net::streams::open_tunnel(conn, port).await.map_err(|e| e.to_string())?;
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
