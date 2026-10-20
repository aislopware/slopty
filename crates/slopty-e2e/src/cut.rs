//! A UDP proxy in front of a worker whose path can be cut, as a laptop's lid, a dead NAT
//! binding or a Wi-Fi hop cuts it.
//!
//! Cut, it drops every datagram either way, so a link riding it hears nothing and says nothing
//! back, which is how a link dies under a sleeping Mac. It heals at the first QUIC Initial
//! the client sends: a fresh dial finds the path live, as a new connection over a new path
//! would. Nothing is shaped otherwise.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context as _, Result};
use tokio::net::UdpSocket;

/// The largest datagram carried: a truncated QUIC packet is a corrupt one.
const MAX_DATAGRAM: usize = 65_536;

/// The proxy. Dropping it stops it.
#[derive(Debug)]
pub struct Cut {
    addr: SocketAddr,
    state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Default)]
struct State {
    cut: AtomicBool,
    /// Datagrams dropped while cut.
    dropped: AtomicU64,
}

impl Cut {
    /// A proxy that clients reach at `front`, forwarding to `worker` on IPv4 loopback.
    ///
    /// # Errors
    ///
    /// When a socket cannot be bound.
    pub async fn bind(front: SocketAddr, worker: SocketAddr) -> Result<Self> {
        let front = Arc::new(UdpSocket::bind(front).await.context("bind the cut's front")?);
        let addr = front.local_addr()?;
        let state = Arc::new(State::default());
        let task = tokio::spawn(run(front, worker, Arc::clone(&state)));
        Ok(Self { addr, state, task })
    }

    /// Where a client dials it.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Cut the path: nothing goes either way until the client dials again.
    pub fn cut(&self) {
        self.state.cut.store(true, Ordering::SeqCst);
    }

    /// Whether the path is cut still.
    #[must_use]
    pub fn is_cut(&self) -> bool {
        self.state.cut.load(Ordering::SeqCst)
    }

    /// How many datagrams were dropped while cut.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.state.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Cut {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Whether `packet` is a QUIC Initial: the long-header bit set (RFC 9000 §17.2) and the type
/// bits 0 (§17.2.2).
fn is_initial(packet: &[u8]) -> bool {
    packet.first().is_some_and(|first| first & 0xb0 == 0x80)
}

/// Each client gets a back socket of its own, as a NAT gives each a port: a relaunched app is
/// a new peer to the worker, never the one it replaced.
async fn run(front: Arc<UdpSocket>, worker: SocketAddr, state: Arc<State>) {
    let mut backs: HashMap<SocketAddr, Arc<UdpSocket>> = HashMap::new();
    let mut downs = tokio::task::JoinSet::new();
    let mut up = vec![0_u8; MAX_DATAGRAM];
    loop {
        let Ok((n, from)) = front.recv_from(&mut up).await else { return };
        let packet = up.get(..n).unwrap_or_default();
        if state.cut.load(Ordering::SeqCst) {
            if !is_initial(packet) {
                state.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            state.cut.store(false, Ordering::SeqCst);
        }
        let back = if let Some(back) = backs.get(&from) {
            Arc::clone(back)
        } else {
            let Ok(back) = UdpSocket::bind(("127.0.0.1", 0)).await else { continue };
            if back.connect(worker).await.is_err() {
                continue;
            }
            let back = Arc::new(back);
            backs.insert(from, Arc::clone(&back));
            downs.spawn(down(Arc::clone(&front), Arc::clone(&back), from, Arc::clone(&state)));
            back
        };
        let _sent = back.send(packet).await;
    }
}

/// What the worker sends `client`'s back socket goes to `client`, unless the path is cut.
async fn down(front: Arc<UdpSocket>, back: Arc<UdpSocket>, client: SocketAddr, state: Arc<State>) {
    let mut down = vec![0_u8; MAX_DATAGRAM];
    while let Ok(n) = back.recv(&mut down).await {
        if state.cut.load(Ordering::SeqCst) {
            state.dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let _sent = front.send_to(down.get(..n).unwrap_or_default(), client).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Initial is a long header of type 0; a Handshake, a 1-RTT packet or nothing is not.
    #[test]
    fn an_initial_is_told_from_the_rest() {
        assert!(is_initial(&[0xc0, 0, 0, 0, 1]), "Initial");
        assert!(!is_initial(&[0xe0, 0, 0, 0, 1]), "Handshake");
        assert!(!is_initial(&[0x40, 1, 2]), "short header");
        assert!(!is_initial(&[]));
    }

    /// Cut, nothing crosses until the client sends an Initial, which heals it.
    #[tokio::test]
    async fn a_cut_path_heals_at_a_new_dial() {
        let worker = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let front = SocketAddr::from(([127, 0, 0, 1], 0));
        let cut = Cut::bind(front, worker.local_addr().unwrap()).await.unwrap();
        let client = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let mut buf = [0_u8; 16];
        client.send_to(&[0x40, 1], cut.addr()).await.unwrap();
        let (n, _) = worker.recv_from(&mut buf).await.unwrap();
        assert_eq!(buf.get(..n), Some(&[0x40, 1][..]), "carried while whole");
        cut.cut();
        client.send_to(&[0x40, 2], cut.addr()).await.unwrap();
        client.send_to(&[0xc0, 3], cut.addr()).await.unwrap();
        let (n, _) = worker.recv_from(&mut buf).await.unwrap();
        assert_eq!(
            buf.get(..n),
            Some(&[0xc0, 3][..]),
            "the 1-RTT packet was dropped, the Initial went"
        );
        assert!(!cut.is_cut(), "and healed the path");
        assert_eq!(cut.dropped(), 1);
    }
}
