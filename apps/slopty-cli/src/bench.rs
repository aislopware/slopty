//! `slopty bench echo`: keystroke → first frame back on a `cat` session (transport + worker
//! engine + frame coalescing, no terminal emulation on this side).
//!
//! `slopty bench screen` ([`screen`]): open a screen stream and report what arrives.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use slopty_client::{LinkEvent, WorkerLink};
use slopty_proto::ClientMsg;
use slopty_proto::terminal::{OpenSession, TermEvent, TermRequest, TermSize};

use crate::client::{Session, connect_to};

#[cfg(target_vendor = "apple")]
pub mod screen;

/// Longest wait for the echo of one byte before the sample is dropped.
const ECHO_TIMEOUT: Duration = Duration::from_secs(2);
/// Quiet time after each sample so a late frame is not counted for the next byte.
const ECHO_SETTLE: Duration = Duration::from_millis(60);

/// Discard link events for `quiet`.
async fn drain(events: &mut tokio::sync::mpsc::Receiver<LinkEvent>, quiet: Duration) {
    let _elapsed: Result<(), tokio::time::error::Elapsed> =
        tokio::time::timeout(quiet, async { while events.recv().await.is_some() {} }).await;
}

/// Send `count` single bytes to a fresh `cat` session and time each one to the first frame
/// that comes back. Includes the worker's frame coalescing window and one round trip.
pub async fn echo(data_dir: &Path, needle: Option<&str>, count: u32) -> Result<()> {
    let mut session = connect_to(data_dir, needle).await?;
    let size = TermSize { cols: 60, rows: 12, ..TermSize::default() };
    let id = session
        .open(OpenSession {
            size,
            cwd: None,
            command: vec!["/bin/cat".to_owned()],
            env: Vec::new(),
            title: Some("bench echo".to_owned()),
            attach: true,
        })
        .await?;
    let Session { conn, endpoint, .. } = session;
    let mut link = WorkerLink::start(conn);
    let mut events = link.events().context("events")?;

    // Let the attach frames settle before timing anything.
    drain(&mut events, Duration::from_millis(500)).await;

    let mut samples: Vec<u64> = Vec::with_capacity(count as usize);
    let mut timeouts = 0_u32;
    for i in 0..count {
        let byte = if i % 2 == 0 { b"x".to_vec() } else { b"y".to_vec() };
        let sent = Instant::now();
        tracing::trace!(i, "bench send");
        link.send(ClientMsg::Term { session: id, req: TermRequest::Raw(byte) }).await?;
        let echoed = tokio::time::timeout(ECHO_TIMEOUT, async {
            loop {
                match events.recv().await {
                    Some(LinkEvent::Term { session, event: TermEvent::Frame(_) })
                        if session == id =>
                    {
                        break Ok(());
                    }
                    Some(LinkEvent::Disconnected(why)) => break Err(anyhow::anyhow!("{why}")),
                    Some(_other) => {}
                    None => break Err(anyhow::anyhow!("link closed")),
                }
            }
        })
        .await;
        match echoed {
            Ok(Ok(())) => {
                let us = u64::try_from(sent.elapsed().as_micros()).unwrap_or(u64::MAX);
                tracing::trace!(i, us, "bench frame");
                samples.push(us);
            }
            Ok(Err(e)) => bail!("echo bench: {e}"),
            Err(_timeout) => timeouts = timeouts.saturating_add(1),
        }
        drain(&mut events, ECHO_SETTLE).await;
    }
    let rtt = link.rtt().map_or_else(|| "?".to_owned(), |d| format!("{d:.1?}"));
    println!("keystroke → first frame ({count} bytes to /bin/cat):");
    println!("  {}", quantiles(&mut samples));
    println!("  timeouts {timeouts}  quic rtt {rtt}  path: {}", link.path());
    // Echoes shown from their datagram copy, and this side's sending: the keys' packets lost
    // on the way to the worker show here, and nowhere on the worker.
    println!("  echo copies taken {}  keys: {}", link.echo_copies_taken(), link.health());
    let _closed = link.send(ClientMsg::Term { session: id, req: TermRequest::Close }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    link.close();
    crate::client::close_endpoint(&endpoint).await;
    Ok(())
}

/// `min / p50 / p90 / max` in milliseconds.
fn quantiles(samples: &mut [u64]) -> String {
    samples.sort_unstable();
    let (Some(&min), Some(&max)) = (samples.first(), samples.last()) else {
        return "no samples".to_owned();
    };
    let last = samples.len().saturating_sub(1);
    let at = |q: f64| {
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "an index below 2^53"
        )]
        let i = (last as f64 * q).round() as usize;
        samples.get(i.min(last)).copied().unwrap_or(max)
    };
    #[expect(clippy::cast_precision_loss, reason = "microseconds well below 2^53")]
    let ms = |us: u64| us as f64 / 1e3;
    format!(
        "min {:.1} ms  p50 {:.1} ms  p90 {:.1} ms  max {:.1} ms  (n={})",
        ms(min),
        ms(at(0.5)),
        ms(at(0.9)),
        ms(max),
        samples.len()
    )
}
