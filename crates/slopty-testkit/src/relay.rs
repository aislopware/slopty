//! A link a test can cut: a UDP relay on loopback between a daemon and the server it dials.
//!
//! It forwards every datagram both ways until it is [cut](Relay::cut), drops every one while it
//! is, and forwards again once [restored](Relay::restore). Cut [one way](Relay::cut_back), what
//! the dialer sends arrives and nothing comes back: a request acted on whose answer is lost. And
//! it can [move](Relay::move_to) the dialer to another address, as a laptop roaming between
//! networks moves under a link that holds.
//!
//! A server link is QUIC, so this is a network loss as the two ends meet one: packets stop and
//! nothing says so, while both processes and everything else on the box keep running. Nothing
//! is stopped, unlike a `SIGSTOP`, which freezes the daemon's own local work (an agent's hooks)
//! as no network loss does.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::task::{AbortHandle, JoinHandle};

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
#[derive(Debug)]
struct Link {
    /// Whether what the dialer sends is dropped.
    forth_cut: AtomicBool,
    /// Whether what comes back to the dialer is dropped.
    back_cut: AtomicBool,
    /// Datagrams dropped while either way was cut.
    dropped: AtomicU64,
    /// The address the far end sees the dialer at, port aside.
    from: watch::Sender<IpAddr>,
}

impl Link {
    /// Whether a datagram goes on the way `cut` says; one that does not is counted.
    fn passes(&self, cut: &AtomicBool) -> bool {
        let cut = cut.load(Ordering::Acquire);
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
        let link = Arc::new(Link {
            forth_cut: AtomicBool::new(false),
            back_cut: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            from: watch::Sender::new(loopback(to).ip()),
        });
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
        self.link.forth_cut.store(true, Ordering::Release);
        self.link.back_cut.store(true, Ordering::Release);
    }

    /// Cut the way back alone: what the dialer sends still arrives, and nothing the far end
    /// sends reaches it, its answers and acknowledgements both.
    pub fn cut_back(&self) {
        self.link.back_cut.store(true, Ordering::Release);
    }

    /// Restore the link: datagrams go on again, both ways.
    pub fn restore(&self) {
        self.link.forth_cut.store(false, Ordering::Release);
        self.link.back_cut.store(false, Ordering::Release);
    }

    /// Move the dialer: from its next datagram on, the far end sees it at `ip` on a port of its
    /// own, a new address on the same connection, as a NAT rebinding or a roaming laptop shows
    /// it. Its answers still find the dialer.
    ///
    /// # Errors
    ///
    /// When no socket can be bound at `ip` (only `127.0.0.1` is loopback on macOS unless
    /// another is configured; Linux routes all of `127.0.0.0/8`).
    pub async fn move_to(&self, ip: IpAddr) -> io::Result<()> {
        drop(UdpSocket::bind(SocketAddr::new(ip, 0)).await?);
        self.link.from.send_replace(ip);
        Ok(())
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

/// One sender's way to the far end: the socket it goes out on, bound at the address the far
/// end sees it at, and the task bringing the answers back.
struct Way {
    /// The socket.
    up: Arc<UdpSocket>,
    /// Where it is bound.
    at: IpAddr,
    /// The task bringing the answers back.
    back: AbortHandle,
}

/// Forward what reaches `front` to `to`, each sender through a socket of its own whose replies
/// go back to it, until the relay is dropped; the senders' tasks end with this one. A sender
/// moved ([`Relay::move_to`]) gets a new socket at its new address, the old one let go.
async fn forward(front: Arc<UdpSocket>, to: SocketAddr, link: Arc<Link>) {
    let mut toward: HashMap<SocketAddr, Way> = HashMap::new();
    let mut backs = tokio::task::JoinSet::new();
    let mut buf = vec![0_u8; DATAGRAM];
    loop {
        let Ok((len, from)) = front.recv_from(&mut buf).await else { return };
        if !link.passes(&link.forth_cut) {
            continue;
        }
        let at = *link.from.borrow();
        let up = match toward.get(&from) {
            Some(way) if way.at == at => Arc::clone(&way.up),
            _ => {
                let Ok(up) = UdpSocket::bind(SocketAddr::new(at, 0)).await else { continue };
                if up.connect(to).await.is_err() {
                    continue;
                }
                let up = Arc::new(up);
                let back =
                    backs.spawn(back(Arc::clone(&up), Arc::clone(&front), from, Arc::clone(&link)));
                if let Some(old) = toward.insert(from, Way { up: Arc::clone(&up), at, back }) {
                    old.back.abort();
                }
                up
            }
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
        if !link.passes(&link.back_cut) {
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

    /// Cut one way, what the dialer sends arrives and the answer is dropped; restored, the
    /// answers come again.
    #[tokio::test]
    async fn a_link_cut_on_the_way_back_delivers_and_loses_the_answer() {
        let far = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay = Relay::start(far.local_addr().unwrap()).await.unwrap();
        let near = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        relay.cut_back();
        near.send_to(b"do it", relay.addr()).await.unwrap();
        let mut buf = [0_u8; 64];
        let (len, path) = far.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], b"do it", "the request arrives");
        far.send_to(b"done", path).await.unwrap();
        assert_eq!(heard(&near).await, None, "its answer is lost");
        assert_eq!(relay.dropped(), 1);

        relay.restore();
        far.send_to(b"done again", path).await.unwrap();
        assert_eq!(heard(&near).await.as_deref(), Some(&b"done again"[..]));
    }

    /// Moved, the dialer reaches the far end from a new address, and its answers still come
    /// back. Linux routes all of `127.0.0.0/8` to loopback; macOS only `127.0.0.1`, and the move
    /// says so.
    #[tokio::test]
    async fn a_moved_dialer_arrives_from_its_new_address_and_still_hears_back() {
        let far = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let port = far.local_addr().unwrap().port();
        let relay = Relay::start(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await.unwrap();
        let near = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        near.send_to(b"here", relay.addr()).await.unwrap();
        let mut buf = [0_u8; 64];
        let (_, before) = far.recv_from(&mut buf).await.unwrap();
        let elsewhere = IpAddr::from([127, 0, 0, 2]);
        let moved = relay.move_to(elsewhere).await;
        if cfg!(target_os = "linux") {
            moved.unwrap();
        } else if moved.is_err() {
            return;
        }
        near.send_to(b"there", relay.addr()).await.unwrap();
        let (len, after) = far.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], b"there");
        assert_eq!((before.ip(), after.ip()), (IpAddr::from(Ipv4Addr::LOCALHOST), elsewhere));
        far.send_to(b"heard", after).await.unwrap();
        assert_eq!(heard(&near).await.as_deref(), Some(&b"heard"[..]));
    }
}
