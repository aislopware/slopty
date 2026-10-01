//! The worker's network for this client's pages: a SOCKS5 proxy on this client's loopback
//! (RFC 1928: CONNECT, no authentication), each connection a tunnel the worker dials. A web
//! view that takes it as its proxy reaches every host the worker reaches, by the worker's own
//! names: a container, a LAN machine, a name in its `/etc/hosts`.
//!
//! A CONNECT is answered at once, before the worker has dialled. The request the page sends
//! next leaves right behind the tunnel's header, so a new connection costs the one round trip
//! a forwarded port's does. A target the worker cannot reach then resets the connection rather
//! than failing the CONNECT, and the worker's reason is kept for the page to say why
//! ([`Proxy::refusal`]).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_net::Connection;
use slopty_proto::transfer::{TunnelHost, TunnelOpen, TunnelRefusal};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

use super::{Heard, OPEN_WITHIN, join, keep_accepting, open_within};

/// The protocol's version byte, first in every message.
const VERSION: u8 = 5;
/// The authentication method "none".
const NO_AUTH: u8 = 0;
/// The method byte that refuses every method offered.
const NO_METHOD: u8 = 0xff;
/// The one command served.
const CONNECT: u8 = 1;
/// Address types: an IPv4 address, a name, an IPv6 address.
const IPV4: u8 = 1;
const NAME: u8 = 3;
const IPV6: u8 = 4;
/// Reply codes: on the way, a failure of the proxy's own, an unserved command, an unserved
/// address type.
const SUCCEEDED: u8 = 0;
const FAILURE: u8 = 1;
const NO_COMMAND: u8 = 7;
const NO_ADDRESS_TYPE: u8 = 8;

/// How long a connection may take to say where it goes. The page's network stack sends its
/// greeting and request at once; anything slower is not a page.
const HANDSHAKE: Duration = Duration::from_secs(5);

/// Why the worker last could not reach each target, by host as the page named it and port.
type Refusals = Arc<Mutex<HashMap<(String, u16), TunnelRefusal>>>;

/// One worker link's proxy. Dropping it stops taking connections; those taken run on until
/// they end or the link does.
pub struct Proxy {
    local: u16,
    task: JoinHandle<()>,
    refusals: Refusals,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy").field("local", &self.local).finish_non_exhaustive()
    }
}

impl Proxy {
    /// A proxy over `conn` on a free port of this machine's loopback. Call it inside the
    /// link's runtime.
    ///
    /// # Errors
    ///
    /// When no loopback port can be bound.
    pub fn start(conn: Connection) -> std::io::Result<Self> {
        let tcp = super::exclusive(0)?;
        let local = tcp.local_addr()?.port();
        let refusals = Refusals::default();
        let taken = Arc::clone(&refusals);
        let take = move |(socket, _from): (TcpStream, std::net::SocketAddr)| {
            let (conn, refusals) = (conn.clone(), Arc::clone(&taken));
            tokio::spawn(async move {
                if let Err(e) = relay(socket, &conn, &refusals).await {
                    tracing::debug!(error = %e, "proxied connection ended");
                }
            });
        };
        let task = tokio::spawn(async move { keep_accepting(local, || tcp.accept(), take).await });
        tracing::info!(local, "the worker's network is served here");
        Ok(Self { local, task, refusals })
    }

    /// The port it listens on, on `127.0.0.1`.
    #[must_use]
    pub const fn local(&self) -> u16 {
        self.local
    }

    /// Why the worker could not reach `host:port` the last time a page asked, unless it has
    /// answered since. `host` as the page names it.
    #[must_use]
    pub fn refusal(&self, host: &str, port: u16) -> Option<TunnelRefusal> {
        self.refusals.lock().get(&(host.to_ascii_lowercase(), port)).copied()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serve one connection: read where it goes, answer it, and join it to a tunnel there.
async fn relay(
    mut socket: TcpStream,
    conn: &Connection,
    refusals: &Refusals,
) -> Result<(), String> {
    // As a forward's: a dev server's websocket is as latency-bound as a terminal.
    let _nodelay = socket.set_nodelay(true);
    let open = match tokio::time::timeout(HANDSHAKE, handshake(&mut socket)).await {
        Ok(open) => open?,
        Err(_late) => return Err("no request in time".to_owned()),
    };
    let target = (named(&open.host), open.port);
    tracing::debug!(host = %target.0, port = target.1, "proxied tunnel");
    let (send, recv) = match open_within(conn, &open, OPEN_WITHIN).await {
        Ok(stream) => stream,
        Err(e) => {
            let _told = socket.write_all(&reply(FAILURE)).await;
            return Err(e);
        }
    };
    socket.write_all(&reply(SUCCEEDED)).await.map_err(|e| e.to_string())?;
    // Kept before the page hears the reset, so the page's failure finds it.
    let heard = |heard| {
        let mut refusals = refusals.lock();
        let _was = match heard {
            Heard::Answered => refusals.remove(&target),
            Heard::Refused(why) => refusals.insert(target.clone(), why),
        };
    };
    join(socket, send, recv, heard).await.map_err(|broke| broke.to_string())
}

/// A reply to a request with `code`; the bound address is unused by a CONNECT's client, so it
/// is all zeros.
const fn reply(code: u8) -> [u8; 10] {
    [VERSION, code, 0, IPV4, 0, 0, 0, 0, 0, 0]
}

/// Read the greeting and the request, answering the greeting: where the connection goes. A
/// request this proxy does not serve is answered with why before the error returns.
async fn handshake(socket: &mut TcpStream) -> Result<TunnelOpen, String> {
    let io = |e: std::io::Error| e.to_string();
    let mut greeting = [0_u8; 2];
    socket.read_exact(&mut greeting).await.map_err(io)?;
    let [VERSION, count] = greeting else { return Err(format!("not SOCKS5: {greeting:?}")) };
    let mut methods = vec![0_u8; usize::from(count)];
    socket.read_exact(&mut methods).await.map_err(io)?;
    if !methods.contains(&NO_AUTH) {
        socket.write_all(&[VERSION, NO_METHOD]).await.map_err(io)?;
        return Err("asks for authentication".to_owned());
    }
    socket.write_all(&[VERSION, NO_AUTH]).await.map_err(io)?;
    let mut request = [0_u8; 4];
    socket.read_exact(&mut request).await.map_err(io)?;
    let [VERSION, command, _reserved, kind] = request else {
        return Err(format!("not a SOCKS5 request: {request:?}"));
    };
    if command != CONNECT {
        socket.write_all(&reply(NO_COMMAND)).await.map_err(io)?;
        return Err(format!("command {command}"));
    }
    let host = match kind {
        IPV4 => {
            let mut ip = [0_u8; 4];
            socket.read_exact(&mut ip).await.map_err(io)?;
            TunnelHost::Ip(Ipv4Addr::from(ip).into())
        }
        IPV6 => {
            let mut ip = [0_u8; 16];
            socket.read_exact(&mut ip).await.map_err(io)?;
            TunnelHost::Ip(Ipv6Addr::from(ip).into())
        }
        NAME => {
            let len = socket.read_u8().await.map_err(io)?;
            let mut name = vec![0_u8; usize::from(len)];
            socket.read_exact(&mut name).await.map_err(io)?;
            let Ok(name) = String::from_utf8(name) else {
                socket.write_all(&reply(NO_ADDRESS_TYPE)).await.map_err(io)?;
                return Err("a name that is not UTF-8".to_owned());
            };
            host_of(name)
        }
        kind => {
            socket.write_all(&reply(NO_ADDRESS_TYPE)).await.map_err(io)?;
            return Err(format!("address type {kind}"));
        }
    };
    let port = socket.read_u16().await.map_err(io)?;
    Ok(TunnelOpen { host, port })
}

/// A name as the worker dials it: `localhost` is its loopback (both families, as a forward
/// dials), an address written out is that address, and the rest is for its resolver.
fn host_of(name: String) -> TunnelHost {
    if name.eq_ignore_ascii_case("localhost") {
        TunnelHost::Loopback
    } else if let Ok(ip) = name.parse::<IpAddr>() {
        TunnelHost::Ip(ip)
    } else {
        TunnelHost::Name(name)
    }
}

/// The host as a page names it, which is how [`Proxy::refusal`] is asked.
fn named(host: &TunnelHost) -> String {
    match host {
        TunnelHost::Loopback => "localhost".to_owned(),
        TunnelHost::Name(name) => name.to_ascii_lowercase(),
        TunnelHost::Ip(ip) => ip.to_string(),
    }
}

#[cfg(test)]
mod tests;
