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

/// The shortest and the longest pause after an accept that failed for want of descriptors or
/// memory; the pause doubles while the failures last.
const ACCEPT_PAUSE: (std::time::Duration, std::time::Duration) =
    (std::time::Duration::from_millis(10), std::time::Duration::from_secs(1));

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
    let take = |(socket, from): (TcpStream, std::net::SocketAddr)| {
        tracing::debug!(port, %from, "tunnel");
        let conn = conn.clone();
        tokio::spawn(async move {
            if let Err(e) = splice(socket, &conn, port, OPEN_WITHIN).await {
                tracing::debug!(port, error = %e, "tunnel ended");
            }
        });
    };
    keep_accepting(port, || tcp.accept(), take).await;
}

/// Take every connection `next` accepts for as long as the forward lives (its task is aborted
/// when the forward goes). A failed accept is a connection gone before it was taken, tried
/// again at once, or the machine short of descriptors or memory (EMFILE, ENFILE, ENOBUFS),
/// tried again after a pause that grows while the failures last. None of them ends the
/// forward: the port would stay unserved until the link came back.
async fn keep_accepting<T, F: Future<Output = std::io::Result<T>>>(
    port: u16,
    mut next: impl FnMut() -> F,
    mut take: impl FnMut(T),
) -> ! {
    let mut pause = std::time::Duration::ZERO;
    loop {
        match next().await {
            Ok(accepted) => {
                pause = std::time::Duration::ZERO;
                take(accepted);
            }
            Err(e) if gone_before_taken(&e) => tracing::debug!(port, error = %e, "forward accept"),
            Err(e) => {
                if pause.is_zero() {
                    tracing::warn!(port, error = %e, "forward accept; pausing");
                }
                pause = pause.saturating_mul(2).clamp(ACCEPT_PAUSE.0, ACCEPT_PAUSE.1);
                tokio::time::sleep(pause).await;
            }
        }
    }
}

/// An accept that failed on the one connection, not the listener.
fn gone_before_taken(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{ConnectionAborted, ConnectionReset, Interrupted};
    matches!(e.kind(), ConnectionAborted | ConnectionReset | Interrupted)
}

/// Join a local connection to a tunnel stream until both directions finish. A stream not had
/// `within` resets the connection. A clean end of one direction half-closes it and the other
/// goes on; a failure of either ends both, each side reset so neither waits on the other: the
/// browser's socket when the worker's side failed, the stream when the browser's did.
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
            let n = from_app.read(&mut buf).await.map_err(Broke::app)?;
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                return send.finish().map_err(Broke::tunnel);
            };
            send.write_all(chunk).await.map_err(Broke::tunnel)?;
        }
    };
    let down = async {
        while let Some(chunk) = recv.chunk(CHUNK).await.map_err(Broke::tunnel)? {
            to_app.write_all(&chunk).await.map_err(Broke::app)?;
        }
        to_app.shutdown().await.map_err(Broke::app)
    };
    let broke = match tokio::try_join!(up, down) {
        Ok(((), ())) => return Ok(()),
        Err(broke) => broke,
    };
    match &broke {
        Broke::Tunnel(_) => {
            // A zero linger turns the close into a reset, as when no stream could be had. The
            // halves go back together first: a write half dropped alone sends a FIN, and the
            // peer would read that clean end before the reset.
            if let Ok(socket) = from_app.reunite(to_app) {
                let _linger = socket.set_zero_linger();
            }
        }
        Broke::App(_) => {
            let _reset = send.reset(0_u32.into());
            recv.stop();
        }
    }
    Err(broke.to_string())
}

/// Which side of a tunnel failed.
#[derive(Debug)]
enum Broke {
    /// The local connection.
    App(String),
    /// The stream to the worker.
    Tunnel(String),
}

impl Broke {
    fn app(e: impl std::fmt::Display) -> Self {
        Self::App(e.to_string())
    }

    fn tunnel(e: impl std::fmt::Display) -> Self {
        Self::Tunnel(e.to_string())
    }
}

impl std::fmt::Display for Broke {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "local connection: {e}"),
            Self::Tunnel(e) => write!(f, "tunnel stream: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::time::Duration;

    use slopty_net::Connection;
    use slopty_net::streams::accept_tunnel;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;

    use super::{keep_accepting, splice};

    const PATIENCE: Duration = Duration::from_secs(5);

    /// A client's connection to a worker, and the worker's end of it.
    async fn linked() -> (Connection, Connection) {
        let loopback = SocketAddr::from(([127, 0, 0, 1], 0));
        let worker = slopty_net::endpoint::bind(loopback, true).unwrap();
        let client = slopty_net::endpoint::bind(loopback, false).unwrap();
        let at = worker.local_addr().unwrap();
        tokio::join!(async { client.connect(at, "worker").unwrap().await.unwrap() }, async {
            worker.accept().await.unwrap().await.unwrap()
        })
    }

    /// A browser's socket, and a tunnel splicing the other end of it to the worker.
    async fn tunnel(conn: &Connection) -> (TcpStream, JoinHandle<Result<(), String>>) {
        let local = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap();
        let browser = TcpStream::connect(local.local_addr().unwrap()).await.unwrap();
        let (accepted, _from) = local.accept().await.unwrap();
        let conn = conn.clone();
        let splicing = tokio::spawn(async move { splice(accepted, &conn, 80, PATIENCE).await });
        (browser, splicing)
    }

    async fn ended(splicing: JoinHandle<Result<(), String>>) -> Result<(), String> {
        tokio::time::timeout(PATIENCE, splicing).await.expect("the tunnel ended").unwrap()
    }

    /// Nothing listens on the worker's port, so the worker resets the stream: the browser's
    /// connection is reset too rather than left waiting for an answer.
    #[tokio::test]
    async fn a_stream_the_worker_resets_resets_the_browser() {
        let (conn, worker) = linked().await;
        let (mut browser, splicing) = tunnel(&conn).await;
        browser.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        let (_open, mut send, mut rx) = accept_tunnel(&worker).await.unwrap();
        send.reset(0_u32.into()).unwrap();
        rx.stop();
        let mut byte = [0_u8; 1];
        let read = tokio::time::timeout(PATIENCE, browser.read(&mut byte)).await;
        let read = read.expect("the browser hears at once");
        assert_eq!(read.unwrap_err().kind(), std::io::ErrorKind::ConnectionReset);
        assert!(ended(splicing).await.is_err());
    }

    /// The browser resets its connection (a long poll or websocket given up on): the worker's
    /// stream is reset too rather than left open with the tunnel waiting on it.
    #[tokio::test]
    async fn a_browser_that_resets_resets_the_stream() {
        let (conn, worker) = linked().await;
        let (browser, splicing) = tunnel(&conn).await;
        let (_open, _send, mut rx) = accept_tunnel(&worker).await.unwrap();
        browser.set_zero_linger().unwrap();
        drop(browser);
        let heard = tokio::time::timeout(PATIENCE, rx.chunk(1024)).await;
        assert!(heard.expect("the worker hears at once").is_err(), "reset, not a clean end");
        assert!(ended(splicing).await.is_err());
    }

    /// Clean ends still half-close: the browser's request ends, the worker answers after it
    /// and ends, and the browser reads the whole answer.
    #[tokio::test]
    async fn clean_ends_half_close_each_way() {
        let (conn, worker) = linked().await;
        let (mut browser, splicing) = tunnel(&conn).await;
        browser.write_all(b"ping").await.unwrap();
        browser.shutdown().await.unwrap();
        let (_open, mut send, mut rx) = accept_tunnel(&worker).await.unwrap();
        let mut request = Vec::new();
        while let Some(chunk) = rx.chunk(1024).await.unwrap() {
            request.extend_from_slice(&chunk);
        }
        assert_eq!(request, b"ping");
        send.write_all(b"pong").await.unwrap();
        send.finish().unwrap();
        let mut answer = Vec::new();
        browser.read_to_end(&mut answer).await.unwrap();
        assert_eq!(answer, b"pong");
        ended(splicing).await.unwrap();
    }

    /// Accepting fails when the machine runs out of descriptors (EMFILE) or a connection is
    /// gone before it is taken: the port stays served, after a pause while descriptors are
    /// short rather than in a hot loop.
    #[tokio::test]
    async fn a_failed_accept_keeps_the_port_served() {
        const EMFILE: i32 = 24;
        let mut script: VecDeque<std::io::Result<u32>> = VecDeque::from([
            Err(std::io::Error::from_raw_os_error(EMFILE)),
            Err(std::io::Error::from_raw_os_error(EMFILE)),
            Err(std::io::ErrorKind::ConnectionAborted.into()),
            Ok(1),
            Err(std::io::Error::from_raw_os_error(EMFILE)),
            Ok(2),
        ]);
        let next = move || {
            let answer = script.pop_front();
            async move {
                match answer {
                    Some(answer) => answer,
                    None => std::future::pending().await,
                }
            }
        };
        let (took, mut taken) = tokio::sync::mpsc::unbounded_channel();
        let started = tokio::time::Instant::now();
        let serving = tokio::spawn(keep_accepting(80, next, move |n| {
            let _gone = took.send(n);
        }));
        assert_eq!(taken.recv().await, Some(1));
        let paused = started.elapsed();
        assert!(paused >= Duration::from_millis(20), "paused while short: {paused:?}");
        assert_eq!(taken.recv().await, Some(2));
        serving.abort();
    }

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
