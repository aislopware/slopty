//! A link a test can cut: a UDP relay on loopback between a daemon and the server it dials.
//!
//! It forwards every datagram both ways until it is [cut](Relay::cut), drops every one while it
//! is, and forwards again once [restored](Relay::restore).
//!
//! A server link is QUIC, so this is a network loss as the two ends meet one: packets stop and
//! nothing says so, while both processes and everything else on the box keep running. Nothing
//! is stopped, unlike a `SIGSTOP`, which freezes the daemon's own local work (an agent's hooks)
//! as no network loss does.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

/// The largest datagram forwarded; QUIC's are well under it.
const DATAGRAM: usize = 65_535;

/// A relay to one address; dropped, it stops.
#[derive(Debug)]
pub struct Relay {
    addr: SocketAddr,
    link: Arc<Link>,
    task: JoinHandle<()>,
}

/// What the relay's tasks share.
#[derive(Debug, Default)]
struct Link {
    /// Whether the link is cut: every datagram is dropped.
    cut: AtomicBool,
    /// Datagrams dropped while it was.
    dropped: AtomicU64,
}

impl Link {
    /// Whether a datagram goes on; one that does not is counted.
    fn passes(&self) -> bool {
        let cut = self.cut.load(Ordering::Acquire);
        if cut {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        !cut
    }
}

impl Relay {
    /// A relay on loopback to `to`, forwarding from now on. Each address that sends to it gets
    /// a socket of its own toward `to`, so the far end tells the senders apart as it would
    /// without the relay.
    ///
    /// # Errors
    ///
    /// When no loopback socket can be bound.
    pub async fn start(to: SocketAddr) -> io::Result<Self> {
        let front = Arc::new(UdpSocket::bind(loopback(to)).await?);
        let addr = front.local_addr()?;
        let link = Arc::new(Link::default());
        let task = tokio::spawn(forward(front, to, Arc::clone(&link)));
        Ok(Self { addr, link, task })
    }

    /// Where a daemon dials to go through the relay.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Cut the link: every datagram either way is dropped from now on, and nobody is told.
    pub fn cut(&self) {
        self.link.cut.store(true, Ordering::Release);
    }

    /// Restore the link: datagrams go on again.
    pub fn restore(&self) {
        self.link.cut.store(false, Ordering::Release);
    }

    /// How many datagrams were dropped while the link was cut.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.link.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Loopback, port chosen, in the family of `to`.
fn loopback(to: SocketAddr) -> SocketAddr {
    match to {
        SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
    }
}

/// Forward what reaches `front` to `to`, each sender through a socket of its own whose replies
/// go back to it, until the relay is dropped; the senders' tasks end with this one.
async fn forward(front: Arc<UdpSocket>, to: SocketAddr, link: Arc<Link>) {
    let mut toward: HashMap<SocketAddr, Arc<UdpSocket>> = HashMap::new();
    let mut backs = tokio::task::JoinSet::new();
    let mut buf = vec![0_u8; DATAGRAM];
    loop {
        let Ok((len, from)) = front.recv_from(&mut buf).await else { return };
        if !link.passes() {
            continue;
        }
        let up = if let Some(up) = toward.get(&from) {
            Arc::clone(up)
        } else {
            let Ok(up) = UdpSocket::bind(loopback(to)).await else { continue };
            if up.connect(to).await.is_err() {
                continue;
            }
            let up = Arc::new(up);
            backs.spawn(back(Arc::clone(&up), Arc::clone(&front), from, Arc::clone(&link)));
            toward.insert(from, Arc::clone(&up));
            up
        };
        // A datagram lost on the way is a datagram lost; QUIC sends it again.
        let _sent = up.send(buf.get(..len).unwrap_or_default()).await;
    }
}

/// Send what `to` answers on `up` back to `sender` through `front`, while the link passes it.
async fn back(up: Arc<UdpSocket>, front: Arc<UdpSocket>, sender: SocketAddr, link: Arc<Link>) {
    let mut buf = vec![0_u8; DATAGRAM];
    loop {
        // The far end gone for a moment (a server restarting) refuses a datagram; the relay
        // goes on, as the network would. Any other failure ends this sender's way back.
        let len = match up.recv(&mut buf).await {
            Ok(len) => len,
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => continue,
            Err(_) => return,
        };
        if !link.passes() {
            continue;
        }
        let _sent = front.send_to(buf.get(..len).unwrap_or_default(), sender).await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// What `socket` receives within a second, if anything.
    async fn heard(socket: &UdpSocket) -> Option<Vec<u8>> {
        let mut buf = [0_u8; 64];
        let got = tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut buf)).await;
        let (len, _) = got.ok()?.ok()?;
        Some(buf.get(..len)?.to_vec())
    }

    /// Datagrams go both ways through the relay; cut, none goes either way and each is
    /// counted; restored, they go again, on the same path.
    #[tokio::test]
    async fn a_cut_link_drops_everything_both_ways_until_restored() {
        let far = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay = Relay::start(far.local_addr().unwrap()).await.unwrap();
        let near = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        near.send_to(b"hello", relay.addr()).await.unwrap();
        let mut buf = [0_u8; 64];
        let (len, path) = far.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], b"hello");
        far.send_to(b"back", path).await.unwrap();
        assert_eq!(heard(&near).await.as_deref(), Some(&b"back"[..]));

        relay.cut();
        near.send_to(b"lost", relay.addr()).await.unwrap();
        assert_eq!(heard(&far).await, None, "nothing reaches the far end");
        far.send_to(b"lost too", path).await.unwrap();
        assert_eq!(heard(&near).await, None, "nothing comes back");
        assert_eq!(relay.dropped(), 2);

        relay.restore();
        near.send_to(b"again", relay.addr()).await.unwrap();
        let (len, again) = far.recv_from(&mut buf).await.unwrap();
        assert_eq!((&buf[..len], again), (&b"again"[..], path), "the same path");
        far.send_to(b"back again", path).await.unwrap();
        assert_eq!(heard(&near).await.as_deref(), Some(&b"back again"[..]));
    }
}
