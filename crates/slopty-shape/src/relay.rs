//! The relay loop: one UDP socket both ends speak through, with [`Shaper`] deciding each packet.
//!
//! One client per run, learned from the first packet that is not the worker's. That is all a
//! measurement needs, and it keeps the return path unambiguous: whatever the worker sends back
//! goes to the address the client was last seen at.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{Mutex, mpsc};

use crate::{Fate, Link, Shaper, Tally};

/// The largest datagram the relay carries. `recv_from` truncates silently into a short buffer,
/// and a truncated QUIC packet is a corrupt one, so this is the largest a datagram can be.
const MAX_DATAGRAM: usize = 65_536;

/// What each direction has carried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Carried {
    /// Client to worker.
    pub up: Tally,
    /// Worker to client.
    pub down: Tally,
}

/// A packet waiting for its turn on the wire.
#[derive(Debug)]
struct Outgoing {
    /// How far into the run it may leave.
    leaves: Duration,
    packet: Vec<u8>,
    to: SocketAddr,
}

/// A relay that is running. Dropping it stops nothing; the loop owns itself.
#[derive(Debug)]
pub struct Relay {
    socket: Arc<UdpSocket>,
    worker: SocketAddr,
    /// The run's epoch. Every `leaves` is measured from here, in both the shaper and the senders.
    started: Instant,
    up: Arc<Mutex<Shaper>>,
    down: Arc<Mutex<Shaper>>,
    /// Each direction has one sender, so packets leave in the order they were admitted.
    to_worker: mpsc::UnboundedSender<Outgoing>,
    to_client: mpsc::UnboundedSender<Outgoing>,
}

impl Relay {
    /// Bind a relay at `at`, forwarding to `worker` over `link`.
    ///
    /// `at` is normally the wildcard address: the client reaches the relay on loopback while the
    /// worker is on a real interface, and a socket bound to `127.0.0.1` cannot send there.
    ///
    /// # Errors
    ///
    /// If the address is taken.
    pub async fn bind(
        at: SocketAddr,
        worker: SocketAddr,
        link: Link,
        seed: u64,
    ) -> std::io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(at).await?);
        let started = Instant::now();
        let sender = || {
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(send_in_order(Arc::clone(&socket), started, rx));
            tx
        };
        let (to_worker, to_client) = (sender(), sender());
        Ok(Self {
            socket,
            worker,
            started,
            up: Arc::new(Mutex::new(Shaper::new(link, seed))),
            down: Arc::new(Mutex::new(Shaper::new(link, seed ^ 0xffff_ffff))),
            to_worker,
            to_client,
        })
    }

    /// Where the client should dial: loopback when the relay took the wildcard, since
    /// nothing can be dialed at `0.0.0.0`.
    ///
    /// # Errors
    ///
    /// If the socket has no address, which cannot happen once it is bound.
    pub fn addr(&self) -> std::io::Result<SocketAddr> {
        let bound = self.socket.local_addr()?;
        Ok(if bound.ip().is_unspecified() {
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bound.port()))
        } else {
            bound
        })
    }

    /// What each direction has carried so far.
    pub async fn carried(&self) -> Carried {
        Carried { up: self.up.lock().await.tally(), down: self.down.lock().await.tally() }
    }

    /// Carry packets until the socket fails. Never returns otherwise.
    ///
    /// # Errors
    ///
    /// If the socket cannot be read.
    pub async fn run(&self) -> std::io::Result<()> {
        let mut client: Option<SocketAddr> = None;
        let mut buf = vec![0_u8; MAX_DATAGRAM];
        loop {
            let (len, from) = self.socket.recv_from(&mut buf).await?;
            let Some(datagram) = buf.get(..len) else { continue };
            let (side, sender, to) = if from == self.worker {
                let Some(back) = client else {
                    tracing::warn!("the worker spoke before any client did; dropping");
                    continue;
                };
                (&self.down, &self.to_client, back)
            } else {
                if client != Some(from) {
                    tracing::info!(%from, "client");
                    client = Some(from);
                }
                (&self.up, &self.to_worker, self.worker)
            };
            let fate = side.lock().await.admit(len as u64, self.started.elapsed());
            let Fate::At(leaves) = fate else { continue };
            let outgoing = Outgoing { leaves, packet: datagram.to_vec(), to };
            if sender.send(outgoing).is_err() {
                tracing::warn!("the sender for this direction is gone");
            }
        }
    }
}

/// Send each packet once the run has reached its `leaves`, one direction's worth in the order
/// they were admitted. A single task is what keeps that order: a task per packet would let a
/// short wait overtake a long one, and QUIC counts an overtaken packet towards loss.
async fn send_in_order(
    socket: Arc<UdpSocket>,
    started: Instant,
    mut queue: mpsc::UnboundedReceiver<Outgoing>,
) {
    while let Some(Outgoing { leaves, packet, to }) = queue.recv().await {
        let wait = leaves.saturating_sub(started.elapsed());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        if let Err(error) = socket.send_to(&packet, to).await {
            tracing::warn!(%error, %to, "send");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a relay binds: any interface, any free port.
    fn wildcard() -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], 0))
    }

    /// A socket on the loopback address, standing in for one end of the link.
    async fn end() -> UdpSocket {
        UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap()
    }

    /// Read one datagram, or give up after `limit`.
    async fn next(socket: &UdpSocket, limit: Duration) -> Option<Vec<u8>> {
        let mut buf = vec![0_u8; MAX_DATAGRAM];
        let (len, _from) =
            tokio::time::timeout(limit, socket.recv_from(&mut buf)).await.ok()?.ok()?;
        buf.get(..len).map(<[u8]>::to_vec)
    }

    #[tokio::test]
    async fn a_packet_reaches_the_worker_and_the_answer_comes_back_to_the_client() {
        let worker = end().await;
        let client = end().await;
        let relay = Arc::new(
            Relay::bind(wildcard(), worker.local_addr().unwrap(), Link::CLEAR, 1).await.unwrap(),
        );
        let address = relay.addr().unwrap();
        tokio::spawn({
            let relay = Arc::clone(&relay);
            async move { relay.run().await }
        });

        client.send_to(b"hello", address).await.unwrap();
        assert_eq!(next(&worker, Duration::from_secs(2)).await.as_deref(), Some(&b"hello"[..]));
        // The worker answers the relay, which is the only address it has ever seen.
        worker.send_to(b"hi back", address).await.unwrap();
        assert_eq!(next(&client, Duration::from_secs(2)).await.as_deref(), Some(&b"hi back"[..]));

        let carried = relay.carried().await;
        assert_eq!((carried.up.sent, carried.down.sent), (1, 1));
        assert_eq!((carried.up.bytes, carried.down.bytes), (5, 7));
    }

    #[tokio::test]
    async fn a_delayed_link_holds_a_packet_for_its_delay() {
        let worker = end().await;
        let client = end().await;
        let link = Link { delay: Duration::from_millis(150), ..Link::CLEAR };
        let relay =
            Arc::new(Relay::bind(wildcard(), worker.local_addr().unwrap(), link, 1).await.unwrap());
        let address = relay.addr().unwrap();
        tokio::spawn({
            let relay = Arc::clone(&relay);
            async move { relay.run().await }
        });

        let sent = Instant::now();
        client.send_to(b"slow", address).await.unwrap();
        assert!(next(&worker, Duration::from_secs(2)).await.is_some(), "it still arrives");
        let took = sent.elapsed();
        assert!(took >= Duration::from_millis(150), "held for the delay, took {took:?}");
        assert!(took < Duration::from_millis(600), "and not much longer, took {took:?}");
    }

    // Multi-threaded on purpose: this is where a task per packet would actually reorder, because
    // tokio runs the most recently spawned task first when it can.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_jittered_link_still_delivers_in_the_order_it_was_given() {
        const RUN: u16 = 100;
        let worker = end().await;
        let client = end().await;
        // Jitter is the one fault that could reorder: each packet draws its own wait.
        let link = Link { jitter: Duration::from_millis(20), ..Link::CLEAR };
        let relay =
            Arc::new(Relay::bind(wildcard(), worker.local_addr().unwrap(), link, 5).await.unwrap());
        let address = relay.addr().unwrap();
        tokio::spawn({
            let relay = Arc::clone(&relay);
            async move { relay.run().await }
        });

        for seq in 0..RUN {
            client.send_to(&seq.to_be_bytes(), address).await.unwrap();
        }
        for seq in 0..RUN {
            let datagram = next(&worker, Duration::from_secs(2)).await.expect("every one arrives");
            let bytes: [u8; 2] = datagram.as_slice().try_into().unwrap();
            assert_eq!(u16::from_be_bytes(bytes), seq, "packet {seq} arrived out of turn");
        }
    }

    #[tokio::test]
    async fn a_link_that_loses_everything_delivers_nothing() {
        let worker = end().await;
        let client = end().await;
        let link = Link { loss: 1.0, ..Link::CLEAR };
        let relay =
            Arc::new(Relay::bind(wildcard(), worker.local_addr().unwrap(), link, 1).await.unwrap());
        let address = relay.addr().unwrap();
        tokio::spawn({
            let relay = Arc::clone(&relay);
            async move { relay.run().await }
        });

        for _packet in 0..5_u32 {
            client.send_to(b"gone", address).await.unwrap();
        }
        assert!(next(&worker, Duration::from_millis(300)).await.is_none());
        assert_eq!(relay.carried().await.up.lost, 5);
    }
}
