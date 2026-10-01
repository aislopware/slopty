//! Port tunnels: each bidirectional stream a client opens after the control stream is a TCP
//! connection it accepted, joined here to the host and port it names as this worker reaches
//! them: its own loopback, or any name or address its resolver and routes reach.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_core::ClientId;
use slopty_net::Connection;
use slopty_net::streams::{self, RawRecv};
use slopty_proto::transfer::{TunnelHost, TunnelRefusal};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::Instant;

/// Bytes moved at a time each way.
const CHUNK: usize = 64 << 10;

/// How long one address may take to answer before the next is tried. A LAN host that is off
/// drops the SYN rather than refusing it, and the page waits on this.
const DIAL: Duration = Duration::from_secs(5);

/// How long a name's addresses are dialled without asking the resolver again. A page opens
/// many connections to its host at once, and a lookup costs this worker a millisecond, the
/// whole of a fresh connection's budget on a fast link (MEASUREMENTS.md, "a page through the
/// worker's proxy"). Chromium keeps a system lookup for a minute (`HostCache`); an address
/// that no longer answers is looked up again at once, so a moved host costs one dial.
const KEEP_NAME: Duration = Duration::from_secs(60);

/// The addresses of the names this client's tunnels went to lately, the one that answered
/// last first.
#[derive(Default)]
struct Names(Mutex<HashMap<String, (Instant, Vec<IpAddr>)>>);

impl Names {
    /// `name`'s addresses, while they are fresh at `now`.
    fn get(&self, name: &str, now: Instant) -> Option<Vec<IpAddr>> {
        let (at, addrs) = self.0.lock().get(name).cloned()?;
        (now.saturating_duration_since(at) < KEEP_NAME).then_some(addrs)
    }

    /// `name` is at `addrs` as of `now`, and `answered` took the connection.
    fn put(&self, name: &str, mut addrs: Vec<IpAddr>, answered: IpAddr, now: Instant) {
        if let Some(head) =
            addrs.iter().position(|a| *a == answered).and_then(|at| addrs.get_mut(..=at))
        {
            head.rotate_right(1);
        }
        let mut names = self.0.lock();
        // A client's tunnels name a handful of hosts; the stale go when one more comes.
        names.retain(|_, (at, _)| now.saturating_duration_since(*at) < KEEP_NAME);
        names.insert(name.to_owned(), (now, addrs));
    }
}

/// Accept the client's tunnels until the connection ends. Each header is read on the tunnel's
/// own task: one whose header was lost holds up no tunnel opened after it.
pub async fn accept(conn: Connection, client: ClientId) {
    let names = Arc::new(Names::default());
    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::debug!(%client, error = %e, "tunnels end");
                break;
            }
        };
        let names = Arc::clone(&names);
        drop(tokio::spawn(async move {
            match streams::read_tunnel(send, recv).await {
                Ok((open, send, rx)) => {
                    tracing::debug!(%client, host = ?open.host, port = open.port, "tunnel");
                    splice(&open.host, open.port, &names, send, rx).await;
                }
                Err(e) => tracing::debug!(%client, error = %e, "tunnel refused"),
            }
        }));
    }
}

/// A server at `host:port` as this worker reaches it. The loopback is IPv4 first, then IPv6 (a
/// dev server bound to `localhost` on macOS is often on `::1` only); a name is every address
/// the resolver gives, in its order, until one answers, from `names` while they are fresh.
async fn connect(host: &TunnelHost, port: u16, names: &Names) -> Result<TcpStream, TunnelRefusal> {
    let name = match host {
        TunnelHost::Loopback => {
            let loopback = [IpAddr::from(Ipv4Addr::LOCALHOST), IpAddr::from(Ipv6Addr::LOCALHOST)];
            return dial(&loopback, port).await.map(|(tcp, _)| tcp);
        }
        TunnelHost::Ip(ip) => return dial(&[*ip], port).await.map(|(tcp, _)| tcp),
        TunnelHost::Name(name) => name.to_ascii_lowercase(),
    };
    let kept = names.get(&name, Instant::now());
    let mut why = None;
    if let Some(addrs) = &kept {
        match dial(addrs, port).await {
            Ok((tcp, at)) => {
                names.put(&name, addrs.clone(), at, Instant::now());
                return Ok(tcp);
            }
            Err(refused) => why = Some(refused),
        }
    }
    let addrs: Vec<IpAddr> = match tokio::net::lookup_host((name.as_str(), port)).await {
        Ok(addrs) => addrs.map(|a| a.ip()).collect(),
        Err(e) => {
            tracing::debug!(%name, error = %e, "tunnel target unresolved");
            return Err(TunnelRefusal::Unresolved);
        }
    };
    // The same addresses refuse the same way: no second dial for what the first heard.
    if let (Some(why), Some(kept)) = (why, &kept)
        && same_set(kept, &addrs)
    {
        return Err(why);
    }
    let (tcp, at) = dial(&addrs, port).await?;
    names.put(&name, addrs, at, Instant::now());
    Ok(tcp)
}

/// Whether `a` and `b` hold the same addresses, in any order.
fn same_set(a: &[IpAddr], b: &[IpAddr]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.contains(x))
}

/// Dial each of `addrs` at `port` in turn until one answers: the connection and who it is.
async fn dial(addrs: &[IpAddr], port: u16) -> Result<(TcpStream, IpAddr), TunnelRefusal> {
    // A refusal says most (the host is there, the port is not), so it outranks the rest.
    let mut why = None;
    for &ip in addrs {
        let addr = SocketAddr::new(ip, port);
        match tokio::time::timeout(DIAL, TcpStream::connect(addr)).await {
            Ok(Ok(tcp)) => return Ok((tcp, ip)),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                why = Some(TunnelRefusal::Refused);
            }
            Ok(Err(e)) => {
                tracing::debug!(%addr, error = %e, "tunnel target unreachable");
                why = why.or(Some(TunnelRefusal::Unreachable));
            }
            Err(_late) => why = why.or(Some(TunnelRefusal::Unreachable)),
        }
    }
    Err(why.unwrap_or(TunnelRefusal::Unresolved))
}

/// Join the stream to `host:port` both ways until both directions finish. A finished stream
/// shuts the socket's write half, and the socket's EOF finishes the stream. A target that
/// cannot be reached resets the stream with why.
async fn splice(
    host: &TunnelHost,
    port: u16,
    names: &Names,
    mut send: noq::SendStream,
    mut rx: RawRecv,
) {
    let tcp = match connect(host, port, names).await {
        Ok(tcp) => tcp,
        Err(why) => {
            tracing::debug!(?host, port, ?why, "tunnel target not reached");
            let _reset = send.reset(why.code().into());
            rx.stop();
            return;
        }
    };
    let _nodelay = tcp.set_nodelay(true);
    let (mut from_tcp, mut to_tcp) = tcp.into_split();
    let up = async {
        while let Ok(Some(chunk)) = rx.chunk(CHUNK).await {
            if to_tcp.write_all(&chunk).await.is_err() {
                rx.stop();
                return;
            }
        }
        let _shut = to_tcp.shutdown().await;
    };
    let down = async {
        let mut buf = vec![0_u8; CHUNK];
        loop {
            match from_tcp.read(&mut buf).await {
                Ok(0) => {
                    let _finished = send.finish();
                    return;
                }
                Ok(n) => {
                    if send.write_all(buf.get(..n).unwrap_or_default()).await.is_err() {
                        return;
                    }
                }
                Err(_e) => {
                    let _reset = send.reset(0_u32.into());
                    return;
                }
            }
        }
    };
    tokio::join!(up, down);
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use tokio::time::Instant;

    use super::{KEEP_NAME, Names, same_set};

    /// A name's addresses are kept for a minute, the one that answered first, and are gone
    /// after it.
    #[test]
    fn a_name_is_kept_answered_first_until_it_ages() {
        let names = Names::default();
        let now = Instant::now();
        let (v6, v4) = (IpAddr::from(Ipv6Addr::LOCALHOST), IpAddr::from(Ipv4Addr::LOCALHOST));
        let other = IpAddr::from(Ipv4Addr::new(10, 0, 0, 7));
        names.put("db", vec![v6, other, v4], v4, now);
        let kept = names.get("db", now);
        assert_eq!(kept, Some(vec![v4, v6, other]), "the one that answered first");
        assert_eq!(names.get("web", now), None);
        assert_eq!(names.get("db", now + KEEP_NAME), None, "aged out");
    }

    #[test]
    fn the_same_addresses_in_any_order_are_the_same_set() {
        let (a, b) = (IpAddr::from(Ipv4Addr::LOCALHOST), IpAddr::from(Ipv6Addr::LOCALHOST));
        assert!(same_set(&[a, b], &[b, a]));
        assert!(!same_set(&[a], &[a, b]));
        assert!(!same_set(&[a, b], &[a, a]));
    }
}
