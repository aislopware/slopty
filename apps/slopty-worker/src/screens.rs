//! One screen stream on a client's connection: the QUIC datagrams it sends into, and the task
//! that owns it.
//!
//! The task is the only thing that touches the [`ScreenStream`], so everything the stream does
//! slowly — the 115–300 ms open, an encoder rebuild, a close waiting on ScreenCaptureKit, the
//! geometry probe's window-server reads — waits only for that stream. The connection's own loop
//! hands commands over and goes straight back to the terminals.

use std::sync::Arc;

use bytes::Bytes;
use slopty_core::{ClientId, StreamId};
use slopty_net::{Connection, WorkerMsg};
use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenInput};
use slopty_worker::screen::{Refused, ScreenStream, StreamControl, StreamEvent};
use tokio::sync::mpsc;

use crate::Daemon;

/// How often a stream's target is checked for a size change.
/// Also how fast a window on the display-crop path follows a drag: the crop lags a move by
/// one period plus ScreenCaptureKit's ~20 ms configuration update (MEASUREMENTS.md, "capture
/// floor"), during which one edge of the picture shows the desktop the window left.
const GEOMETRY_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);

/// A connection's datagrams, written from whichever thread produced them.
pub struct QuicSink(pub Connection);

impl slopty_worker::DatagramSink for QuicSink {
    fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
        let sent = match datagrams {
            [one] => self.0.send_datagram(one.clone()),
            many => self.0.send_many_datagrams(many).map(|_queued| ()),
        };
        sent.map_err(|e| match e {
            noq::SendDatagramError::TooLarge => Refused::TooLarge,
            noq::SendDatagramError::UnsupportedByPeer
            | noq::SendDatagramError::Disabled
            | noq::SendDatagramError::ConnectionLost(_) => Refused::Closed,
        })
    }

    fn max_size(&self) -> Option<usize> {
        self.0.max_datagram_size()
    }

    fn held(&self) -> usize {
        slopty_net::endpoint::DATAGRAM_BUFFER.saturating_sub(self.0.datagram_send_buffer_space())
    }

    fn cwnd(&self) -> u64 {
        slopty_net::endpoint::path_rtt_cwnd(&self.0).map_or(0, |(_rtt, cwnd)| cwnd)
    }

    fn is_closed(&self) -> bool {
        self.0.close_reason().is_some()
    }
}

/// What the connection asks of one stream, in the order it asked.
#[derive(Debug)]
pub enum Command {
    Input(ScreenInput),
    Focus,
    SetQuality(Quality),
    Resize { width: u32, height: u32 },
    Close,
}

/// What a stream's task tells the connection back.
#[derive(Debug)]
pub enum Told {
    /// Open: feedback and reports go straight to it from now on.
    Opened(StreamId, StreamControl),
    /// Ended, however it ended; the connection forgets it.
    Gone(StreamId),
}

/// Everything a stream's task needs from its connection.
pub struct Link {
    pub daemon: Daemon,
    pub client: ClientId,
    pub conn: Connection,
    pub out: mpsc::Sender<WorkerMsg>,
    pub told: mpsc::UnboundedSender<Told>,
}

/// Open the stream, then serve its commands and its geometry until it is closed or the
/// connection lets go of it.
pub async fn run(
    link: Link,
    id: StreamId,
    target: CaptureTarget,
    quality: Quality,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let Link { daemon, client, conn, out, told } = link;
    let events = out.clone();
    let on_event = move |e: StreamEvent| {
        let event = match e {
            StreamEvent::Stopped(e) => ScreenEvent::Closed { stream: id, reason: e.to_string() },
            StreamEvent::Cursor(shape) => ScreenEvent::Cursor { stream: id, shape: Some(shape) },
        };
        let _sent = events.try_send(WorkerMsg::Screen(event));
    };
    let sink = Arc::new(QuicSink(conn.clone()));
    let mut stream = match ScreenStream::open(id, target, quality, sink, on_event).await {
        Ok((stream, opened)) => {
            tracing::info!(%client, %id, ?target, "screen opened");
            daemon.screens.insert(&client, target, stream.stats_handle());
            let _told = told.send(Told::Opened(id, stream.control()));
            let _sent = out.send(WorkerMsg::Screen(opened)).await;
            stream
        }
        Err(e) => {
            tracing::warn!(%client, ?target, error = %e, "screen open");
            let event = ScreenEvent::Closed { stream: id, reason: e.to_string() };
            let _sent = out.send(WorkerMsg::Screen(event)).await;
            let _told = told.send(Told::Gone(id));
            return;
        }
    };

    let mut geometry = tokio::time::interval(GEOMETRY_PERIOD);
    geometry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The probe runs beside the commands, not ahead of them: input for this window never waits
    // on the window server either.
    let mut probing: Option<tokio::task::JoinHandle<slopty_worker::screen::Probe>> = None;
    let by_client = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break false,
                Some(Command::Close) => break true,
                Some(command) => apply(&mut stream, client, command),
            },
            probe = async { probing.as_mut()?.await.ok() }, if probing.is_some() => {
                probing = None;
                let Some(probe) = probe else { continue };
                let mut events = Vec::new();
                match stream.check_geometry(&probe) {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(%client, stream = %id, error = %e, "geometry"),
                }
                events.extend(stream.check_source());
                for event in events {
                    let _sent = out.send(WorkerMsg::Screen(event)).await;
                }
            }
            _ = geometry.tick(), if probing.is_none() => {
                probing = Some(tokio::task::spawn_blocking(stream.prober()));
            }
        }
    };
    if let Some(probe) = probing {
        probe.abort();
    }
    if by_client {
        tracing::info!(
            %client,
            %id,
            path = %slopty_net::endpoint::describe_health(&conn),
            "screen closing"
        );
    }
    stream.close().await;
    daemon.screens.remove(&client, id);
    if by_client {
        let event = ScreenEvent::Closed { stream: id, reason: "closed by client".to_owned() };
        let _sent = out.send(WorkerMsg::Screen(event)).await;
    }
    let _told = told.send(Told::Gone(id));
}

fn apply(stream: &mut ScreenStream, client: ClientId, command: Command) {
    let id = stream.id();
    match command {
        Command::Input(input) => {
            if let Err(e) = stream.inject(&input) {
                tracing::debug!(%client, stream = %id, error = %e, "input");
            }
        }
        Command::Focus => {
            if let Err(e) = stream.focus() {
                tracing::debug!(%client, stream = %id, error = %e, "focus");
            }
        }
        Command::SetQuality(quality) => {
            if let Err(e) = stream.set_quality(&quality) {
                tracing::warn!(%client, stream = %id, error = %e, "set quality");
            }
        }
        Command::Resize { width, height } => {
            let Some((window, w, h)) = stream.resize_points(width, height) else {
                tracing::debug!(%client, stream = %id, "resize: not a window stream");
                return;
            };
            // Off the runtime: a few accessibility round trips.
            drop(tokio::task::spawn_blocking(move || {
                match slopty_worker::screen::resize_window(window, w, h) {
                    Ok(()) => tracing::debug!(%client, stream = %id, w, h, "window resized"),
                    Err(e) => tracing::debug!(%client, stream = %id, error = %e, "resize"),
                }
            }));
        }
        Command::Close => {}
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "measurement arithmetic on small counts and microseconds"
)]
mod tests {
    use std::time::Duration;

    use slopty_worker::DatagramSink as _;

    use super::QuicSink;

    /// Frames of datagrams handed to QUIC from a plain thread, as the encoder's callback does, one
    /// call per frame, to a loopback client: the one-way time each datagram took and what the
    /// process spent per frame. `datagram_send_cost` in MEASUREMENTS.md.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measurement"]
    async fn datagram_send_cost() {
        let (server, client, _endpoints) = pair().await;
        let sink = QuicSink(server);
        let epoch = std::time::Instant::now();
        let before = cpu_us();
        let producer = std::thread::spawn(move || {
            for n in 0..FRAMES {
                let started = std::time::Instant::now();
                let frame: Vec<bytes::Bytes> =
                    std::iter::repeat_with(|| stamped(epoch, n)).take(PER_FRAME).collect();
                sink.send(&frame).unwrap();
                #[expect(clippy::disallowed_methods, reason = "the frame clock of a test thread")]
                std::thread::sleep(FRAME.saturating_sub(started.elapsed()));
            }
        });
        let (latencies, whole) = receive(&client, epoch).await;
        let cpu = cpu_us().saturating_sub(before);
        producer.join().unwrap();
        report("direct send", &latencies, cpu);
        report("  whole frames", &whole, cpu);
    }

    /// The path this replaced, kept here only to measure against: the frame's datagrams into a
    /// 4096-slot queue one by one, and a task draining it into QUIC, reading the buffer and the
    /// datagram size beside every send and polling at 1 kHz while QUIC holds bytes.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measurement"]
    async fn datagram_send_cost_through_a_pump() {
        let (server, client, _endpoints) = pair().await;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(4096);
        let pump = tokio::spawn(async move {
            let mut holding = false;
            loop {
                let datagram = if holding {
                    match tokio::time::timeout(Duration::from_millis(1), rx.recv()).await {
                        Ok(Some(d)) => Some(d),
                        Ok(None) => break,
                        Err(_elapsed) => None,
                    }
                } else {
                    rx.recv().await
                };
                let held = slopty_net::endpoint::DATAGRAM_BUFFER
                    .saturating_sub(server.datagram_send_buffer_space());
                holding = held > 0;
                let Some(datagram) = datagram else { continue };
                if server.max_datagram_size().is_some_and(|max| datagram.len() > max) {
                    continue;
                }
                if server.send_datagram(datagram).is_err() {
                    break;
                }
            }
        });
        let epoch = std::time::Instant::now();
        let before = cpu_us();
        let producer = std::thread::spawn(move || {
            for n in 0..FRAMES {
                let started = std::time::Instant::now();
                for _ in 0..PER_FRAME {
                    let _full = tx.try_send(stamped(epoch, n));
                }
                #[expect(clippy::disallowed_methods, reason = "the frame clock of a test thread")]
                std::thread::sleep(FRAME.saturating_sub(started.elapsed()));
            }
        });
        let (latencies, whole) = receive(&client, epoch).await;
        let cpu = cpu_us().saturating_sub(before);
        producer.join().unwrap();
        pump.abort();
        report("queue + pump", &latencies, cpu);
        report("  whole frames", &whole, cpu);
    }

    const FRAMES: usize = 300;
    const PER_FRAME: usize = 64;
    const FRAME: Duration = Duration::from_micros(16_667);

    fn stamped(epoch: std::time::Instant, frame: usize) -> bytes::Bytes {
        let mut datagram = vec![0_u8; 1150];
        let us = u64::try_from(epoch.elapsed().as_micros()).unwrap();
        datagram[..8].copy_from_slice(&us.to_le_bytes());
        datagram[8..16].copy_from_slice(&(frame as u64).to_le_bytes());
        bytes::Bytes::from(datagram)
    }

    /// Each datagram's one-way time, and each frame's: from its first datagram's stamp to
    /// its last datagram's arrival, which is when a receiver could decode it.
    async fn receive(
        client: &slopty_net::Connection,
        epoch: std::time::Instant,
    ) -> (Vec<u64>, Vec<u64>) {
        let mut latencies = Vec::with_capacity(FRAMES * PER_FRAME);
        let mut frames: std::collections::HashMap<u64, (u64, u64, usize)> =
            std::collections::HashMap::new();
        let quiet = Duration::from_millis(500);
        while let Ok(Ok(datagram)) = tokio::time::timeout(quiet, client.read_datagram()).await {
            let sent = u64::from_le_bytes(datagram[..8].try_into().unwrap());
            let frame = u64::from_le_bytes(datagram[8..16].try_into().unwrap());
            let now = u64::try_from(epoch.elapsed().as_micros()).unwrap();
            latencies.push(now.saturating_sub(sent));
            let entry = frames.entry(frame).or_insert((sent, now, 0));
            *entry = (entry.0.min(sent), entry.1.max(now), entry.2.saturating_add(1));
        }
        let whole = frames
            .values()
            .filter(|(_, _, n)| *n == PER_FRAME)
            .map(|(first, last, _)| last.saturating_sub(*first))
            .collect();
        (latencies, whole)
    }

    fn report(label: &str, latencies: &[u64], cpu_us: u64) {
        let mut sorted = latencies.to_vec();
        sorted.sort_unstable();
        let at = |q: f64| {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "index")]
            let i = ((sorted.len().saturating_sub(1)) as f64 * q).round() as usize;
            sorted.get(i).copied().unwrap_or(0)
        };
        eprintln!(
            "{label}: {} of {} datagrams, one-way p50 {} / p99 {} / max {} µs, process cpu {} µs per frame",
            sorted.len(),
            FRAMES * PER_FRAME,
            at(0.5),
            at(0.99),
            sorted.last().copied().unwrap_or(0),
            cpu_us / FRAMES as u64,
        );
    }

    /// This process's CPU time so far, microseconds, from `ps` (`MM:SS.cc`).
    fn cpu_us() -> u64 {
        let out = std::process::Command::new("ps")
            .args(["-o", "time=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let mut parts = text.rsplit(':');
        let secs: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let mins: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a test's cpu time"
        )]
        let us = (mins.mul_add(60.0, secs) * 1e6) as u64;
        us
    }

    /// A loopback worker connection and the client end of it.
    async fn pair() -> (
        slopty_net::Connection,
        slopty_net::Connection,
        (slopty_net::worker::WorkerListener, slopty_net::Endpoint),
    ) {
        use slopty_proto::handshake::{Caps, ClientKind, Hello};
        let listener = slopty_net::worker::WorkerListener::bind(
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            slopty_net::admission::Admission::default(),
        )
        .unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let hello = Hello {
            protocol: slopty_proto::PROTOCOL_VERSION,
            client: slopty_core::ClientId::new(),
            kind: ClientKind::Tool,
            name: "bench".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
        };
        let dialing = endpoint.clone();
        let client =
            tokio::spawn(
                async move { slopty_net::client::connect_addr(&dialing, addr, hello).await },
            );
        let mut accepted = listener.accept().await.unwrap();
        let ack = slopty_proto::handshake::HelloAck {
            protocol: slopty_proto::PROTOCOL_VERSION,
            worker: slopty_core::WorkerId::new(),
            name: "bench".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
            sessions: Vec::new(),
        };
        accepted.tx.send(&slopty_net::WorkerMsg::HelloAck(ack)).await.unwrap();
        let client = client.await.unwrap().unwrap();
        (accepted.conn, client.conn, (listener, endpoint))
    }
}
